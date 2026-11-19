//! End-to-end result-semantics tests for the enhanced delete engine.
//!
//! These tests pin the *external* contract of
//! `EnhancedDeleteEngine::delete_files_with_progress` — the part the frontend
//! depends on — and are written to hold for both the serial implementation and
//! the rayon-parallel rewrite on `recycle-bin-parallel-delete`:
//!
//! 1. every submitted path produces exactly one `FileDeleteResult`;
//! 2. the outcome counts conserve: success + failed (+ reboot-pending) == N;
//! 3. one rejected target never takes the other entries down with it.
//!
//! Deliberately *not* asserted here (per the plan doc §7): result ordering and
//! progress-event semantics. Ordering is covered by the engine's own unit
//! tests, and progress assertions must wait until the parallel rewrite lands,
//! or they would misreport the current serial emitter as a regression.
//!
//! The round-trip test against the real recycle bin is `#[ignore]`d: it sends
//! a real entry through the shell into the current user's `$Recycle.Bin` and
//! deletes it for real. Run it explicitly with
//! `cargo test --test recycle_delete --release -- --ignored`
//! (needs a normal user session, no elevation; the entry lands in the current
//! user's own SID folder).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use cleanup_c_lib::test_api::{DeleteFailureReason, EnhancedDeleteEngine};

/// Scratch-directory counter so repeated runs (or same-named tests) never
/// share state, on top of the per-process id.
static SCRATCH_SEQ: AtomicUsize = AtomicUsize::new(0);

/// Create a unique scratch directory for one test and remember what to clean.
fn scratch_dir(tag: &str) -> PathBuf {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("lightc-it-{}-{}-{}", tag, std::process::id(), seq));
    let _ = fs::remove_dir_all(&dir); // stale leftovers from an aborted run
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// `tag` + per-file name for a payload inside a scratch dir.
fn scratch_file(dir: &Path, name: &str, payload: &[u8]) -> PathBuf {
    let file = dir.join(name);
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).expect("create nested scratch dir");
    }
    fs::write(&file, payload).expect("write scratch file");
    file
}

/// Path-compare form matching the engine's own normalization: backslash
/// separators, ASCII-lowercased.
fn compare_form(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase()
}

// ============================================================================
// Real recycle-bin plumbing (only used by the #[ignore] round-trip test)
// ============================================================================

/// Send one file to the recycle bin through the real shell chain, so it
/// becomes a genuine `$R`/`$I` pair in the current user's SID folder.
///
/// Uses `SHFileOperationW` directly instead of spawning PowerShell: a test
/// does not pay one process start per entry, and AGENTS.md already rules out
/// per-file subprocesses in batch scenarios.
fn send_to_recycle_bin(path: &Path) -> Result<(), String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE,
        SHFILEOPSTRUCTW,
    };

    // pFrom is a double-NUL-terminated wide string.
    let mut from: Vec<u16> = path.to_string_lossy().encode_utf16().collect();
    from.extend_from_slice(&[0, 0]);

    let mut op = SHFILEOPSTRUCTW {
        hwnd: HWND::default(),
        wFunc: FO_DELETE,
        pFrom: windows::core::PCWSTR::from_raw(from.as_ptr()),
        pTo: windows::core::PCWSTR::null(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI).0 as u16,
        fAnyOperationsAborted: windows::core::BOOL::default(),
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: windows::core::PCWSTR::null(),
    };

    // SAFETY: `op` owns valid pointers into `from`; the call does not retain them.
    let code = unsafe { SHFileOperationW(&mut op) };
    if code != 0 {
        return Err(format!("SHFileOperationW failed with code {code}"));
    }
    if op.fAnyOperationsAborted.as_bool() {
        return Err("SHFileOperationW reported the operation as aborted".to_string());
    }
    Ok(())
}

/// Recover the original path recorded in a `$I` metadata file.
///
/// $I format v2 (Windows Vista+): 8-byte version, 8-byte logical size,
/// 8-byte deletion time, then a 4-byte path length in UTF-16 units and the
/// NUL-terminated UTF-16 path itself. This mirrors the canonical parser in
/// `scanner/recycle_bin.rs::parse_metadata_bytes` (test plumbing is a separate
/// crate and cannot reach that private function); if the format ever drifts,
/// the round-trip test fails loudly rather than silently matching nothing.
fn recycle_metadata_original_path(metadata_bytes: &[u8]) -> Option<String> {
    if metadata_bytes.len() < 28 {
        return None;
    }
    let version = u64::from_le_bytes(metadata_bytes[0..8].try_into().ok()?);
    if version != 2 {
        return None;
    }
    let path_length = u32::from_le_bytes(metadata_bytes[24..28].try_into().ok()?) as usize;
    if path_length == 0 {
        return None;
    }
    let path_end = 28usize.checked_add(path_length.checked_mul(2)?)?;
    if path_end > metadata_bytes.len() {
        return None;
    }
    let units: Vec<u16> = metadata_bytes[28..path_end]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|character| *character != 0)
        .collect();
    Some(String::from_utf16_lossy(&units))
}

/// Find the `$R` data path of the recycle entry whose original path was
/// `sent_path`, by reading the `$I` metadata on the same volume.
///
/// SID folders that cannot be read (other accounts) are skipped — the entry we
/// just sent landed in the current user's own folder anyway.
fn find_recycle_data_path(sent_path: &Path) -> Option<PathBuf> {
    let sent_key = compare_form(sent_path);
    let volume_root = sent_path.ancestors().last()?;
    let recycle_bin = volume_root.join("$Recycle.Bin");

    for sid_dir in fs::read_dir(&recycle_bin).ok()?.flatten() {
        let Ok(entries) = fs::read_dir(sid_dir.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("$I") || name.len() <= 2 {
                continue;
            }
            let Ok(bytes) = fs::read(entry.path()) else {
                continue;
            };
            let Some(original) = recycle_metadata_original_path(&bytes) else {
                continue;
            };
            if compare_form(Path::new(&original)) == sent_key {
                return Some(sid_dir.path().join(format!("$R{}", &name[2..])));
            }
        }
    }
    None
}

// ============================================================================
// Implementation-independent result semantics
// ============================================================================

/// Contract 1: N_results == N_submitted.
///
/// Mixed payloads (plain files, a nested directory, an empty file) because the
/// parallel rewrite must account for every kind of submitted entry, and an
/// empty file exercises the zero-size bookkeeping.
#[test]
fn file_results_cover_exactly_the_submitted_paths() {
    let dir = scratch_dir("coverage");
    let mut submitted: Vec<String> = vec![];
    for name in ["alpha.txt", "bravo.bin", "empty.dat"] {
        let payload: &[u8] = if name == "empty.dat" {
            b""
        } else {
            name.as_bytes()
        };
        submitted.push(
            scratch_file(&dir, name, payload)
                .to_string_lossy()
                .into_owned(),
        );
    }
    // One directory entry with a nested file, so directory handling is counted too.
    scratch_file(&dir, r"nested\inner.txt", b"nested");
    submitted.push(dir.join("nested").to_string_lossy().into_owned());

    let result = EnhancedDeleteEngine::new().delete_files(&submitted);

    let mut submitted_sorted = submitted.clone();
    submitted_sorted.sort();
    let mut results_sorted: Vec<String> = result
        .file_results
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    results_sorted.sort();

    assert_eq!(
        results_sorted.len(),
        submitted.len(),
        "file_results must carry exactly one entry per submitted path"
    );
    assert_eq!(
        results_sorted, submitted_sorted,
        "the result set must be exactly the submitted path set"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// Contract 2: outcome counts conserve —
/// success + failed (+ reboot-pending) == file_results.len() == N.
///
/// Mixes one deterministically failing normal path (missing file) so both
/// outcome kinds are exercised in the same run; no reboot-pending entry is
/// constructible without a locked file, which would make the test flaky.
#[test]
fn outcome_counts_conserve_the_submitted_total() {
    let dir = scratch_dir("conservation");
    let mut submitted: Vec<String> = vec![];
    for name in ["one.txt", "two.txt"] {
        submitted.push(
            scratch_file(&dir, name, name.as_bytes())
                .to_string_lossy()
                .into_owned(),
        );
    }
    let missing = dir.join("missing.txt").to_string_lossy().into_owned();
    submitted.push(missing.clone());

    let result = EnhancedDeleteEngine::new().delete_files(&submitted);

    assert_eq!(result.file_results.len(), submitted.len());
    assert_eq!(
        result.success_count + result.failed_count + result.reboot_pending_count,
        result.file_results.len(),
        "every submitted path must land in exactly one outcome bucket"
    );

    // Cross-check the aggregate counters against the per-entry results.
    let succeeded = result
        .file_results
        .iter()
        .filter(|entry| entry.success)
        .count();
    let failed_plain = result
        .file_results
        .iter()
        .filter(|entry| !entry.success && !entry.marked_for_reboot)
        .count();
    let reboot_pending = result
        .file_results
        .iter()
        .filter(|entry| entry.marked_for_reboot)
        .count();
    assert_eq!(result.success_count, succeeded);
    assert_eq!(result.failed_count, failed_plain);
    assert_eq!(result.reboot_pending_count, reboot_pending);

    assert_eq!(result.success_count, 2, "the two real files must succeed");
    assert_eq!(result.failed_count, 1, "only the missing path may fail");
    let missing_entry = result
        .file_results
        .iter()
        .find(|entry| entry.path == missing)
        .expect("the missing path must still produce a result");
    assert!(
        matches!(
            missing_entry.failure_reason,
            Some(DeleteFailureReason::NotFound)
        ),
        "a missing normal path must report NotFound, got {:?}",
        missing_entry.failure_reason
    );

    let _ = fs::remove_dir_all(&dir);
}

/// Contract 3: failure isolation. One entry the engine must reject (a recycle
/// path under a foreign SID — the scanner can never legitimately surface one)
/// fails on its own; every normal temp entry still succeeds and produces its
/// own result.
#[test]
fn one_rejected_recycle_path_does_not_fail_the_remaining_entries() {
    let dir = scratch_dir("isolation");
    let mut submitted: Vec<String> = vec![];
    for name in ["keep-a.txt", "keep-b.txt", "keep-c.txt", "keep-d.txt"] {
        submitted.push(
            scratch_file(&dir, name, name.as_bytes())
                .to_string_lossy()
                .into_owned(),
        );
    }
    // S-1-5-18 is the well-known LocalSystem SID and can never be the current
    // user, so `is_current_user_entry_path` must reject this path for every
    // caller — a deterministic failure that does not touch the real bin.
    let rejected = r"C:\$Recycle.Bin\S-1-5-18\$Rlightc-it-rejected.dat".to_string();
    submitted.push(rejected.clone());

    let result = EnhancedDeleteEngine::new().delete_files(&submitted);

    assert_eq!(
        result.file_results.len(),
        submitted.len(),
        "the rejected path still counts as processed"
    );

    for entry in &result.file_results {
        if entry.path == rejected {
            assert!(!entry.success, "the foreign-SID entry must fail");
            assert!(
                matches!(entry.failure_reason, Some(DeleteFailureReason::Other(_))),
                "the rejection must carry a reason, got {:?}",
                entry.failure_reason
            );
        } else {
            assert!(
                entry.success,
                "entry {} must survive the rejected sibling, got {:?}",
                entry.path, entry.failure_reason
            );
        }
    }

    assert_eq!(result.success_count, 4);
    assert_eq!(result.failed_count, 1);

    let _ = fs::remove_dir_all(&dir);
}

/// End-to-end round trip through the real recycle bin: a temp file becomes a
/// genuine `$R`/`$I` pair via the shell, then the engine deletes the `$R` path
/// and the pair disappears completely.
///
/// Ignored because it mutates the real recycle bin of the machine it runs on,
/// and because it needs a *quiet* bin: while another process is filling the
/// bin (dataset builders) or emptying it (baseline wipes), a freshly sent
/// entry can be swept before this test finds it, which would report a false
/// failure. Run manually on an idle machine with
/// `cargo test --test recycle_delete -- --ignored`; needs a normal user
/// session (no elevation) on a volume with a `$Recycle.Bin`.
#[test]
#[ignore = "touches the real recycle bin: sends one real entry through the shell and deletes it for real; run explicitly with --ignored on an idle machine (no elevation, no concurrent bin writes/wipes)"]
fn recycle_bin_round_trip_deletes_both_pair_halves() {
    const PAYLOAD: &[u8] = b"lightc integration test payload";

    let dir = scratch_dir("roundtrip");
    let file = scratch_file(&dir, "roundtrip.txt", PAYLOAD);

    send_to_recycle_bin(&file).expect("file must land in the recycle bin");
    assert!(
        !file.exists(),
        "the shell must have moved the file out of its original location"
    );

    let data_path = find_recycle_data_path(&file)
        .expect("the just-sent entry must be findable via its $I metadata");

    let result =
        EnhancedDeleteEngine::new().delete_files(&[data_path.to_string_lossy().into_owned()]);

    assert_eq!(result.file_results.len(), 1);
    assert_eq!(result.success_count, 1, "the pair deletion must succeed");
    let entry = &result.file_results[0];
    assert!(entry.success, "got failure {:?}", entry.failure_reason);
    assert_eq!(
        entry.logical_size,
        PAYLOAD.len() as u64,
        "the engine must read the logical size from the $I metadata, not from the moved file"
    );

    assert!(
        !data_path.exists(),
        "the $R data blob must be physically gone"
    );
    let data_name = data_path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let metadata_path = data_path
        .parent()
        .unwrap()
        .join(format!("$I{}", &data_name[2..]));
    assert!(
        !metadata_path.exists(),
        "the $I metadata twin must be gone too, or Explorer keeps showing a phantom entry"
    );

    let _ = fs::remove_dir_all(&dir);
}
