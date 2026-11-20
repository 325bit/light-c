//! Manual A/B driver for the ordinary-file batch (普通文件删除并行化 §6).
//!
//! Not a correctness test: it deletes whatever list it is pointed at, for real,
//! and prints the wall clock and throughput so one round can be compared against
//! another on a freshly rebuilt dataset.
//!
//! Usage (idle machine, no elevation):
//!
//! ```text
//! LIGHTC_AB_ROOT=D:\lightc-bench LIGHTC_AB_LIST=<paths.txt> \
//!   cargo test --release --test normal_delete_bench -- --ignored --nocapture
//! ```
//!
//! The engine reads its worker count from `LIGHTC_NORMAL_DELETE_WORKERS` once the
//! batch is parallel, so the concurrency matrix reuses this same command with
//! that variable set.
//!
//! Why a separate driver instead of extending `recycle_bin_bench`: that one hard-
//! rejects anything outside `$Recycle.Bin`, which is what keeps a typo in a list
//! path away from real user files. Relaxing it to serve both batches would remove
//! the only guard on the destructive driver. This one carries its own, narrower
//! guard: every path must live under `LIGHTC_AB_ROOT`, the throwaway benchmark
//! tree.

use std::path::{Path, PathBuf};
use std::time::Instant;

use cleanup_c_lib::test_api::EnhancedDeleteEngine;

/// Normalize a path for the containment check: lowercase and drop trailing
/// separators, so `D:\lightc-bench\` and `d:/lightc-bench` compare equal.
///
/// Deliberately no `canonicalize`: the paths are about to be deleted, and asking
/// NTFS for their real names would add a stat per entry to every measured round.
fn compare_key(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\");
    text.trim_end_matches('\\').to_ascii_lowercase()
}

/// Refuse any list that reaches outside the benchmark root. This driver deletes
/// for real; a wrong list path must fail before the first `remove_file`.
fn under_bench_root(paths: &[String], root: &str) -> Result<(), String> {
    let root = compare_key(Path::new(root));
    if root.len() < 4 || !root.contains(':') {
        // A root like `D:\` would pass containment for the whole volume, which is
        // exactly the accident this guard exists to prevent.
        return Err(format!(
            "LIGHTC_AB_ROOT is too broad to be a bench root: {root}"
        ));
    }
    for path in paths {
        let key = compare_key(Path::new(path));
        // Component-aware prefix: `\lightc-bench2\x` must not match `\lightc-bench`.
        let inside = key == root || key.starts_with(&format!("{root}\\"));
        if !inside {
            return Err(format!("path outside LIGHTC_AB_ROOT ({root}): {path}"));
        }
    }
    Ok(())
}

fn read_list(list_path: &str) -> Vec<String> {
    let list = std::fs::read_to_string(list_path)
        .unwrap_or_else(|error| panic!("read the path list {list_path}: {error}"));
    let paths: Vec<String> = list
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    assert!(!paths.is_empty(), "the path list is empty: {list_path}");
    paths
}

#[test]
#[ignore = "deletes a real dataset listed in LIGHTC_AB_LIST; measurement driver, not a correctness test"]
fn delete_normal_batch_from_list_file() {
    let list_path =
        std::env::var("LIGHTC_AB_LIST").expect("LIGHTC_AB_LIST must point at a path list");
    let root = std::env::var("LIGHTC_AB_ROOT").expect("LIGHTC_AB_ROOT must name the bench tree");
    let paths = read_list(&list_path);
    if let Err(reason) = under_bench_root(&paths, &root) {
        panic!("refusing to delete: {reason}");
    }

    // `env_logger` is a normal dependency of the crate, so the engine's own phase
    // and throughput lines are readable from the test output instead of being
    // duplicated here.
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init();

    let entries = paths.len();
    let directories = paths
        .iter()
        .filter(|path| PathBuf::from(path).is_dir())
        .count();
    let engine = EnhancedDeleteEngine::new();
    let started = Instant::now();
    let result = engine.delete_files(&paths);
    let wall = started.elapsed();

    // Every submitted entry has exactly one terminal outcome.
    assert_eq!(
        result.file_results.len(),
        entries,
        "N_results != N_submitted"
    );
    assert_eq!(
        result.success_count + result.failed_count + result.reboot_pending_count,
        entries,
        "outcomes do not conserve the submitted total"
    );

    for failed in result
        .file_results
        .iter()
        .filter(|entry| !entry.success)
        .take(5)
    {
        eprintln!(
            "[ab] failure: {} -> {:?}",
            failed.path, failed.failure_reason
        );
    }

    let wall_ms = wall.as_millis().max(1);
    let throughput = entries as f64 * 1000.0 / wall_ms as f64;
    println!(
        "[ab] list={list_path} entries={entries} dirs={directories} ok={} failed={} reboot={} freed={} wall_ms={wall_ms} throughput={throughput:.1}/s",
        result.success_count,
        result.failed_count,
        result.reboot_pending_count,
        result.freed_physical_size
    );
}
