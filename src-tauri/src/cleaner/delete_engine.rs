// ============================================================================
// Delete engine - core deletion logic implementation
// Multiple layers of safety protection to ensure important files are never deleted by mistake
// ============================================================================

use log::{debug, error, info, warn};
use std::fs;
use std::path::Path;

use super::delete_attempt::{
    attempt_delete, AttemptPolicy, AttributeClearance, DeleteOutcome, RebootPolicy,
};
use crate::error::{AppError, ErrorCode};
use crate::path_guard::{ensure_deletable, DeleteScope};
use crate::scanner::DeleteResult;

/// Junk clean escalates exactly two rungs: attributes (read-only leftovers from
/// installers, and now for directories too — it used to skip those) and the
/// reboot queue (files held open by a running app). It never takes ownership —
/// junk clean touches user data, and granting Administrators full control over a
/// file the user merely selected is not warranted.
///
/// The reboot gate is the entry scope: `ensure_deletable(JunkClean)` already
/// proved the path sits inside the junk allowlist, and junk clean only ever
/// handles rebuildable data.
const JUNK_CLEAN_POLICY: AttemptPolicy = AttemptPolicy {
    clear_attributes: AttributeClearance::OnPermissionDenied,
    take_ownership: false,
    ownership_for_caches: false,
    reboot: RebootPolicy::EntryScope,
};

/// Delete engine
///
/// Field-less. Its only knob was a "skip files in use" flag, which `new()` set
/// to true and nothing ever read — the shared ladder decides what happens to a
/// file that is in use (it queues it for reboot), so the flag never had a
/// decision to make. It went out together with its builder setter, which had no
/// callers either.
///
/// Left as a named type rather than folded into free functions because this is
/// the seam for the open S3c question: whether junk clean should keep its own
/// path loop or reuse the enhanced batch driver. A zero-sized struct makes that
/// decision cheap to defer and cheap to make — swapping the body does not touch
/// a single call site.
pub struct DeleteEngine;

impl DeleteEngine {
    /// Create a new delete engine
    pub fn new() -> Self {
        DeleteEngine
    }

    /// Delete a list of specified paths
    pub fn delete_paths(&self, paths: &[String]) -> DeleteResult {
        let mut result = DeleteResult::new();

        info!("开始删除 {} 个路径", paths.len());

        for path in paths {
            let file_path = Path::new(path);
            let size = self.get_path_size(file_path);

            match self.delete_single_file(path) {
                DeleteOutcome::Freed => {
                    result.add_success(size);
                    debug!("成功删除: {}", path);
                }
                DeleteOutcome::RebootPending => {
                    result.add_reboot_pending(size);
                    debug!("已标记重启删除: {}", path);
                }
                DeleteOutcome::Failed(e) => {
                    result.add_failure(path.clone(), e);
                    warn!("删除失败: {}", path);
                }
            }
        }

        info!(
            "删除完成: 成功 {} 个, 失败 {} 个, 待重启 {} 个, 释放空间 {} 字节",
            result.success_count,
            result.failed_count,
            result.reboot_pending_count,
            result.freed_size
        );

        result
    }

    /// Delete a single file or directory: gate first, then the shared ladder.
    fn delete_single_file(&self, path: &str) -> DeleteOutcome {
        let file_path = Path::new(path);

        // S3a: one decision point covers what used to be two separate layers here
        // (protected path + allowed scope). It also covers the `..` / `.` traversal
        // gate that C1 added, because `ensure_deletable` starts with it.
        //
        // The error keeps its code instead of being flattened to a sentence: the
        // per-file result carries `failure_code` to the UI, and the command layer
        // re-validates so a traversal attempt arrives as `invalid_path` rather than
        // an untyped `internal` message.
        if let Err(error) = ensure_deletable(file_path, DeleteScope::JunkClean) {
            error!("安全拦截: 拒绝删除 {} - {}", path, error);
            return DeleteOutcome::Failed(error);
        }

        // Check whether the path exists
        if !file_path.exists() {
            return DeleteOutcome::Failed(AppError::new(ErrorCode::NotFound).arg("path", path));
        }

        // S3b: the ladder used to live here, split into delete_file and
        // delete_directory, and it deliberately gave up on directories that were
        // in use. The shared ladder queues a directory bottom-up instead, which is
        // what makes "restart to finish cleaning" true for directories as well.
        attempt_delete(file_path, DeleteScope::JunkClean, &JUNK_CLEAN_POLICY)
    }

    /// Get the size of a path
    fn get_path_size(&self, path: &Path) -> u64 {
        if path.is_file() {
            fs::metadata(path).map(|m| m.len()).unwrap_or(0)
        } else if path.is_dir() {
            walkdir::WalkDir::new(path)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        } else {
            0
        }
    }
}

impl Default for DeleteEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path_guard::ensure_deletable;

    /// Junk clean must refuse the system table. The table itself lives in
    /// `path_guard` now (S3a) and is covered there in full; these cases guard the
    /// wiring: the engine really calls it with the junk-clean scope.
    fn refused(path: &str) -> bool {
        ensure_deletable(Path::new(path), DeleteScope::JunkClean).is_err()
    }

    #[test]
    fn protected_paths_are_refused() {
        assert!(refused(r"C:\Windows\System32\test.dll"));
        assert!(refused(r"C:\Program Files\App"));
        assert!(refused(r"C:\Windows.old"));
        assert!(refused(r"C:\Windows.old\Windows\System32\old.dll"));
        assert!(refused(r"C:\$Windows.~BT"));
    }

    #[test]
    fn rebuildable_system_caches_pass_through() {
        // Only rebuildable cache subdirectories explicitly listed by the Windows
        // cleanup wizard may pass through system-directory protection.
        assert!(!refused(r"C:\Windows\System32\d3d_cache\shader.bin"));
        assert!(!refused(
            r"C:\ProgramData\Microsoft\Windows Defender\Support\MPLog.log"
        ));
        assert!(refused(
            r"C:\ProgramData\Microsoft\Windows Defender\Quarantine\entry.bin"
        ));
        assert!(!refused(r"C:\Temp\test.tmp"));
    }

    /// C1 regression: the three bypass spellings from the audit must all be blocked.
    #[test]
    fn bypass_spellings_are_blocked() {
        // 1. Forward slashes: previously matched no `\`-based prefix at all
        assert!(refused("C:/Windows/System32/foo.dll"));
        assert!(refused("C:/Program Files/App"));
        assert!(refused("C:/"));

        // 2. Extended-length prefix: previously kept `\\?\` in the string, so every prefix missed
        assert!(refused(r"\\?\C:\Windows\System32\foo.dll"));
        assert!(refused(r"\\?\C:\"));

        // 3. Relative components: rejected outright before any check or deletion,
        //    because the prefix checks match the literal while the OS resolves `..`.
        assert!(crate::path_guard::validate_deletable(Path::new(
            r"C:\Temp\..\Windows\System32\config"
        ))
        .is_err());
    }

    /// C1 regression: the allowed-scope check is a hard error, not a warning.
    #[test]
    fn out_of_scope_paths_are_rejected() {
        // A user document outside every allowed scope must be rejected by
        // delete_single_file, not deleted with a warning like before the fix.
        assert!(refused(r"C:\Users\Test\Documents\report.docx"));

        // In-scope junk paths still pass, so normal cleanup is unaffected.
        assert!(!refused(r"C:\Users\Test\AppData\Local\Temp\x.tmp"));
        assert!(!refused("C:/Windows/Temp/install.log"));
    }
}
