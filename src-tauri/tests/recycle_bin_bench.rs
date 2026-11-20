//! Manual A/B driver for the recycle-bin batch (提速方案 §8.2).
//!
//! Not a correctness test: it deletes whatever list it is pointed at, for real,
//! and lets the engine's own `[回收站删除耗时]` lines through so one round can be
//! compared against `recycle-tools/baseline-harness` on the same dataset.
//!
//! Usage (idle machine, no elevation):
//!
//! ```text
//! LIGHTC_AB_LIST=<paths.txt> cargo test --test recycle_bin_bench -- --ignored --nocapture
//! ```
//!
//! The engine reads its worker count from `LIGHTC_RECYCLE_DELETE_WORKERS`, so the
//! concurrency matrix reuses this same command with that variable set.

use std::time::Instant;

use cleanup_c_lib::test_api::EnhancedDeleteEngine;

/// Refuse lists that are not recycle entries: this driver deletes for real, and a
/// typo in the list path must never reach ordinary files.
fn recycle_only(paths: &[String]) -> bool {
    paths
        .iter()
        .all(|path| path.to_ascii_lowercase().contains("\\$recycle.bin\\"))
}

#[test]
#[ignore = "deletes a real recycle-bin dataset listed in LIGHTC_AB_LIST; measurement driver, not a correctness test"]
fn delete_recycle_batch_from_list_file() {
    let list_path = std::env::var("LIGHTC_AB_LIST").expect("LIGHTC_AB_LIST must point at a path list");
    let list = std::fs::read_to_string(&list_path).expect("read the path list");
    let paths: Vec<String> = list
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    assert!(!paths.is_empty(), "the path list is empty: {list_path}");
    assert!(recycle_only(&paths), "the list contains non-recycle paths");

    // `env_logger` is a normal dependency of the crate, so the engine's phase
    // timings are readable from the test output instead of being duplicated here.
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init();

    let entries = paths.len();
    let engine = EnhancedDeleteEngine::new();
    let started = Instant::now();
    let result = engine.delete_files(&paths);
    let wall = started.elapsed();

    // §5.5: every submitted entry has exactly one terminal outcome.
    assert_eq!(result.file_results.len(), entries, "N_results != N_submitted");
    assert_eq!(
        result.success_count + result.failed_count + result.reboot_pending_count,
        entries,
        "outcomes do not conserve the submitted total"
    );

    for failed in result.file_results.iter().filter(|entry| !entry.success).take(5) {
        eprintln!("[ab] failure: {} -> {:?}", failed.path, failed.failure_reason);
    }

    let throughput = entries as f64 * 1000.0 / wall.as_millis().max(1) as f64;
    println!(
        "[ab] list={list_path} entries={entries} ok={} failed={} freed={} wall_ms={} throughput={throughput:.1}/s",
        result.success_count,
        result.failed_count,
        result.freed_physical_size,
        wall.as_millis()
    );
}
