// ============================================================================
// Deletion ladder - "the delete failed, now what?"
//
// S3b audit fix. The three delete engines each carried their own answer to this
// question, and the answers disagreed on three points that matter:
//
// | same OS failure            | delete_engine | enhanced_delete | permanent_delete |
// |----------------------------|---------------|-----------------|------------------|
// | ERROR_ACCESS_DENIED (5)    | permission    | Other("权限不足：…")  | permission  |
// | ERROR_SHARING_VIOLATION(32)| in use + queue| locked + queue  | queue           |
// | ERROR_LOCK_VIOLATION (33)  | internal      | locked + queue  | internal        |
// | directory in use           | give up       | queue the root  | queue bottom-up |
//
// So the same locked file produced `target_in_use` in one module, a Chinese
// sentence assembled in Rust in another, and `internal` in a third. None of the
// three was wrong on its own; nobody was comparing them. The ladder below is the
// single answer, and the differences that are *intentional* became policy fields
// (`AttemptPolicy`) instead of being spread over four functions per engine.
//
// What this module does NOT do:
//
// - It does not decide whether a path may be deleted. That is
//   `path_guard::ensure_deletable`, called before the ladder starts.
// - It does not measure anything. The three engines account sizes differently
//   (logical, cluster-aligned physical, file counts) and a shared walk here would
//   add a fourth traversal of the same tree, which is what M8 is about.
// - It does not run anything in parallel. `permanent_delete` still uses rayon
//   around this call; M7 changes that separately.
// ============================================================================

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use log::{debug, info, warn};
use walkdir::WalkDir;

use crate::error::{AppError, ErrorCode};
use crate::path_guard::{
    ensure_deletable, is_rebuildable_system_cache_path, is_safe_for_ownership, DeleteScope,
};

/// Use the built-in Administrators group SID to avoid icacls failing to resolve
/// localized group names on Chinese Windows.
const LOCAL_ADMINISTRATORS_SID: &str = "*S-1-5-32-544";

/// How far the ladder may escalate before reporting failure.
///
/// Every field defaults to "off" so a caller has to opt into escalation
/// explicitly; the three engines then differ only in what they opt into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttemptPolicy {
    /// Rung 2: clear read-only / hidden / system attributes, then retry.
    ///
    /// For a directory this walks the tree, because `remove_dir_all` fails on the
    /// first protected child and does not say which one it was — which is also
    /// why the trigger matters: on a sharing violation that walk would rewrite
    /// metadata across a tree that is not going to be deleted anyway.
    pub clear_attributes: AttributeClearance,
    /// Rung 3: take ownership from Administrators, then retry.
    ///
    /// Only ever applies inside `path_guard::is_safe_for_ownership` — this is the
    /// user-facing toggle, not the safety check.
    pub take_ownership: bool,
    /// Rung 3 exception: rebuildable system caches get the ownership step even
    /// when `take_ownership` is off, because the Windows cleanup wizard deletes
    /// them anyway and the user already asked for that category.
    pub ownership_for_caches: bool,
    /// Rung 4: queue for deletion on reboot when the OS says the target is in use.
    pub reboot: RebootPolicy,
}

impl AttemptPolicy {
    /// Direct delete only: no attribute clearing, no ownership, no reboot queue.
    ///
    /// Not used by any engine today — kept as the documented floor so the
    /// escalation each engine does opt into stays visible at its call site.
    #[allow(dead_code)]
    pub const MINIMAL: Self = Self {
        clear_attributes: AttributeClearance::Off,
        take_ownership: false,
        ownership_for_caches: false,
        reboot: RebootPolicy::Off,
    };
}

/// When rung 2 may run.
///
/// The three engines disagreed here too, and only one of the three answers was
/// written down: `enhanced_delete` always cleared attributes, `permanent_delete`
/// only on a permission error, `delete_engine` only on a permission error and
/// only for files (directories were skipped entirely).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributeClearance {
    /// Never.
    Off,
    /// Only when the first attempt was refused for permissions.
    ///
    /// A read-only or system bit is the usual cause, and a sharing violation is
    /// not something an attribute change can fix.
    OnPermissionDenied,
    /// Always. `enhanced_delete` does this because its tree is a cache directory
    /// that is going away either way, so the walk is never wasted.
    Always,
}

/// Whether — and behind which gate — the reboot queue may be used.
///
/// The three engines used three different gates here, and only one of them was
/// written down as a decision. Naming them makes the difference reviewable: if
/// `EnhancedClean` should stop requiring the whitelist, that is now one enum
/// value at one call site rather than a deleted condition inside a branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebootPolicy {
    /// Never queue. The engine reports "in use" and the user closes the app.
    Off,
    /// Queue only after re-asking the entry gate for this scope.
    ///
    /// The path already passed `ensure_deletable` before the ladder started;
    /// asking again is cheap (this rung is only reached on failure) and it makes
    /// the ladder safe by construction rather than by convention — a future
    /// caller that forgets the entry gate still cannot reach the reboot queue.
    EntryScope,
    /// Queue only inside the ownership whitelist.
    OwnershipWhitelist,
}

/// Result of one path going through the ladder.
///
/// No byte count: the three engines account sizes differently and already have
/// the number before they call in. Reporting it here would mean walking the tree
/// a second time.
#[derive(Debug)]
pub enum DeleteOutcome {
    /// Gone from the filesystem.
    Freed,
    /// Still present, but the OS will remove it during the next boot.
    RebootPending,
    /// Still present and nothing else will happen.
    Failed(AppError),
}

/// The three buckets every delete result reports (S3c).
///
/// Written once because the three engines each answered it themselves, and one of
/// them — `enhanced_delete` — derived it from `FileDeleteResult::failure_reason`,
/// a field whose only job is the tooltip. That is the mistake M16 removed from
/// the command layer: a display field deciding control flow. A new variant of
/// `DeleteFailureReason` added for wording would have silently changed the counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeKind {
    /// The bytes are free right now.
    Freed,
    /// The OS removes it at next boot, so nothing is free yet.
    RebootPending,
    /// Nothing happened; the path is still there.
    Failed,
}

impl DeleteOutcome {
    pub fn kind(&self) -> OutcomeKind {
        match self {
            Self::Freed => OutcomeKind::Freed,
            Self::RebootPending => OutcomeKind::RebootPending,
            Self::Failed(_) => OutcomeKind::Failed,
        }
    }
}

// ----------------------------------------------------------------------------
// Reporting contract
//
// All three engines report the same three buckets and agree on two rules that
// used to be written three times, in three shapes, in three files:
//
// 1. A reboot-pending item is NOT a success, and its bytes are NOT freed. It
//    must never reach the freed total — that is what produced "claimed 3 GB
//    freed, actually 1 GB" before `DeleteResult` started tracking
//    `reboot_pending_size` separately.
// 2. A path refused by the gate is a FAILURE, not a skip. The user selected it;
//    dropping it silently would make the cleanup look complete when it is not.
//
// The counters themselves stay in each result struct, because the three structs
// measure different things (logical vs cluster-aligned physical bytes, file
// counts, safety verdicts). Unifying the counters further is blocked on M7:
// `permanent_delete` runs under rayon and accumulates through atomics, so it
// cannot take `&mut` on a shared accumulator.
//
// An engine may report extra buckets beyond these three — `permanent_delete` has
// `manual_review_count` for "contains executables". That is a *pre-check*
// verdict, not a delete outcome, and it stays engine-specific on purpose.
// ----------------------------------------------------------------------------

/// Run one path through the ladder.
///
/// `scope` is only used by `RebootPolicy::EntryScope`; the caller must already
/// have passed `ensure_deletable(path, scope)` before calling.
pub fn attempt_delete(path: &Path, scope: DeleteScope, policy: &AttemptPolicy) -> DeleteOutcome {
    let mut trail: Vec<String> = Vec::new();

    // Rung 1: just try. This is the only rung that runs on the happy path.
    let first = match direct_delete(path) {
        Ok(()) => return DeleteOutcome::Freed,
        Err(error) => error,
    };
    trail.push(format!("direct delete: {first}"));

    // Rung 2: attributes.
    let attributes_allowed = match policy.clear_attributes {
        AttributeClearance::Off => false,
        AttributeClearance::OnPermissionDenied => first.kind() == io::ErrorKind::PermissionDenied,
        AttributeClearance::Always => true,
    };
    if attributes_allowed {
        match clear_attributes_then_retry(path) {
            Ok(()) => return DeleteOutcome::Freed,
            Err(message) => trail.push(message),
        }
    }

    // Rung 3: ownership. The whitelist is checked before the toggle is even
    // consulted, so `ownership_for_caches` can only widen the toggle — it can
    // never grant ownership outside the whitelist.
    let cache_exempt =
        policy.ownership_for_caches && is_rebuildable_system_cache_path(&path.to_string_lossy());
    if (policy.take_ownership || cache_exempt) && is_safe_for_ownership(path) {
        match take_ownership_then_retry(path) {
            Ok(()) => return DeleteOutcome::Freed,
            Err(message) => trail.push(message),
        }
    }

    // Rung 4: reboot queue. Only a sharing / lock violation qualifies; a
    // permission error is not something a reboot fixes, and queueing it would
    // tell the user "restart to finish" for a file that will still be there.
    if is_in_use(&first) && reboot_allowed(path, scope, policy.reboot) {
        match queue_for_reboot(path) {
            Ok(marked) => {
                info!("已标记重启删除 ({} 项): {}", marked, path.display());
                return DeleteOutcome::RebootPending;
            }
            Err(message) => trail.push(message),
        }
    }

    let error = AppError::new(classify(&first))
        .arg("path", path.to_string_lossy().as_ref())
        .detail(trail.join("; "));
    warn!("删除失败: {} - {}", path.display(), trail.join("; "));
    DeleteOutcome::Failed(error)
}

/// `remove_dir_all` for a directory, `remove_file` otherwise.
///
/// Deliberately not `symlink_metadata().is_file()`: an unreadable directory
/// answers `false` there and would be handed to `remove_file`, which reports a
/// confusing "invalid name" instead of the real access error.
fn direct_delete(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Rung 2: clear attributes across the tree, then retry once.
///
/// `WalkDir` on a single file yields just that file, so one implementation
/// covers both shapes. Entries that cannot even be stat'ed are skipped rather
/// than aborting: the retry below reports the real error.
fn clear_attributes_then_retry(path: &Path) -> Result<(), String> {
    for entry in WalkDir::new(path).into_iter().filter_map(Result::ok) {
        let entry_path = entry.path().to_string_lossy().to_string();
        #[cfg(windows)]
        let _ = super::enhanced_delete::windows_api::remove_protection_attributes(&entry_path);
        #[cfg(not(windows))]
        let _ = &entry_path;
    }

    direct_delete(path).map_err(|error| format!("retry after clearing attributes: {error}"))
}

/// Rung 3: grant Administrators ownership with icacls, then retry once.
///
/// No `/T` recursion — the caller runs this per path, and a recursive icacls on
/// a deep cache directory is far slower than the walk that follows it.
fn take_ownership_then_retry(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::process::Command;

        let path_str = path.to_string_lossy();

        let output = Command::new("icacls")
            .arg(&*path_str)
            .arg("/setowner")
            .arg(LOCAL_ADMINISTRATORS_SID)
            .arg("/C") // continue on errors
            .arg("/Q") // quiet mode
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .output()
            .map_err(|error| format!("icacls setowner could not start: {error}"))?;

        if !output.status.success() {
            return Err(format!(
                "icacls setowner failed (exit {:?})",
                output.status.code()
            ));
        }

        let output = Command::new("icacls")
            .arg(&*path_str)
            .arg("/grant")
            .arg(format!("{LOCAL_ADMINISTRATORS_SID}:F"))
            .arg("/C")
            .arg("/Q")
            .creation_flags(0x08000000)
            .output()
            .map_err(|error| format!("icacls grant could not start: {error}"))?;

        if !output.status.success() {
            return Err(format!(
                "icacls grant failed (exit {:?})",
                output.status.code()
            ));
        }
    }

    #[cfg(not(windows))]
    return Err("take ownership is Windows-only".to_string());

    #[allow(unreachable_code)]
    direct_delete(path).map_err(|error| format!("retry after taking ownership: {error}"))
}

/// Rung 4: hand the path to `MoveFileExW(MOVEFILE_DELAY_UNTIL_REBOOT)`.
///
/// `MOVEFILE_DELAY_UNTIL_REBOOT` refuses a non-empty directory, so a directory
/// must be queued bottom-up. `permanent_delete` already did this; the other two
/// engines queued the root and the call silently failed, which is why they never
/// reported a directory as pending-reboot.
///
/// Returns how many entries were queued; zero means the OS refused all of them.
fn queue_for_reboot(path: &Path) -> Result<usize, String> {
    #[cfg(not(windows))]
    {
        let _ = path;
        return Err("the reboot queue is Windows-only".to_string());
    }

    #[cfg(windows)]
    {
        // Deepest first; files before directories at the same depth, so a
        // directory is only queued once everything below it already is.
        let mut entries: Vec<(usize, PathBuf, bool)> = WalkDir::new(path)
            .into_iter()
            .filter_map(Result::ok)
            .map(|entry| {
                (
                    entry.depth(),
                    entry.path().to_path_buf(),
                    entry.file_type().is_dir(),
                )
            })
            .collect();
        entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.2.cmp(&a.2)));

        let mut marked = 0usize;
        for (_, entry_path, _is_dir) in &entries {
            let entry_str = entry_path.to_string_lossy().to_string();
            match super::enhanced_delete::windows_api::mark_for_delete_on_reboot(&entry_str) {
                Ok(()) => marked += 1,
                Err(error) => debug!("标记重启删除失败: {} - {}", entry_str, error),
            }
        }

        if marked == 0 {
            Err("the OS refused every reboot-queue entry".to_string())
        } else {
            Ok(marked)
        }
    }
}

fn reboot_allowed(path: &Path, scope: DeleteScope, policy: RebootPolicy) -> bool {
    match policy {
        RebootPolicy::Off => false,
        RebootPolicy::EntryScope => ensure_deletable(path, scope).is_ok(),
        RebootPolicy::OwnershipWhitelist => is_safe_for_ownership(path),
    }
}

/// `ERROR_SHARING_VIOLATION` / `ERROR_LOCK_VIOLATION`.
///
/// Both are "someone else has this open", which a reboot resolves. Note that
/// `delete_engine` only ever matched 32, so a file held with a byte-range lock
/// (33) was reported as `internal` there and as "locked" in `enhanced_delete`.
#[cfg(windows)]
fn is_in_use(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(32 | 33))
}

#[cfg(not(windows))]
fn is_in_use(_error: &io::Error) -> bool {
    false
}

/// One mapping from an OS error to a wire code, for all three engines.
///
/// The numeric branch is Win32 (`2`/`3` not found, `5` access denied, `32`/`33`
/// in use); `ErrorKind` covers the same cases portably and is checked first, so
/// only the codes `std` does not classify reach the numbers.
fn classify(error: &io::Error) -> ErrorCode {
    match error.kind() {
        io::ErrorKind::NotFound => return ErrorCode::NotFound,
        io::ErrorKind::PermissionDenied => return ErrorCode::PermissionDenied,
        _ => {}
    }

    match error.raw_os_error() {
        Some(2 | 3) => ErrorCode::NotFound,
        Some(5) => ErrorCode::PermissionDenied,
        Some(32 | 33) => ErrorCode::TargetInUse,
        _ => ErrorCode::Internal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(
        clear_attributes: AttributeClearance,
        take_ownership: bool,
        reboot: RebootPolicy,
    ) -> AttemptPolicy {
        AttemptPolicy {
            clear_attributes,
            take_ownership,
            ownership_for_caches: false,
            reboot,
        }
    }

    /// A fresh directory under the system temp dir, unique per test.
    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lightc-s3b-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    /// A handle that denies sharing, so the OS reports a sharing violation and
    /// the ladder has to decide what to do about it.
    ///
    /// `File::open` is not enough: Rust asks for `FILE_SHARE_READ |
    /// FILE_SHARE_WRITE | FILE_SHARE_DELETE`, so the file stays deletable. That
    /// mistake made the first version of these tests assert on `Freed` while
    /// claiming to test the locked path.
    #[cfg(windows)]
    fn hold_exclusively(path: &Path) -> fs::File {
        use std::os::windows::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(path)
            .expect("open exclusively")
    }

    #[test]
    fn a_plain_file_is_freed_by_the_first_rung() {
        let dir = scratch_dir("plain-file");
        let file = dir.join("payload.bin");
        fs::write(&file, b"x").expect("write file");

        let outcome = attempt_delete(
            &file,
            DeleteScope::UserSelected,
            &policy(AttributeClearance::Always, false, RebootPolicy::Off),
        );
        assert!(matches!(outcome, DeleteOutcome::Freed));
        assert!(!file.exists());
    }

    #[test]
    fn a_directory_is_freed_by_the_first_rung() {
        let dir = scratch_dir("plain-dir");
        let nested = dir.join("a").join("b");
        fs::create_dir_all(&nested).expect("create nested");
        fs::write(nested.join("payload.bin"), b"x").expect("write file");

        let outcome = attempt_delete(
            &dir,
            DeleteScope::UserSelected,
            &policy(AttributeClearance::Always, false, RebootPolicy::Off),
        );
        assert!(matches!(outcome, DeleteOutcome::Freed));
        assert!(!dir.exists());
    }

    #[test]
    fn a_missing_target_fails_with_not_found() {
        let dir = scratch_dir("missing");
        let missing = dir.join("not-there.bin");

        match attempt_delete(
            &missing,
            DeleteScope::UserSelected,
            &policy(AttributeClearance::Off, false, RebootPolicy::Off),
        ) {
            DeleteOutcome::Failed(error) => assert_eq!(error.code, ErrorCode::NotFound),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// The trail is the only place that says which rungs ran, so the policy
    /// fields are asserted through it rather than through the outcome — Rust's
    /// `remove_dir_all` clears the read-only bit itself, so "it succeeded" would
    /// pass even with rung 2 switched off.
    #[cfg(windows)]
    #[test]
    fn the_attribute_rung_runs_when_the_policy_allows_it() {
        let dir = scratch_dir("trail-on");
        let file = dir.join("held.bin");
        fs::write(&file, b"x").expect("write file");
        let _held = hold_exclusively(&file);

        match attempt_delete(
            &file,
            DeleteScope::UserSelected,
            &policy(AttributeClearance::Always, false, RebootPolicy::Off),
        ) {
            DeleteOutcome::Failed(error) => {
                let detail = error.detail.unwrap_or_default();
                assert!(detail.contains("direct delete"), "detail was: {detail}");
                assert!(
                    detail.contains("retry after clearing attributes"),
                    "rung 2 should have run: {detail}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn the_attribute_rung_stays_out_when_the_policy_says_off() {
        let dir = scratch_dir("trail-off");
        let file = dir.join("held.bin");
        fs::write(&file, b"x").expect("write file");
        let _held = hold_exclusively(&file);

        match attempt_delete(
            &file,
            DeleteScope::UserSelected,
            &policy(AttributeClearance::Off, false, RebootPolicy::Off),
        ) {
            DeleteOutcome::Failed(error) => {
                let detail = error.detail.unwrap_or_default();
                assert!(detail.contains("direct delete"), "detail was: {detail}");
                assert!(
                    !detail.contains("retry after clearing attributes"),
                    "rung 2 must not run when the policy is Off: {detail}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// `OnPermissionDenied` must not fire for a sharing violation: clearing
    /// attributes across a locked tree rewrites metadata on files that stay put.
    #[cfg(windows)]
    #[test]
    fn the_attribute_rung_skips_a_sharing_violation() {
        let dir = scratch_dir("trail-perm-only");
        let file = dir.join("held.bin");
        fs::write(&file, b"x").expect("write file");
        let _held = hold_exclusively(&file);

        match attempt_delete(
            &file,
            DeleteScope::UserSelected,
            &policy(
                AttributeClearance::OnPermissionDenied,
                false,
                RebootPolicy::Off,
            ),
        ) {
            DeleteOutcome::Failed(error) => {
                let detail = error.detail.unwrap_or_default();
                assert!(
                    !detail.contains("retry after clearing attributes"),
                    "a sharing violation is not an attribute problem: {detail}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// `RebootPolicy::Off` must never touch the reboot queue, even for a locked
    /// file — otherwise "off" would silently mean "on".
    ///
    /// The test never enables the queue: marking a temp file for deletion on
    /// reboot is a real side effect when the suite runs elevated.
    #[cfg(windows)]
    #[test]
    fn a_locked_file_fails_when_the_reboot_rung_is_off() {
        let dir = scratch_dir("reboot-off");
        let file = dir.join("held.bin");
        fs::write(&file, b"x").expect("write file");
        let _held = hold_exclusively(&file);

        let outcome = attempt_delete(
            &file,
            DeleteScope::UserSelected,
            &policy(AttributeClearance::Off, false, RebootPolicy::Off),
        );
        match outcome {
            DeleteOutcome::Failed(error) => assert_eq!(error.code, ErrorCode::TargetInUse),
            other => panic!("a locked file with the queue off must fail: {other:?}"),
        }
        assert!(file.exists());
    }

    #[test]
    fn only_sharing_and_lock_violations_count_as_in_use() {
        let sharing = io::Error::from_raw_os_error(32);
        let lock = io::Error::from_raw_os_error(33);
        let denied = io::Error::from_raw_os_error(5);

        if cfg!(windows) {
            assert!(is_in_use(&sharing));
            assert!(is_in_use(&lock));
        }
        // A permission error is not something a reboot fixes.
        assert!(!is_in_use(&denied));
    }

    /// The three buckets, and the rule that reboot-pending is not a success.
    #[test]
    fn outcomes_map_to_their_reporting_bucket() {
        assert_eq!(DeleteOutcome::Freed.kind(), OutcomeKind::Freed);
        assert_eq!(
            DeleteOutcome::RebootPending.kind(),
            OutcomeKind::RebootPending
        );
        assert_eq!(
            DeleteOutcome::Failed(AppError::new(ErrorCode::Internal)).kind(),
            OutcomeKind::Failed
        );
    }

    /// The mapping the three engines disagreed on, asserted in one place.
    #[test]
    fn os_errors_map_to_the_same_codes_for_every_engine() {
        assert_eq!(
            classify(&io::Error::from_raw_os_error(2)),
            ErrorCode::NotFound
        );
        assert_eq!(
            classify(&io::Error::from_raw_os_error(3)),
            ErrorCode::NotFound
        );
        assert_eq!(
            classify(&io::Error::from_raw_os_error(5)),
            ErrorCode::PermissionDenied
        );
        assert_eq!(
            classify(&io::Error::from_raw_os_error(32)),
            ErrorCode::TargetInUse
        );
        // 33 was `internal` in two of the three engines before S3b.
        assert_eq!(
            classify(&io::Error::from_raw_os_error(33)),
            ErrorCode::TargetInUse
        );
        // Anything unclassified stays `internal` rather than pretending to be
        // something the UI has a specific message for.
        assert_eq!(
            classify(&io::Error::from_raw_os_error(1234)),
            ErrorCode::Internal
        );
    }

    /// `EntryScope` re-asks the gate; a protected path must not reach the queue
    /// even if a caller forgot to check first.
    #[test]
    fn the_entry_scope_gate_refuses_a_protected_path() {
        assert!(!reboot_allowed(
            Path::new(r"C:/Windows/System32/config/SYSTEM"),
            DeleteScope::JunkClean,
            RebootPolicy::EntryScope,
        ));
        assert!(reboot_allowed(
            Path::new(r"C:/Users/Test/AppData/Local/Temp/x.tmp"),
            DeleteScope::JunkClean,
            RebootPolicy::EntryScope,
        ));
    }

    #[test]
    fn the_ownership_gate_uses_the_whitelist_not_the_scope() {
        assert!(reboot_allowed(
            Path::new(r"C:/Windows/Temp/setup.log"),
            DeleteScope::EnhancedClean,
            RebootPolicy::OwnershipWhitelist,
        ));
        // Deletable, but not a place we may escalate — so not a place we may
        // queue either, under this policy.
        assert!(!reboot_allowed(
            Path::new(r"C:/Program Files/SomeApp/app.dll"),
            DeleteScope::EnhancedClean,
            RebootPolicy::OwnershipWhitelist,
        ));
    }

    #[test]
    fn the_reboot_gate_is_off_when_the_policy_is_off() {
        assert!(!reboot_allowed(
            Path::new(r"C:/Windows/Temp/setup.log"),
            DeleteScope::EnhancedClean,
            RebootPolicy::Off,
        ));
    }
}
