//! End-to-end result-semantics tests for the ordinary-file batch of the enhanced
//! delete engine.
//!
//! The recycle-bin batch already had result-semantics tests (`recycle_delete.rs`)
//! because it is parallel and the aggregation moved to the main thread. The
//! ordinary-file batch had the same rewrite (普通文件删除并行化), so it gets the same
//! pinned contract:
//!
//! 1. every submitted path produces exactly one `FileDeleteResult`, in request order;
//! 2. success + failed + reboot-pending == N (the conservation law the front end and
//!    the summary message both rely on);
//! 3. one refused or missing entry never takes the other entries down with it;
//! 4. progress is monotonic and reaches the submitted total even when entries fail.
//!
//! Deliberately not asserted: how many workers ran (that is an environment variable
//! and a timing property, covered by the A/B driver), and wall-clock numbers.
//!
//! These tests delete for real inside `%TEMP%`. They are ordinary tests (not
//! `#[ignore]`d) because everything they touch is a scratch directory they created.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use cleanup_c_lib::test_api::{DeleteFailureReason, EnhancedDeleteEngine};

static SCRATCH_SEQ: AtomicUsize = AtomicUsize::new(0);

/// Unique scratch directory per test, so a repeated run or two tests in parallel
/// never share state.
fn scratch_dir(tag: &str) -> PathBuf {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("lightc-nd-{}-{}-{}", tag, std::process::id(), seq));
    let _ = fs::remove_dir_all(&dir); // stale leftovers from an aborted run
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn scratch_file(dir: &Path, name: &str, payload: &[u8]) -> PathBuf {
    let file = dir.join(name);
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).expect("create nested scratch dir");
    }
    fs::write(&file, payload).expect("write scratch file");
    file
}

/// A small tree, submitted as *one* entry: the engine deletes it with
/// `remove_dir_all` and must still report it as a single result.
fn scratch_tree(dir: &Path, name: &str, depth: usize) -> PathBuf {
    let root = dir.join(name);
    fs::create_dir_all(&root).expect("create tree root");
    fs::write(root.join("leaf.bin"), b"leaf").expect("write leaf");
    if depth > 1 {
        for index in 0..2 {
            scratch_tree(&root, &format!("sub-{index}"), depth - 1);
        }
    }
    root
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

/// Build `count` files spread over subdirectories, interleaved with directory trees,
/// so the batch sees the mixed payload the deep scanner produces.
fn build_mixed_batch(dir: &Path, files: usize, trees: usize) -> Vec<String> {
    let mut entries = Vec::new();
    for index in 0..files {
        let file = scratch_file(
            dir,
            &format!("sub-{}/payload-{index}.tmp", index % 4),
            b"0123456789abcdef",
        );
        entries.push(path_string(&file));
        // Interleave trees instead of appending them, so the request order is not
        // "all files then all directories" — a wrong aggregation that reordered by
        // kind would still be caught.
        if index < trees && index % 3 == 0 {
            let tree = scratch_tree(dir, &format!("tree-{index}"), 2);
            entries.push(path_string(&tree));
        }
    }
    entries
}

#[test]
fn normal_batch_conserves_counts_and_keeps_request_order() {
    let dir = scratch_dir("order");
    let entries = build_mixed_batch(&dir, 120, 12);
    let engine = EnhancedDeleteEngine::new();

    let result = engine.delete_files(&entries);

    assert_eq!(
        result.file_results.len(),
        entries.len(),
        "N_results != N_submitted"
    );
    assert_eq!(
        result.success_count + result.failed_count + result.reboot_pending_count,
        entries.len(),
        "outcomes do not conserve the submitted total"
    );
    for (index, (reported, submitted)) in result.file_results.iter().zip(&entries).enumerate() {
        assert_eq!(
            &reported.path, submitted,
            "result {index} is out of request order"
        );
    }

    // Everything that succeeded must be gone from disk, and the batch must not have
    // lost entries on the way: this batch was created to be deleted.
    for entry in &entries {
        assert!(!Path::new(entry).exists(), "still on disk: {entry}");
    }
    assert_eq!(
        result.failed_count, 0,
        "unexpected failures in a clean batch"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn normal_batch_isolates_refused_and_missing_entries() {
    let dir = scratch_dir("isolate");
    let mut entries = Vec::new();
    for index in 0..8 {
        entries.push(path_string(&scratch_file(
            &dir,
            &format!("good-{index}.tmp"),
            b"payload",
        )));
    }

    // A path that vanished between scan and delete: reported as NotFound, and it must
    // not stop the entries behind it.
    let missing = dir.join("never-existed.tmp");
    entries.insert(3, path_string(&missing));

    // A traversal spelling: refused by the shared path gate before any filesystem
    // work. The target it would have reached is a scratch file of this test, so a
    // gate regression deletes test data rather than anything real.
    let traversal = dir.join("sub").join("..").join("good-0.tmp");
    entries.insert(6, path_string(&traversal));

    let engine = EnhancedDeleteEngine::new();
    let result = engine.delete_files(&entries);

    assert_eq!(
        result.file_results.len(),
        entries.len(),
        "N_results != N_submitted"
    );
    assert_eq!(
        result.success_count + result.failed_count + result.reboot_pending_count,
        entries.len(),
        "outcomes do not conserve the submitted total"
    );
    assert_eq!(result.success_count, 8, "the good entries must all succeed");
    assert_eq!(
        result.failed_count, 2,
        "missing + traversal are two failures"
    );

    let missing_result = result
        .file_results
        .iter()
        .find(|entry| entry.path.ends_with("never-existed.tmp"))
        .expect("the missing entry has a result");
    assert_eq!(
        missing_result.failure_reason,
        Some(DeleteFailureReason::NotFound)
    );

    let traversal_result = result
        .file_results
        .iter()
        .find(|entry| entry.path.contains(".."))
        .expect("the traversal entry has a result");
    assert_eq!(
        traversal_result.failure_reason,
        Some(DeleteFailureReason::OutOfScope),
        "a relative component must be refused by the gate, not attempted"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn normal_batch_progress_is_monotonic_and_reaches_total() {
    let dir = scratch_dir("progress");
    let mut entries = build_mixed_batch(&dir, 300, 0);
    // One path that cannot be deleted, so the "reaches 100% despite failures"
    // requirement is exercised rather than assumed.
    entries.push(path_string(&dir.join("ghost.tmp")));
    let total = entries.len();

    let seen: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
    let engine = EnhancedDeleteEngine::new();
    let result = engine.delete_files_with_progress(&entries, |progress| {
        seen.lock()
            .expect("progress log")
            .push((progress.processed_count, progress.total_count));
    });

    let events = seen.into_inner().expect("progress log");
    assert!(!events.is_empty(), "no progress events at all");
    for window in events.windows(2) {
        assert!(
            window[1].0 >= window[0].0,
            "progress went backwards: {:?} -> {:?}",
            window[0],
            window[1]
        );
    }
    let (last_processed, last_total) = *events.last().expect("last progress event");
    assert_eq!(last_total, total);
    assert_eq!(
        last_processed, total,
        "progress must reach the submitted total"
    );
    assert_eq!(
        result.failed_count, 1,
        "the ghost entry is the only failure"
    );

    let _ = fs::remove_dir_all(&dir);
}
