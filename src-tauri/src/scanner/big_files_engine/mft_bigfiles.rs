// ============================================================================
// MFT big-file scanner — one sequential pass over $MFT + Top-N
//
// Previously USN enumeration got the names, then the whole $MFT was read again for sizes (the same data passed through twice),
// and USN enumeration is a kernel traversal of the entire table, the slower of the two passes.
// ============================================================================

#![cfg(target_os = "windows")]

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::time::Instant;

use log::info;

use crate::scanner::big_files::{
    compute_file_risk_level, compute_source_label, is_cancelled, LargeFileEntry,
};
use crate::scanner::big_files_engine::mft_core;

pub struct MftBigFileProgress {
    pub stage: String,
    pub message: String,
    pub processed: usize,
    pub found_count: usize,
    pub elapsed_ms: u64,
    /// Key numbers for this stage (segmentation / disk read / parse, record count, candidate count…), shown directly by the frontend
    pub detail: String,
}

/// Candidate pool multiplier: take this many times TopN first, then filter system directories, so system files don't crowd out user files
const CANDIDATE_MULTIPLIER: usize = 20;
const CANDIDATE_MINIMUM: usize = 1000;
/// Progress event interval: sending too densely would hog the scan thread
const PROGRESS_STEP: usize = 100_000;

const SKIP_PATH_SEGMENTS: &[&str] = &[
    "\\windows\\system32\\",
    "\\windows\\syswow64\\",
    "\\windows\\winsxs\\",
    "\\windows\\assembly\\",
    "\\windows\\microsoft.net\\",
];

fn is_system_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    SKIP_PATH_SEGMENTS.iter().any(|s| p.contains(s))
}

pub fn scan_top_files_via_mft(
    top_n: usize,
    drive_letter: char,
    progress_cb: impl Fn(MftBigFileProgress) + Sync,
) -> Result<Vec<LargeFileEntry>, String> {
    let t0 = Instant::now();
    // The big-file module supports multi-drive selection; MFT direct read must explicitly use the user-selected drive letter, no longer implicitly binding to the system drive.
    let drive_letter = drive_letter.to_ascii_uppercase();

    info!(
        "[MFT-BigFiles] ===== 扫描开始 drive={} top_n={} =====",
        drive_letter, top_n
    );

    let progress = |stage: &str,
                    message: &str,
                    processed: usize,
                    found_count: usize,
                    detail: String| {
        info!("[MFT-BigFiles] {}: {}", message, processed);
        progress_cb(MftBigFileProgress {
            stage: stage.to_string(),
            message: message.to_string(),
            processed,
            found_count,
            elapsed_ms: t0.elapsed().as_millis() as u64,
            detail,
        });
    };

    // ---------- Step 1: read $MFT in one pass (names / parent-child / size / time) ----------
    let mft_started = Instant::now();
    let reader = mft_core::NtfsFileMetadataReader::open(drive_letter)?;
    let (records, mft_stats) = reader.read_all_records(&|processed| {
        // Respond to cancellation during disk reading too, otherwise after clicking stop the user must wait for the whole table to be read
        if processed % PROGRESS_STEP == 0 {
            progress("mft", "正在读取 NTFS 主文件表", processed, 0, String::new());
        }
        !is_cancelled()
    })?;
    let mft_elapsed = mft_started.elapsed();
    let mft_detail = format!(
        "MFT 分段 {} · 读盘 {:.1}s、解析 {:.1}s（同时进行）",
        mft_stats.fragments,
        mft_stats.io_ms as f64 / 1000.0,
        mft_stats.parse_ms as f64 / 1000.0,
    );
    progress(
        "mft",
        &format!("$MFT 读取完成：{} 条有效记录", records.len()),
        records.len(),
        0,
        mft_detail,
    );
    info!(
        "[MFT-BigFiles] Step1: {} 条, {:.1}s",
        records.len(),
        mft_elapsed.as_secs_f32()
    );

    if records.is_empty() {
        return Err("MFT 空".into());
    }

    // ---------- Step 2: candidate pool (keep TopN × 20 by size) ----------
    let candidate_limit = top_n
        .saturating_mul(CANDIDATE_MULTIPLIER)
        .max(CANDIDATE_MINIMUM);
    let mut pool: BinaryHeap<Reverse<(u64, u64, i64)>> =
        BinaryHeap::with_capacity(candidate_limit + 1);
    for record in &records {
        if is_cancelled() {
            return Err("扫描已取消".into());
        }
        // Directory records always have size 0; files without $DATA also have no size, skip them too
        if record.is_dir || record.size == 0 {
            continue;
        }
        pool.push(Reverse((record.size, record.mft_id, record.modified)));
        if pool.len() > candidate_limit {
            pool.pop();
        }
    }

    // (size, record number, modified time), ordered by size descending
    let mut candidates: Vec<(u64, u64, i64)> = pool
        .into_iter()
        .map(|Reverse(found)| found)
        .collect();
    candidates.sort_unstable_by(|left, right| right.0.cmp(&left.0));

    let candidate_ids: HashSet<u64> = candidates.iter().map(|item| item.1).collect();
    progress(
        "topn",
        "正在挑选最大的文件",
        records.len(),
        candidate_ids.len(),
        format!(
            "有效记录 {} 条 → 候选 {} 个",
            records.len(),
            candidate_ids.len()
        ),
    );

    // ---------- Step 3: rebuild paths only for candidates ----------
    let paths_started = Instant::now();
    let paths = mft_core::rebuild_paths_for_records(&records, drive_letter, &candidate_ids);
    let paths_elapsed = paths_started.elapsed();
    progress(
        "paths",
        "正在重建候选文件路径",
        candidate_ids.len(),
        paths.len(),
        format!("候选 {} 个，解析出 {} 条路径", candidate_ids.len(), paths.len()),
    );

    // Once paths are obtained the records are no longer needed; release them early (1.2M records with names take hundreds of MB)
    let record_count = records.len();
    drop(records);

    // ---------- Step 4: system-directory filter + TopN ----------
    let mut top: BinaryHeap<Reverse<(u64, u64)>> = BinaryHeap::with_capacity(top_n + 1);
    for (size, mft_id, _) in &candidates {
        let Some(path) = paths.get(mft_id) else {
            continue;
        };
        if is_system_path(path) {
            continue;
        }
        top.push(Reverse((*size, *mft_id)));
        if top.len() > top_n {
            top.pop();
        }
    }

    // ---------- Step 5: results ----------
    let details_by_id: HashMap<u64, (u64, i64)> = candidates
        .iter()
        .map(|(size, mft_id, modified)| (*mft_id, (*size, *modified)))
        .collect();
    let mut results: Vec<LargeFileEntry> = top
        .into_iter()
        .filter_map(|Reverse((_, mft_id))| {
            let path = paths.get(&mft_id)?;
            let (size, modified) = *details_by_id.get(&mft_id)?;
            Some(LargeFileEntry {
                path: path.clone(),
                size,
                modified,
                risk_level: compute_file_risk_level(path),
                source_label: compute_source_label(path),
            })
        })
        .collect();
    results.sort_by(|left, right| right.size.cmp(&left.size));

    let total_elapsed = t0.elapsed();
    info!(
        "[MFT-BigFiles] ===== 完成: Top-{}, 总 {:.1}s =====",
        results.len(),
        total_elapsed.as_secs_f32()
    );
    progress(
        "summary",
        "扫描完成",
        results.len(),
        results.len(),
        format!(
            "总耗时 {:.1}s · 读盘 {:.1}s、解析 {:.1}s · 路径 {:.2}s · 有效记录 {} 条 → Top {}",
            total_elapsed.as_secs_f32(),
            mft_stats.io_ms as f64 / 1000.0,
            mft_stats.parse_ms as f64 / 1000.0,
            paths_elapsed.as_secs_f32(),
            record_count,
            results.len(),
        ),
    );

    Ok(results)
}
