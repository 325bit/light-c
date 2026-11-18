// ============================================================================
// Junk cleanup deep scanner
//
// Building on the quick scan's known junk directories, deep scanning covers all partitions and all users, and uses
// MFT/USN plus controlled traversal to supplement cache directories; matching rules identify only high-confidence temp
// files, caches, and report directories, without whole-disk extension wildcards, so the user's logs, downloads, or project
// files are not misjudged as junk. Like the quick scan, deep scanning does not filter by file modification time, ensuring deep
// scan results (count and size) are always a superset of the quick scan.
// ============================================================================

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use log::{info, warn};
use rayon::prelude::*;
use serde::Serialize;
use tauri::{Emitter, Window};
use walkdir::WalkDir;

use super::{CategoryScanResult, FileInfo, JunkCategory};

const DEEP_JUNK_PAGE_SIZE: usize = 500;

/// Maximum number of entries returned in a single call.
///
/// Note: the UI's "View all files" needs to fetch an entire category at once (tens of thousands of entries),
/// and fetching one entry at a time over IPC is too slow; a cap is still kept so that a mistaken usize::MAX from the
/// frontend cannot serialize the whole result into hundreds of MB.
const MAX_CATEGORY_PAGE: usize = 100_000;
// A deep scan takes a long time, and the user then needs to browse categories and pages, so sessions are kept for 60 minutes to avoid expiring too early.
const DEEP_JUNK_SESSION_TTL: Duration = Duration::from_secs(60 * 60);
// Number of deep scan sessions to keep, so old sessions are not immediately evicted when the user runs several scans in a row.
const MAX_DEEP_JUNK_SESSIONS: usize = 4;

static DEEP_JUNK_SCAN_CANCELLED: AtomicBool = AtomicBool::new(false);

/// Deep scan stage progress; the frontend uses it to show the current partition and MFT stage.
#[derive(Debug, Clone, Serialize)]
pub struct DeepJunkScanProgress {
    pub stage: String,
    pub drive_letter: String,
    pub message: String,
    pub processed: usize,
    pub matched_count: usize,
    pub elapsed_ms: u64,
}

/// Summary of a single partition's deep scan.
#[derive(Debug, Clone, Serialize)]
pub struct DeepJunkDriveSummary {
    pub drive_letter: String,
    pub file_system: String,
    pub backend: String,
    pub matched_file_count: usize,
    pub matched_size: u64,
    pub warning: Option<String>,
    /// Duration of each stage, used to locate which part of a scan is slow.
    pub stage_durations: Vec<DeepJunkStageDuration>,
    /// Total time for this drive from start to finish (including small untracked overhead).
    pub total_duration_ms: u64,
    /// Number of data runs in `$MFT`. More fragments mean a more fragmented table and less effective sequential-read prefetching.
    pub mft_fragments: usize,
    /// Time spent waiting on disk while reading `$MFT`, tracked separately from the parsing time below.
    pub mft_io_ms: u64,
    /// Time spent parsing `$MFT` records (fixups + attributes).
    pub mft_parse_ms: u64,
}

/// Duration of a single scan stage.
#[derive(Debug, Clone, Serialize)]
pub struct DeepJunkStageDuration {
    pub stage: String,
    pub duration_ms: u64,
}

/// Deep junk scan result. The quick scan keeps using the original ScanResult to avoid breaking the old calling protocol.
#[derive(Debug, Clone, Serialize)]
pub struct DeepJunkScanResult {
    pub scan_mode: String,
    pub scan_id: String,
    pub categories: Vec<CategoryScanResult>,
    pub total_size: u64,
    pub total_file_count: usize,
    pub scan_duration_ms: u64,
    pub scan_timestamp: i64,
    pub drives: Vec<DeepJunkDriveSummary>,
    /// Overhead outside the drives: partition discovery, recycle bin, parallel waits, and aggregation.
    ///
    /// Previously only per-drive timings were available: the first scan took 26.9 seconds while the four drives summed to just 12.8,
    /// so there was no way to tell where the other ten-plus seconds went.
    pub outer_stages: Vec<DeepJunkStageDuration>,
    /// Cumulative wait for progress reporting (summed across threads). Used to tell whether emitting progress is blocking the scan threads.
    pub emit_wait_ms: u64,
}

struct DriveScanResult {
    summary: DeepJunkDriveSummary,
    files: Vec<(JunkCategory, FileInfo)>,
}

struct DeepJunkScanSession {
    created_at: Instant,
    result: DeepJunkScanResult,
}

static DEEP_JUNK_SESSIONS: LazyLock<Mutex<HashMap<String, DeepJunkScanSession>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static DEEP_JUNK_SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Reset the deep scan cancel flag.
pub fn reset_cancelled() {
    DEEP_JUNK_SCAN_CANCELLED.store(false, Ordering::SeqCst);
}

/// Set the deep scan cancel flag; both MFT and fallback traversal check it.
pub fn cancel() {
    DEEP_JUNK_SCAN_CANCELLED.store(true, Ordering::SeqCst);
}

fn is_cancelled() -> bool {
    DEEP_JUNK_SCAN_CANCELLED.load(Ordering::SeqCst)
}

/// Recursion depth limit for deep scanning, passed in from the settings page's "deep discovery scan depth".
///
/// Module-level state is used instead of passing the depth down at every level: the scan chain goes through MFT
/// supplementation and controlled traversal, so adding a parameter would scatter it across five or six function
/// signatures; keeping it beside the cancel flag matches the file's existing style and guarantees a single source of truth for "which depth this scan uses".
static DEEP_SCAN_MAX_DEPTH: AtomicU64 = AtomicU64::new(DEFAULT_DEEP_SCAN_DEPTH);

/// Default depth, kept in sync with the frontend `junkDeepScanDepth` default (16 on the settings page).
/// The old hard-coded 12 was too conservative — deep scanning exists precisely to find deeply hidden caches, so too shallow a depth misses them.
const DEFAULT_DEEP_SCAN_DEPTH: u64 = 16;

/// Upper-bound fallback: even if the frontend sends an abnormal value, traversal depth cannot run away and make scan time explode.
const MAX_DEEP_SCAN_DEPTH: u64 = 24;

/// Set the recursion depth for this deep scan. The command entry point calls this after `reset_cancelled()`.
/// When `None` is passed (old frontend or a direct command call), fall back to the default for predictable behavior.
pub fn set_scan_depth(depth: Option<u32>) {
    let resolved = depth
        .map(|value| (value as u64).clamp(1, MAX_DEEP_SCAN_DEPTH))
        .unwrap_or(DEFAULT_DEEP_SCAN_DEPTH);
    DEEP_SCAN_MAX_DEPTH.store(resolved, Ordering::SeqCst);
}

/// Read the recursion depth limit for this scan.
fn scan_depth() -> usize {
    DEEP_SCAN_MAX_DEPTH.load(Ordering::SeqCst) as usize
}

/// Run the deep junk scan across all fixed partitions.
pub fn scan_all(window: &Window) -> Result<DeepJunkScanResult, String> {
    let started_at = std::time::Instant::now();
    let mut outer_stages: Vec<DeepJunkStageDuration> = Vec::new();
    EMIT_WAIT_MS.store(0, Ordering::Relaxed);
    let mut categories = JunkCategory::all()
        .into_iter()
        .map(CategoryScanResult::new)
        .collect::<Vec<_>>();
    let mut drives = Vec::new();

    #[cfg(windows)]
    {
        let stage = std::time::Instant::now();
        let drive_letters = fixed_drive_letters()?;
        record_stage(&mut outer_stages, "discover", stage);
        if drive_letters.is_empty() {
            return Err("没有找到可访问的固定磁盘分区".to_string());
        }

        emit_progress(
            window,
            "discover",
            "",
            "已发现固定磁盘分区，准备开始深度扫描",
            0,
            0,
            started_at,
        );

        // Drives are independent of each other and must be scanned in parallel: serially the total is the sum of all drives
        // (measured C 7.3s + D 2.7s + E 6.9s + F 1.1s = 18s),
        // while in parallel the total approaches the slowest drive; any remaining time is wasted waiting.
        // The gain is largest when the disks are independent; if two drive letters are on the same physical disk they share bandwidth, reducing the benefit.
        let stage = std::time::Instant::now();
        let (drive_results, recycle_result, recycle_ms) = std::thread::scope(|scope| {
            // Deep mode still keeps the Explorer-visible recycle bin entries and must not enumerate internal files directly via MFT.
            // It is independent of the drives, so it goes in the same scope — previously it ran serially before the parallel block,
            // which measured 1.5 seconds of pure queueing.
            let recycle_handle = scope.spawn(|| {
                let recycle_started = std::time::Instant::now();
                let mut result = CategoryScanResult::new(JunkCategory::RecycleBin);
                super::recycle_bin::scan_current_user(&JunkCategory::RecycleBin, &mut result);
                (result, recycle_started.elapsed().as_millis() as u64)
            });

            let handles: Vec<_> = drive_letters
                .iter()
                .map(|&drive_letter| {
                    scope.spawn(move || {
                        let result = if super::big_files_engine::mft_core::is_ntfs(drive_letter) {
                            match scan_ntfs_drive(drive_letter, window, started_at) {
                                Ok(result) => Ok(result),
                                Err(error) if error == "扫描已取消" => Err(error),
                                Err(error) => {
                                    warn!(
                                        "{} 盘 MFT 深度扫描失败，降级为受控遍历: {}",
                                        drive_letter, error
                                    );
                                    Ok(scan_non_ntfs_drive(
                                        drive_letter,
                                        window,
                                        started_at,
                                        "NTFS",
                                        Some(format!(
                                            "MFT 扫描失败，已降级为受控目录遍历: {}",
                                            error
                                        )),
                                    ))
                                }
                            }
                        } else {
                            Ok(scan_non_ntfs_drive(
                                drive_letter,
                                window,
                                started_at,
                                "非 NTFS",
                                Some("非 NTFS 分区使用受控目录遍历，未执行 MFT 扫描".to_string()),
                            ))
                        };
                        (drive_letter, result)
                    })
                })
                .collect();
            // Join order matches creation order, so results still come back in drive-letter order
            let drives: Vec<(char, Result<DriveScanResult, String>)> = handles
                .into_iter()
                .map(|handle| handle.join().expect("扫描线程异常退出"))
                .collect();
            let (recycle, recycle_ms) = recycle_handle.join().expect("回收站扫描线程异常退出");
            (drives, recycle, recycle_ms)
        });

        // The recycle bin and drives run in the same parallel window, so it no longer adds to total time;
        // it is still recorded separately to tell whether it became the slowest branch.
        outer_stages.push(DeepJunkStageDuration {
            stage: "recycle".to_string(),
            duration_ms: recycle_ms,
        });
        record_stage(&mut outer_stages, "parallel", stage);

        if let Some(category_result) = categories
            .iter_mut()
            .find(|item| item.category == JunkCategory::RecycleBin)
        {
            for file in recycle_result.files {
                category_result.add_file(file);
            }
        }

        let stage = std::time::Instant::now();
        for (_drive_letter, result) in drive_results {
            let result = result?;

            if is_cancelled() {
                return Err("扫描已取消".to_string());
            }

            for (category, file) in result.files {
                if let Some(category_result) =
                    categories.iter_mut().find(|item| item.category == category)
                {
                    category_result.add_file(file);
                }
            }
            drives.push(result.summary);
        }
        record_stage(&mut outer_stages, "merge", stage);
    }

    #[cfg(not(windows))]
    {
        let _ = window;
        return Err("深度垃圾扫描仅支持 Windows 系统".to_string());
    }

    let mut result = DeepJunkScanResult {
        scan_mode: "deep".to_string(),
        scan_id: String::new(),
        categories,
        total_size: 0,
        total_file_count: 0,
        scan_duration_ms: started_at.elapsed().as_millis() as u64,
        scan_timestamp: chrono::Utc::now().timestamp(),
        drives,
        outer_stages,
        emit_wait_ms: EMIT_WAIT_MS.load(Ordering::Relaxed),
    };
    result.total_size = result.categories.iter().map(|item| item.total_size).sum();
    result.total_file_count = result.categories.iter().map(|item| item.file_count).sum();

    // Deep results are merged by partition order and sorted by path within a partition; the order means nothing to the user.
    // Sort all categories by size descending before creating the session, so the frontend's pagination shows "the largest files first".
    for category in &mut result.categories {
        category.sort_files_by_size_desc();
    }

    emit_progress(
        window,
        "summary",
        "",
        &format!(
            "深度扫描完成：发现 {} 个文件，{} 字节",
            result.total_file_count, result.total_size
        ),
        result.total_file_count,
        result.total_file_count,
        started_at,
    );
    info!(
        "深度垃圾扫描完成：{} 个文件，{} 字节，耗时 {}ms",
        result.total_file_count, result.total_size, result.scan_duration_ms
    );
    Ok(result)
}

/// Save the full deep result but return only the first screen of data, to avoid passing a huge number of paths into the WebView at once.
pub fn create_session(mut result: DeepJunkScanResult) -> Result<DeepJunkScanResult, String> {
    let session_id = format!(
        "deep-{}-{}",
        current_unix_timestamp(),
        DEEP_JUNK_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    result.scan_id = session_id.clone();

    let response = page_result(&result, 0, DEEP_JUNK_PAGE_SIZE)?;
    let mut sessions = DEEP_JUNK_SESSIONS
        .lock()
        .map_err(|_| "深度扫描会话锁异常，请重试".to_string())?;
    sessions.retain(|_, session| session.created_at.elapsed() < DEEP_JUNK_SESSION_TTL);
    while sessions.len() >= MAX_DEEP_JUNK_SESSIONS {
        let Some(oldest_id) = sessions
            .iter()
            .min_by_key(|(_, session)| session.created_at)
            .map(|(id, _)| id.clone())
        else {
            break;
        };
        sessions.remove(&oldest_id);
    }
    sessions.insert(
        session_id,
        DeepJunkScanSession {
            created_at: Instant::now(),
            result,
        },
    );
    Ok(response)
}

/// Get the next page of files for a deep scan category.
pub fn get_category_page(
    scan_id: &str,
    category_name: &str,
    offset: usize,
    limit: usize,
) -> Result<CategoryScanResult, String> {
    let sessions = DEEP_JUNK_SESSIONS
        .lock()
        .map_err(|_| "深度扫描会话锁异常，请重试".to_string())?;
    let session = sessions
        .get(scan_id)
        .ok_or_else(|| "深度扫描结果已过期，请重新扫描".to_string())?;
    if session.created_at.elapsed() >= DEEP_JUNK_SESSION_TTL {
        return Err("深度扫描结果已过期，请重新扫描".to_string());
    }

    let category = session
        .result
        .categories
        .iter()
        .find(|category| category.display_name == category_name)
        .ok_or_else(|| format!("未知垃圾分类: {}", category_name))?;
    page_category(category, offset, limit.clamp(1, MAX_CATEGORY_PAGE))
}

/// Retrieve the complete category paths from the short-lived scan session so the delete command can handle files not returned by pagination.
pub fn get_paths_for_categories(
    scan_id: &str,
    category_names: &[String],
    excluded_paths: &[String],
) -> Result<Vec<String>, String> {
    let sessions = DEEP_JUNK_SESSIONS
        .lock()
        .map_err(|_| "深度扫描会话锁异常，请重试".to_string())?;
    let session = sessions
        .get(scan_id)
        .ok_or_else(|| "深度扫描结果已过期，请重新扫描".to_string())?;
    if session.created_at.elapsed() >= DEEP_JUNK_SESSION_TTL {
        return Err("深度扫描结果已过期，请重新扫描".to_string());
    }

    let requested_names = category_names.iter().collect::<HashSet<_>>();
    let excluded = excluded_paths
        .iter()
        .map(|path| path.to_lowercase())
        .collect::<HashSet<_>>();
    let mut matched_names = HashSet::new();
    let mut paths = Vec::new();

    for category in &session.result.categories {
        if !requested_names.contains(&category.display_name) {
            continue;
        }
        matched_names.insert(category.display_name.clone());
        paths.extend(
            category
                .files
                .iter()
                .filter(|file| !excluded.contains(&file.path.to_lowercase()))
                .map(|file| file.path.clone()),
        );
    }

    if matched_names.len() != requested_names.len() {
        return Err("深度扫描分类已变化，请重新扫描后再清理".to_string());
    }
    Ok(paths)
}

fn page_result(
    result: &DeepJunkScanResult,
    offset: usize,
    limit: usize,
) -> Result<DeepJunkScanResult, String> {
    // Note: do not call `result.clone()` first and then replace categories — that would copy the complete file
    // lists of all categories (possibly hundreds of thousands of entries) only to discard them immediately, making pagination more expensive than no pagination.
    Ok(DeepJunkScanResult {
        scan_mode: result.scan_mode.clone(),
        scan_id: result.scan_id.clone(),
        categories: result
            .categories
            .iter()
            .map(|category| page_category(category, offset, limit))
            .collect::<Result<Vec<_>, _>>()?,
        total_size: result.total_size,
        total_file_count: result.total_file_count,
        scan_duration_ms: result.scan_duration_ms,
        scan_timestamp: result.scan_timestamp,
        drives: result.drives.clone(),
        outer_stages: result.outer_stages.clone(),
        emit_wait_ms: result.emit_wait_ms,
    })
}

fn page_category(
    category: &CategoryScanResult,
    offset: usize,
    limit: usize,
) -> Result<CategoryScanResult, String> {
    let start = offset.min(category.files.len());
    let end = start.saturating_add(limit).min(category.files.len());
    Ok(CategoryScanResult {
        category: category.category.clone(),
        display_name: category.display_name.clone(),
        description: category.description.clone(),
        risk_level: category.risk_level,
        system_managed: category.system_managed,
        // Likewise, copy only this page, without cloning the whole category first
        files: category.files[start..end].to_vec(),
        total_size: category.total_size,
        file_count: category.file_count,
        has_more: end < category.file_count,
    })
}

#[cfg(windows)]
fn scan_ntfs_drive(
    drive_letter: char,
    window: &Window,
    started_at: std::time::Instant,
) -> Result<DriveScanResult, String> {
    use super::big_files_engine::mft_core;

    let drive_started = std::time::Instant::now();
    let mut stage_durations: Vec<DeepJunkStageDuration> = Vec::new();
    let drive_label = format!("{}:", drive_letter);
    emit_progress(
        window,
        "mft",
        &drive_label,
        "正在读取 NTFS 主文件表",
        0,
        0,
        started_at,
    );

    // One sequential read of $MFT yields names ($FILE_NAME attribute), parent-child relationships, and sizes ($DATA attribute) together.
    // Previously it first ran a USN enumeration to get names, then read $MFT again for sizes, passing the same data twice;
    // measurements showed USN enumeration walks the whole table in the kernel and is 6x slower than a sequential read in user mode, so here we read only once.
    let stage = std::time::Instant::now();
    let metadata_reader = mft_core::NtfsFileMetadataReader::open(drive_letter)?;
    let (entries, mft_stats) = metadata_reader.read_all_records(&|processed| {
        if is_cancelled() {
            return false;
        }
        if processed == 0 || processed % 100_000 == 0 {
            emit_progress(
                window,
                "mft",
                &drive_label,
                &format!("正在读取 NTFS 主文件表，已检查 {} 条记录", processed),
                processed,
                0,
                started_at,
            );
        }
        true
    })?;
    record_stage(&mut stage_durations, "mft", stage);

    if is_cancelled() {
        return Err("扫描已取消".to_string());
    }

    emit_progress(
        window,
        "path",
        &drive_label,
        "正在重建候选文件路径",
        entries.len(),
        0,
        started_at,
    );

    let parent_ids = entries
        .iter()
        .filter(|entry| !entry.is_dir)
        .map(|entry| entry.parent_id)
        .collect::<HashSet<_>>();
    let stage = std::time::Instant::now();
    let directory_paths = mft_core::rebuild_paths_for_records(&entries, drive_letter, &parent_ids);
    record_stage(&mut stage_durations, "path", stage);
    // Candidates store only indices, not whole records: files matching the rules are usually only a few thousand,
    // but record fields (including filename strings) are still needed during assembly, so indices are cheapest.
    let stage = std::time::Instant::now();
    // Rule matching is pure CPU: comparing strings for millions of paths across the disk takes over two seconds single-threaded.
    // Splitting into chunks and handing them to rayon keeps two reusable buffers per chunk, with no interaction between chunks.
    const FILTER_CHUNK: usize = 16_384;
    let per_chunk: Vec<Vec<(usize, JunkCategory, String)>> = entries
        .par_chunks(FILTER_CHUNK)
        .enumerate()
        .map(|(chunk_index, chunk)| {
            let mut local: Vec<(usize, JunkCategory, String)> = Vec::new();
            // Each chunk has its own buffer to avoid sharing between threads
            let mut path_buffer = String::with_capacity(512);
            let mut normalize_buffer: Vec<u8> = Vec::with_capacity(512);
            for (offset, entry) in chunk.iter().enumerate() {
                if is_cancelled() {
                    return local;
                }
                if entry.is_dir {
                    continue;
                }
                let Some(parent_path) = directory_paths.get(&entry.parent_id) else {
                    continue;
                };
                // MFT already provides the parent directory string; direct concatenation avoids intermediate PathBuf and OsString conversions.
                join_mft_path_into(parent_path, &entry.name, &mut path_buffer);
                let Some(category) =
                    match_deep_junk_category_reusing(&path_buffer, &mut normalize_buffer)
                else {
                    continue;
                };
                // Reaching this point means a real match, and only now is it worth allocating a path
                local.push((
                    chunk_index * FILTER_CHUNK + offset,
                    category,
                    path_buffer.clone(),
                ));
            }
            local
        })
        .collect();

    // Deduplication is done serially after merging: par_chunks preserves chunk order, so the result matches the serial version
    let mut candidates: Vec<(usize, JunkCategory, String)> = Vec::new();
    let mut candidate_paths = HashSet::new();
    for chunk in per_chunk {
        for (index, category, path_string) in chunk {
            if !candidate_paths.insert(path_string.to_ascii_lowercase()) {
                continue;
            }
            candidates.push((index, category, path_string));
        }
    }
    record_stage(&mut stage_durations, "filter", stage);
    let enumerated_count = entries.len();
    drop(directory_paths);
    drop(candidate_paths);

    emit_progress(
        window,
        "filter",
        &drive_label,
        &format!("规则筛选完成：{} 个候选文件", candidates.len()),
        enumerated_count,
        candidates.len(),
        started_at,
    );

    if candidates.is_empty() {
        return Ok(DriveScanResult {
            summary: DeepJunkDriveSummary {
                drive_letter: drive_label,
                file_system: "NTFS".to_string(),
                backend: "mft".to_string(),
                matched_file_count: 0,
                matched_size: 0,
                warning: None,
                stage_durations,
                total_duration_ms: drive_started.elapsed().as_millis() as u64,
                mft_fragments: mft_stats.fragments,
                mft_io_ms: mft_stats.io_ms,
                mft_parse_ms: mft_stats.parse_ms,
            },
            files: Vec::new(),
        });
    }

    // Size and modification time are already parsed during the sequential $MFT read, so there is no second disk read here
    let mut files = Vec::new();
    for (index, category, path) in candidates {
        let record = &entries[index];
        files.push((
            category.clone(),
            FileInfo::new(
                path,
                record.name.clone(),
                record.size,
                record.modified,
                false,
                category,
            ),
        ));
    }

    // MFT/USN enumeration only covers records still present in the change journal; old, long-unchanged system cache files are skipped;
    // here a controlled traversal is done over all whitelisted cache roots, deduplicated with MFT results by lowercase path to fill the gaps.
    let stage = std::time::Instant::now();
    scan_supplement_roots(drive_letter, window, started_at, &mut files);
    record_stage(&mut stage_durations, "walkdir", stage);

    files.sort_by(|left, right| left.1.path.cmp(&right.1.path));
    let matched_size = files.iter().map(|(_, file)| file.size).sum();
    emit_progress(
        window,
        "result",
        &drive_label,
        &format!("{} 盘候选结果整理完成", drive_label),
        files.len(),
        files.len(),
        started_at,
    );

    Ok(DriveScanResult {
        summary: DeepJunkDriveSummary {
            drive_letter: drive_label,
            file_system: "NTFS".to_string(),
            backend: "mft".to_string(),
            matched_file_count: files.len(),
            matched_size,
            warning: None,
            stage_durations,
            total_duration_ms: drive_started.elapsed().as_millis() as u64,
            mft_fragments: mft_stats.fragments,
            mft_io_ms: mft_stats.io_ms,
            mft_parse_ms: mft_stats.parse_ms,
        },
        files,
    })
}

#[cfg(windows)]
fn scan_non_ntfs_drive(
    drive_letter: char,
    window: &Window,
    started_at: std::time::Instant,
    file_system: &str,
    warning: Option<String>,
) -> DriveScanResult {
    let drive_started = std::time::Instant::now();
    let drive_label = format!("{}:", drive_letter);
    let mut files = Vec::new();
    let mut visited = HashSet::new();

    emit_progress(
        window,
        "walkdir",
        &drive_label,
        "当前分区不是 NTFS，正在遍历明确的缓存目录",
        0,
        0,
        started_at,
    );

    for root in non_ntfs_scan_roots(drive_letter) {
        if is_cancelled() {
            break;
        }
        if !root.exists() {
            continue;
        }

        for entry in WalkDir::new(root)
            // Recursion depth is now controlled by the settings page (default 16, max 24), no longer hard-coded.
            .max_depth(scan_depth())
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
        {
            if is_cancelled() {
                break;
            }
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path().to_string_lossy().into_owned();
            if !visited.insert(path.to_ascii_lowercase()) {
                continue;
            }
            let Some(category) = match_deep_junk_category(&path) else {
                continue;
            };
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let modified = metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                .map(|value| value.as_secs() as i64)
                .unwrap_or(0);
            let name = entry.file_name().to_string_lossy().into_owned();
            files.push((
                category.clone(),
                FileInfo::new(path, name, metadata.len(), modified, false, category),
            ));
        }
    }

    files.sort_by(|left, right| left.1.path.cmp(&right.1.path));
    let matched_size = files.iter().map(|(_, file)| file.size).sum();
    emit_progress(
        window,
        "result",
        &drive_label,
        &format!("{} 盘常规遍历完成", drive_label),
        files.len(),
        files.len(),
        started_at,
    );

    DriveScanResult {
        summary: DeepJunkDriveSummary {
            drive_letter: drive_label,
            file_system: file_system.to_string(),
            backend: "walkdir".to_string(),
            matched_file_count: files.len(),
            matched_size,
            warning,
            // Regular traversal does a single full scan with no stage breakdown; without MFT there are no such statistics either
            stage_durations: Vec::new(),
            total_duration_ms: drive_started.elapsed().as_millis() as u64,
            mft_fragments: 0,
            mft_io_ms: 0,
            mft_parse_ms: 0,
        },
        files,
    }
}

#[cfg(windows)]
fn fixed_drive_letters() -> Result<Vec<char>, String> {
    use winapi::um::fileapi::{GetDriveTypeW, GetLogicalDrives};
    use winapi::um::winbase::DRIVE_FIXED;

    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 {
        return Err("无法读取本机磁盘分区列表".to_string());
    }

    Ok((0..26)
        .filter_map(|index| {
            if mask & (1 << index) == 0 {
                return None;
            }
            let letter = (b'A' + index as u8) as char;
            let root = format!("{}:\\", letter);
            let wide_root = root
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect::<Vec<_>>();
            let drive_type = unsafe { GetDriveTypeW(wide_root.as_ptr()) };
            (drive_type == DRIVE_FIXED && Path::new(&root).is_dir()).then_some(letter)
        })
        .collect())
}

#[cfg(windows)]
fn system_level_cache_roots(root: &Path) -> Vec<PathBuf> {
    vec![
        root.join("Windows\\Temp"),
        root.join("Windows\\Prefetch"),
        root.join("Windows\\SoftwareDistribution\\Download"),
        root.join("Windows\\SoftwareDistribution\\DeliveryOptimization"),
        root.join("Windows\\ServiceProfiles\\NetworkService\\AppData\\Local\\Microsoft\\Windows\\DeliveryOptimization\\Cache"),
        root.join("Windows\\Logs"),
        root.join("Windows\\Minidump"),
        root.join("Windows.old"),
        root.join("$Windows.~BT"),
        root.join("$Windows.~WS"),
        root.join("ProgramData\\Microsoft\\Windows\\WER"),
        root.join("ProgramData\\Microsoft\\Windows Defender\\LocalCopy"),
        root.join("ProgramData\\Microsoft\\Windows Defender\\Support"),
        // Defender scan history directory (automatically rebuilt after scanning), consistent with the Defender item in the Windows cleanup wizard.
        root.join("ProgramData\\Microsoft\\Windows Defender\\Scans\\History\\Service"),
        root.join("Windows\\ServiceProfiles\\LocalService\\AppData\\Local\\FontCache"),
        root.join("Windows\\Installer\\$PatchCache$"),
        root.join("Windows\\System32\\d3d_cache"),
        // System-level temporary caches: update rollback staging, kernel crash reports (depth-limited, see below),
        // and Windows Update / app store logs. These directories contain only rebuildable diagnostic/update data.
        root.join("Windows\\SystemTemp"),
        root.join("Windows\\Logs\\WindowsUpdate"),
        root.join("Windows\\ServiceProfiles\\LocalService\\AppData\\Local\\Packages\\Microsoft.WindowsStore_8wekyb3d8bbwe\\LocalCache"),
        // The following directories were not covered before but likewise contain only rebuildable data.
        // ⚠️ Windows\Logs\CBS is **deliberately excluded** — is_windows_log explicitly rejects it,
        //    because component installation logs are the basis for troubleshooting update failures and are "useful", not junk.
        root.join("Windows\\Panther"),                        // install/upgrade logs and leftovers
        root.join("Windows\\Performance\\WinSAT"),            // experience index (Windows Experience Rating) data, rebuildable
        root.join("Windows\\ServiceProfiles\\LocalService\\AppData\\Local\\Temp"), // service account temp directory, keeps accumulating
        root.join("ProgramData\\USOShared\\Logs"),            // update coordinator logs
        root.join("ProgramData\\NVIDIA Corporation\\NV_Cache"), // graphics shader cache, rebuilt by the driver
        root.join("ProgramData\\NVIDIA\\Downloader"),          // driver installer leftovers
    ]
}

/// Supplement root directories that need a traversal depth limit.
///
/// Note: these directories are themselves rebuildable caches, but may contain many deeply nested small files (for example WER's report queue
/// creates multiple subdirectory levels by app/time). Limiting depth covers real junk while preventing deep scanning from spending time on
/// bottomless directory trees.
#[cfg(windows)]
fn depth_limited_system_cache_roots(root: &Path) -> Vec<(PathBuf, usize)> {
    vec![
        (root.join("ProgramData\\Microsoft\\Windows\\WER"), 2),
        (root.join("Windows\\LiveKernelReports"), 1),
    ]
}

/// Explicit high-confidence cache subdirectories under each user profile.
/// Only rebuildable caches are traversed, avoiding WebView/Chromium persistent Profile data.
#[cfg(windows)]
fn user_level_cache_roots(profile: &Path) -> Vec<PathBuf> {
    let local = profile.join("AppData\\Local");
    let roaming = profile.join("AppData\\Roaming");
    vec![
        local.join("Temp"),
        local.join("CrashDumps"),
        local.join("Downloaded Installations"),
        local.join("D3DSCache"),
        local.join("AMD\\DxCache"),
        local.join("NVIDIA\\DXCache"),
        local.join("Intel\\ShaderCache"),
        local.join("Microsoft\\Windows\\INetCache"),
        local.join("Microsoft\\Windows\\Caches"),
        local.join("Microsoft\\Windows\\Explorer"),
        local.join("Microsoft\\Windows\\WebCache"),
        local.join("Microsoft\\Windows\\Clipboard"),
        local.join("Microsoft\\Windows\\WER"),
        // Firefox crash-recovery caches live under Roaming; Service Worker / media caches of Chromium-based browsers
        // are under each browser's user data and are covered by the deep rules.
        roaming.join("Mozilla\\Firefox\\Crash Reports"),
    ]
}

#[cfg(windows)]
fn non_ntfs_scan_roots(drive_letter: char) -> Vec<PathBuf> {
    let root = PathBuf::from(format!("{}:\\", drive_letter));
    let mut roots = system_level_cache_roots(&root);

    // Non-NTFS partitions have no MFT, so the entire AppData\Local must be covered to find scattered caches such as AppCache.
    let users_root = root.join("Users");
    if let Ok(users) = std::fs::read_dir(users_root) {
        for user in users.filter_map(Result::ok) {
            let profile = user.path();
            if profile.is_dir() {
                roots.push(profile.join("AppData\\Local"));
            }
        }
    }
    roots
}

/// Supplement root directories for NTFS deep scanning.
/// MFT/USN enumeration only returns records still present in the change journal; old, long-unchanged or
/// orphaned system cache files are silently skipped; here we list only explicit high-confidence cache subdirectories per user
/// for controlled traversal, and deduplicate/merge with MFT results to reliably cover directories such as Delivery Optimization, update caches, and thumbnails.
#[cfg(windows)]
fn supplement_ntfs_scan_roots(drive_letter: char) -> Vec<PathBuf> {
    let root = PathBuf::from(format!("{}:\\", drive_letter));
    let mut roots = system_level_cache_roots(&root);

    let users_root = root.join("Users");
    if let Ok(users) = std::fs::read_dir(users_root) {
        for user in users.filter_map(Result::ok) {
            let profile = user.path();
            if profile.is_dir() {
                roots.extend(user_level_cache_roots(&profile));
            }
        }
    }
    roots
}

/// Run a controlled traversal over supplement roots, adding high-confidence cache files missed by MFT enumeration to the results.
/// Paths already in `results` (case-insensitive comparison) are skipped to avoid double counting with MFT results.
#[cfg(windows)]
fn scan_supplement_roots(
    drive_letter: char,
    window: &Window,
    started_at: std::time::Instant,
    results: &mut Vec<(JunkCategory, FileInfo)>,
) {
    let drive_label = format!("{}:", drive_letter);
    let mut visited = results
        .iter()
        .map(|(_, file)| file.path.to_ascii_lowercase())
        .collect::<HashSet<_>>();

    emit_progress(
        window,
        "walkdir",
        &drive_label,
        "正在补扫系统缓存目录，兜底 MFT 未覆盖的文件",
        results.len(),
        results.len(),
        started_at,
    );

    let drive_root = PathBuf::from(format!("{}:\\", drive_letter));

    // First filter the supplement roots into a to-do list, then run them in parallel.
    // Filtering (removing roots covered by a parent) must be serial — it depends on the order in which roots were accepted.
    let mut jobs: Vec<(PathBuf, usize)> = Vec::new();
    let mut scanned_roots = Vec::new();
    for root in supplement_ntfs_scan_roots(drive_letter) {
        if is_cancelled() {
            return;
        }
        if is_redundant_supplement_root(&root, &scanned_roots) {
            continue;
        }
        // Supplement roots without their own depth limit follow the settings page's depth.
        jobs.push((root.clone(), scan_depth()));
        scanned_roots.push(root);
    }

    // Depth-limited supplement roots: cover only real junk near the root, avoiding meaningless deep subdirectories.
    for (root, max_depth) in depth_limited_system_cache_roots(&drive_root) {
        if is_cancelled() {
            return;
        }
        if is_redundant_supplement_root(&root, &scanned_roots) {
            continue;
        }
        // These roots carry their own depth limits (to avoid running too long in overly deep trees); take the larger of the two:
        // raising the setting takes effect, while lowering it still keeps each root's own minimum so nothing is missed due to a smaller user setting.
        jobs.push((root.clone(), max_depth.max(scan_depth())));
        scanned_roots.push(root);
    }

    // Roots are independent, so they run in parallel: on C drive this stage measured 1.5 seconds; previously a single thread queued dozens of
    // directories that live in different subtrees with no inherent ordering.
    let batches: Vec<Vec<(JunkCategory, FileInfo)>> = jobs
        .par_iter()
        .map(|(root, max_depth)| {
            let mut local = Vec::new();
            // Each root has its own dedup set: roots may be nested, so deduplication happens again at merge time
            let mut local_visited = HashSet::new();
            scan_supplement_root(root, *max_depth, &mut local, &mut local_visited);
            local
        })
        .collect();

    // Merging follows the to-do list order, with the first occurrence of a duplicate path winning, so the result matches the serial version
    for batch in batches {
        for (category, file) in batch {
            let key = file.path.to_ascii_lowercase();
            if !visited.insert(key) {
                continue;
            }
            results.push((category, file));
        }
    }
}

#[cfg(windows)]
fn join_mft_path_into(parent: &str, name: &str, buffer: &mut String) {
    // Write into the caller's buffer: allocating a String for each of millions of paths is the main cost here,
    // and only the few thousand matching entries actually need to own a path.
    buffer.clear();
    buffer.reserve(parent.len() + 1 + name.len());
    buffer.push_str(parent);
    if !parent.ends_with('\\') && !parent.ends_with('/') {
        buffer.push('\\');
    }
    buffer.push_str(name);
}

#[cfg(windows)]
fn is_redundant_supplement_root(root: &Path, scanned_roots: &[PathBuf]) -> bool {
    let normalized = root
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    scanned_roots.iter().any(|scanned| {
        let existing = scanned
            .to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_ascii_lowercase();
        normalized == existing || normalized.starts_with(&format!("{}\\", existing))
    })
}

/// Run a controlled traversal over a single supplement root, appending matching high-confidence cache files to `results`.
/// When `max_depth` is 0, only check the root directory itself, without recursion.
#[cfg(windows)]
fn scan_supplement_root(
    root: &Path,
    max_depth: usize,
    results: &mut Vec<(JunkCategory, FileInfo)>,
    visited: &mut HashSet<String>,
) {
    if !root.exists() {
        return;
    }

    // 1) Collect file paths within the directory tree in parallel.
    // The cost of supplementation falls almost entirely on one large directory (measured Temp: 19,671 entries / 1.4 seconds,
    // while the other thirty-plus roots together took 10 ms); cross-root parallelism cannot help it, so we must branch within the tree.
    let mut files: Vec<PathBuf> = Vec::new();
    collect_files_parallel(root, max_depth, &mut files);

    // 2) Rule matching is also pure CPU, so it is parallel too; non-matching files stop here and never touch stat
    let hits: Vec<(JunkCategory, PathBuf)> = files
        .into_par_iter()
        .filter_map(|path| {
            let mut normalize_buffer: Vec<u8> = Vec::with_capacity(512);
            let text = path.to_string_lossy();
            match_deep_junk_category_reusing(&text, &mut normalize_buffer)
                .map(|category| (category, path))
        })
        .collect();

    // 3) Deduplication and metadata reads are serial: only matching entries reach here, on the order of thousands rather than tens of thousands
    for (category, path) in hits {
        let path = path.to_string_lossy().into_owned();
        if !visited.insert(path.to_ascii_lowercase()) {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_secs() as i64)
            .unwrap_or(0);
        let name = Path::new(&path)
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default();
        results.push((
            category.clone(),
            FileInfo::new(path, name, metadata.len(), modified, false, category),
        ));
    }
}

/// Collect files at this level under `dir`, appending subdirectories to `subdirs` (do not descend when `remaining` is 1).
/// Only real files count: symlinks and reparse points are always skipped, consistent with the original `WalkDir::follow_links(false)`.
#[cfg(windows)]
fn collect_level(dir: &Path, remaining: usize, out: &mut Vec<PathBuf>, subdirs: &mut Vec<PathBuf>) {
    if remaining == 0 {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.filter_map(Result::ok) {
        if is_cancelled() {
            return;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_file() {
            out.push(entry.path());
        } else if kind.is_dir() && remaining > 1 {
            subdirs.push(entry.path());
        }
    }
}

/// Traverse a subtree serially (used inside parallel branches).
#[cfg(windows)]
fn collect_tree_serial(dir: &Path, remaining: usize, out: &mut Vec<PathBuf>) {
    if remaining == 0 {
        return;
    }
    let mut subdirs = Vec::new();
    collect_level(dir, remaining, out, &mut subdirs);
    for child in subdirs {
        collect_tree_serial(&child, remaining - 1, out);
    }
}

/// Branch only at the first level of the root, with each subtree traversed serially.
///
/// Branching only one level is intentional: deeper recursion would fragment tasks until scheduling overhead exceeds the benefit,
/// while this level's distribution is already uneven (large directories each become a task and land on different threads).
#[cfg(windows)]
fn collect_files_parallel(root: &Path, max_depth: usize, out: &mut Vec<PathBuf>) {
    let mut subdirs = Vec::new();
    collect_level(root, max_depth, out, &mut subdirs);
    if subdirs.is_empty() {
        return;
    }
    let nested: Vec<Vec<PathBuf>> = subdirs
        .par_iter()
        .map(|child| {
            let mut local = Vec::new();
            collect_tree_serial(child, max_depth - 1, &mut local);
            local
        })
        .collect();
    for chunk in nested {
        out.extend(chunk);
    }
}

/// Record the duration of a stage.
///
/// Stages run serially, so timing them individually is the only way to see which one dominates — previously we could only guess.
fn record_stage(
    durations: &mut Vec<DeepJunkStageDuration>,
    stage: &str,
    started: std::time::Instant,
) {
    durations.push(DeepJunkStageDuration {
        stage: stage.to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
    });
}

/// Cumulative milliseconds spent waiting for progress reporting. Four drives report concurrently, so this stores the sum of per-thread waits.
static EMIT_WAIT_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn emit_progress(
    window: &Window,
    stage: &str,
    drive_letter: &str,
    message: &str,
    processed: usize,
    matched_count: usize,
    started_at: std::time::Instant,
) {
    let progress = DeepJunkScanProgress {
        stage: stage.to_string(),
        drive_letter: drive_letter.to_string(),
        message: message.to_string(),
        processed,
        matched_count,
        elapsed_ms: started_at.elapsed().as_millis() as u64,
    };
    // Timing wraps emit: if it needs to wait for the main thread to handle the event, the scan thread is dragged down,
    // which is one candidate cause of "the first scan is especially slow" (the WebView is still initializing at first launch).
    let emit_started = std::time::Instant::now();
    if let Err(error) = window.emit("junk-clean:progress", &progress) {
        warn!("发送垃圾深度扫描进度失败: {}", error);
    }
    EMIT_WAIT_MS.fetch_add(emit_started.elapsed().as_millis() as u64, Ordering::Relaxed);
}

fn current_unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0)
}

/// Determine whether a path belongs to a high-confidence directory allowed for deep cleanup.
pub fn is_deep_junk_path(path: &str) -> bool {
    super::recycle_bin::is_current_user_entry_path(path) || match_deep_junk_category(path).is_some()
}

fn match_deep_junk_category(path: &str) -> Option<JunkCategory> {
    let mut buffer = Vec::new();
    match_deep_junk_category_reusing(path, &mut buffer)
}

/// Necessary condition for a rule match: the normalized (lowercase) path must contain one of the following.
///
/// This list is reverse-engineered from all the rules below; **every rule needs at least one of these**,
/// and missing one means that category of junk will never be found. It must be checked in sync whenever rules change.
const JUNK_PATH_HINTS: &[&str] = &[
    // Three system-level root directories, covering the vast majority of rules
    "\\appdata\\",
    "\\windows\\",
    "\\programdata\\",
    // Legacy system leftovers: the directory name itself is a keyword and does not start with \windows\
    "\\windows.old\\",
    "\\$windows.~bt\\",
    "\\$windows.~ws\\",
    // Browser and third-party app caches are not necessarily under AppData
    // (measured case: E://Google//Chrome//User Data\Default\Cache must also count as a browser cache)
    "\\google\\",
    "\\microsoft\\edge\\",
    "\\bravesoftware\\",
    "\\mozilla\\firefox\\",
    "\\opera software\\",
    "\\tencent\\qqbrowser\\",
    "\\360chrome\\",
    "\\360se6\\",
    "\\sogouexplorer\\",
    "\\ucbrowser\\",
    "\\2345explorer\\",
    "\\liebao\\",
    "\\vivaldi\\",
    "\\yandexbrowser\\",
    "\\centbrowser\\",
    "\\discord\\",
    "\\slack\\",
    "\\microsoft\\teams\\",
    "\\steam\\",
    "\\epicgameslauncher\\",
    "\\netease\\cloudmusic\\",
    "\\tencent\\qqmusic\\",
];

/// Coarse filter: whether the path may contain one of the prefixes above. Byte comparison only, no allocation or case conversion.
///
/// Of the millions of paths on a disk, the vast majority are unrelated to junk rules. Filtering them out here first means
/// the several string allocations and dozen-plus rules below do not have to run.
// `pub` only for benches (through lib.rs's bench_api); the module itself stays pub(crate)
// and is invisible to external users. This is the most performance-sensitive code in the repo,
// so a regression benchmark must keep watch on it.
pub fn could_be_junk_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    for (index, &byte) in bytes.iter().enumerate() {
        // Prefixes all begin with a separator
        if byte != b'\\' && byte != b'/' {
            continue;
        }
        let Some(&next) = bytes.get(index + 1) else {
            continue;
        };
        // Check the first letter first; if it does not match, skip the full comparison
        match next.to_ascii_lowercase() {
            b'a' | b'b' | b'c' | b'd' | b'e' | b'g' | b'l' | b'm' | b'n' | b'o' | b'p' | b's'
            | b't' | b'u' | b'v' | b'w' | b'y' | b'$' | b'2' | b'3' => {}
            _ => continue,
        }
        let rest = &bytes[index..];
        if JUNK_PATH_HINTS
            .iter()
            .any(|hint| starts_with_ignore_ascii_case(rest, hint.as_bytes()))
        {
            return true;
        }
    }
    false
}

fn starts_with_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
    if haystack.len() < needle.len() {
        return false;
    }
    haystack
        .iter()
        .zip(needle)
        .all(|(left, right)| left.to_ascii_lowercase() == *right)
}

/// Write a path into a reusable byte buffer: normalize `/` to `\` and uppercase ASCII to lowercase.
///
/// It processes byte by byte and preserves non-ASCII bytes as-is, so paths containing non-ASCII characters are not corrupted;
/// buffer reuse means a million calls allocate only once, instead of two Strings each time.
// `pub` only for benches (through lib.rs's bench_api): buffer reuse is the core optimization
// on this path, so any change here needs benchmark data to back it up.
pub fn normalize_into(path: &str, buffer: &mut Vec<u8>) {
    buffer.clear();
    buffer.reserve(path.len());
    for &byte in path.as_bytes() {
        buffer.push(match byte {
            b'/' => b'\\',
            b'A'..=b'Z' => byte + 32,
            _ => byte,
        });
    }
}

/// Same as `match_deep_junk_category`, but reuses the caller's buffer for normalization.
///
/// Deep scanning calls this once for every file on the disk, so the allocation count here shows up directly in the timing.
fn match_deep_junk_category_reusing(
    path: &str,
    normalize_buffer: &mut Vec<u8>,
) -> Option<JunkCategory> {
    if !could_be_junk_path(path) {
        return None;
    }
    normalize_into(path, normalize_buffer);
    let Ok(normalized) = std::str::from_utf8(normalize_buffer) else {
        return None;
    };
    if normalized.is_empty() {
        return None;
    }

    // These three whitelist directories must be checked before the system exclusion rules:
    // d3d_cache is under System32, the Defender directories are under the excluded Defender root,
    // and Windows.old itself contains a \Windows\System32\, so checking later would discard it wholesale as a system directory.
    if is_shader_cache(normalized) {
        return Some(JunkCategory::ShaderCache);
    }
    if is_defender_cache(normalized) {
        return Some(JunkCategory::WindowsDefenderCache);
    }
    if is_old_windows_installation(normalized) {
        return Some(JunkCategory::OldWindowsInstallation);
    }
    if is_excluded_path(normalized) {
        return None;
    }

    if contains_any(
        normalized,
        &[
            "\\appdata\\local\\temp\\",
            "\\windows\\temp\\",
            // Windows 10/11 system-level temp directory, containing only cleanable installation/update unpacking data.
            "\\windows\\systemtemp\\",
        ],
    ) {
        return Some(JunkCategory::WindowsTemp);
    }
    if contains_any(
        normalized,
        &[
            "\\windows\\softwaredistribution\\deliveryoptimization\\",
            "\\windows\\serviceprofiles\\networkservice\\appdata\\local\\microsoft\\windows\\deliveryoptimization\\",
        ],
    ) {
        return Some(JunkCategory::DeliveryOptimization);
    }
    if is_windows_internet_cache(normalized) {
        // Legacy IE/WinINet caches are browser resource caches; Cookies use a separate directory and do not match here.
        return Some(JunkCategory::BrowserCache);
    }
    if contains_any(
        normalized,
        &[
            "\\windows\\prefetch\\",
            "\\appdata\\local\\microsoft\\windows\\caches\\",
            // Experience index data and graphics shader caches are both content the system/driver rebuilds after deletion.
            "\\windows\\performance\\winsat\\",
            "\\programdata\\nvidia corporation\\nv_cache\\",
        ],
    ) {
        return Some(JunkCategory::SystemCache);
    }
    if is_browser_cache(normalized) {
        return Some(JunkCategory::BrowserCache);
    }
    if is_user_profile_cache(normalized) {
        // Third-party apps usually put rebuildable caches under the user profile; only explicit cache directory names are matched,
        // without treating all of AppData as junk or touching MSIX/WebView persistent data.
        return Some(JunkCategory::AppCache);
    }
    if is_third_party_app_cache(normalized) {
        // Supplement known apps' unconventional cache directory names (e.g. Steam htmlcache, Epic webcache);
        // these directories are automatically rebuilt after deletion and are put in a separate category for easy identification.
        return Some(JunkCategory::ThirdPartyAppCache);
    }
    if contains_any(normalized, &["\\windows\\softwaredistribution\\download\\"]) {
        return Some(JunkCategory::WindowsUpdate);
    }
    if is_thumbnail_cache(normalized) {
        return Some(JunkCategory::ThumbnailCache);
    }
    if contains_any(
        normalized,
        &[
            "\\appdata\\local\\crashdumps\\",
            "\\windows\\minidump\\",
            "\\appdata\\local\\microsoft\\windows\\wer\\",
            "\\programdata\\microsoft\\windows\\wer\\",
        ],
    ) {
        return Some(JunkCategory::WindowsErrorReports);
    }
    if normalized.ends_with("\\windows\\memory.dmp")
        // Dump files in kernel crash report directories (WATCHDOG and other subdirectories) are also cleanable diagnostic data.
        || normalized.contains("\\windows\\livekernelreports\\")
    {
        return Some(JunkCategory::MemoryDump);
    }
    if is_windows_log(normalized) {
        return Some(JunkCategory::LogFiles);
    }
    if contains_any(
        normalized,
        &[
            // Installation/upgrade logs (Windows\Panther) and update coordinator logs (USOShared\Logs):
            // both troubleshooting history; deleting them does not affect functionality, but their size grows with each update.
            "\\windows\\panther\\",
            "\\programdata\\usoshared\\logs\\",
        ],
    ) {
        return Some(JunkCategory::LogFiles);
    }
    if contains_any(
        normalized,
        &[
            "\\appdata\\local\\downloaded installations\\",
            "\\windows\\installer\\$patchcache$\\",
            // Installer packages left by the NVIDIA driver downloader, useless after installation.
            "\\programdata\\nvidia\\downloader\\",
        ],
    ) {
        return Some(JunkCategory::InstallerTemp);
    }
    if normalized.contains("\\appdata\\local\\microsoft\\windows\\webcache\\") {
        return Some(JunkCategory::AppCache);
    }
    if normalized.contains("\\windows\\serviceprofiles\\localservice\\appdata\\local\\fontcache\\")
    {
        return Some(JunkCategory::FontCache);
    }
    if normalized.contains("\\appdata\\local\\microsoft\\windows\\clipboard\\") {
        return Some(JunkCategory::ClipboardCache);
    }
    None
}

fn contains_any(path: &str, markers: &[&str]) -> bool {
    markers.iter().any(|marker| path.contains(marker))
}

/// Leftovers of old Windows installations and upgrade staging directories.
/// These directories contain complete \Windows\System32\ and similar structures and must be checked before the system exclusion rules,
/// otherwise Windows.old would be reduced to a pile of excluded fragments and the user would see deep scanning "find no old system".
fn is_old_windows_installation(path: &str) -> bool {
    contains_any(
        path,
        &["\\windows.old\\", "\\$windows.~bt\\", "\\$windows.~ws\\"],
    )
}

fn is_excluded_path(path: &str) -> bool {
    contains_any(
        path,
        &[
            "\\$recycle.bin\\",
            "\\system volume information\\",
            "\\program files\\",
            "\\program files (x86)\\",
            "\\users\\default\\",
            "\\windows\\system32\\",
            "\\windows\\syswow64\\",
            "\\windows\\winsxs\\",
            "\\windows\\assembly\\",
            "\\windows\\servicing\\",
            "\\programdata\\microsoft\\windows defender\\",
            "\\appdata\\local\\packages\\",
            "\\ebwebview\\default\\",
            "\\local storage\\",
            "\\indexeddb\\",
            "\\session storage\\",
            "\\databases\\",
            "\\file system\\",
        ],
    ) && !is_shader_cache(path)
        && !is_defender_cache(path)
}

fn is_shader_cache(path: &str) -> bool {
    contains_any(
        path,
        &[
            "\\windows\\system32\\d3d_cache\\",
            "\\appdata\\local\\d3dscache\\",
            "\\appdata\\local\\amd\\dxcache\\",
            "\\appdata\\local\\nvidia\\dxcache\\",
            // NVIDIA OpenGL shader cache and DX compilation cache, automatically rebuilt after deletion
            "\\appdata\\local\\nvidia\\glcache\\",
            "\\appdata\\local\\nvidia\\dxc\\",
            // NVIDIA compute shader cache (CUDA/OptiX compilation output), also automatically rebuilt by the driver.
            "\\appdata\\local\\nvidia\\computecache\\",
            "\\appdata\\local\\intel\\shadercache\\",
        ],
    )
}

fn is_defender_cache(path: &str) -> bool {
    contains_any(
        path,
        &[
            "\\programdata\\microsoft\\windows defender\\localcopy\\",
            "\\programdata\\microsoft\\windows defender\\support\\",
            // Defender scan history directory, automatically rebuilt after scanning, consistent with the Defender item in the Windows cleanup wizard.
            "\\programdata\\microsoft\\windows defender\\scans\\history\\service\\",
        ],
    )
}

fn is_windows_internet_cache(path: &str) -> bool {
    path.contains("\\appdata\\local\\microsoft\\windows\\inetcache\\")
}

fn is_browser_cache(path: &str) -> bool {
    let known_browser = contains_any(
        path,
        &[
            "\\google\\chrome\\user data\\",
            "\\microsoft\\edge\\user data\\",
            "\\bravesoftware\\brave-browser\\user data\\",
            "\\mozilla\\firefox\\profiles\\",
            "\\opera software\\opera stable\\",
            // Domestic Chromium-based browsers (cache directories match Chrome's and are rebuilt automatically)
            "\\tencent\\qqbrowser\\",
            "\\360chrome\\chrome\\user data\\",
            "\\360se6\\user data\\",
            "\\sogouexplorer\\",
            "\\ucbrowser\\user data\\",
            "\\2345explorer\\",
            "\\liebao\\user data\\",
            // Other international Chromium-based browsers
            "\\vivaldi\\user data\\",
            "\\yandexbrowser\\user data\\",
            "\\opera software\\opera gx stable\\",
            "\\centbrowser\\user data\\",
        ],
    );
    known_browser
        && contains_any(
            path,
            &[
                "\\cache\\",
                "\\code cache\\",
                "\\gpucache\\",
                "\\shadercache\\",
                "\\cache2\\",
                // GPU shader cache directory in newer Chrome/Edge versions, automatically rebuilt after deletion
                "\\grshadercache\\",
                "\\graphitedawncache\\",
                "\\dawngraphitecache\\",
                "\\dawnwebgpucache\\",
                // Chromium's PWA/offline resource caches, at the same level as cache and equally rebuildable;
                // note that persistent site-data directories such as CacheStorage/IndexedDB are not included.
                "\\service worker\\",
                "\\scriptcache\\",
                // Media cache: buffered audio/video data from web pages; replaying the content re-downloads it.
                "\\media cache\\",
                "\\jump list icons\\",
                // Firefox's startup cache and crash-recovery cache, both under the profile and automatically rebuilt.
                "\\startupcache\\",
                "\\cache-tmp\\",
                "\\crash reports\\",
            ],
        )
}

fn is_user_profile_cache(path: &str) -> bool {
    if path.contains("\\appdata\\local\\packages\\") {
        return false;
    }

    let profile_markers = [
        "\\appdata\\local\\",
        "\\appdata\\locallow\\",
        "\\appdata\\roaming\\",
    ];
    let Some(profile_marker) = profile_markers
        .iter()
        .find(|marker| path.contains(**marker))
    else {
        return false;
    };
    let Some(profile_path) = path.split_once(profile_marker).map(|(_, suffix)| suffix) else {
        return false;
    };
    let segments = profile_path.split('\\').collect::<Vec<_>>();
    // Only directory names that explicitly mean "cache" are matched; common casing and variants are covered (Cache2/.cache/Cache Data),
    // while persistent directory names such as Data, Settings, and Local Storage are not matched.
    let cache_names = [
        "cache",
        "caches",
        "cache2",
        ".cache",
        "cache data",
        "code cache",
        "gpucache",
        "shadercache",
        "d3dscache",
        "crashdumps",
        // The following directory names likewise contain only rebuildable data but were not included before, so other caches
        // in the same parent were found while these subdirectories were missed (users felt "only half of the app was cleaned").
        "service worker",
        "media cache",
        "startupcache",
        "cache-tmp",
        "crash reports",
        // Package manager caches: npm / pnpm caches are often several GB and 100% rebuildable (re-downloaded on reinstall).
        // These two names were missing before — "npm-cache" is not "cache", so it did not match the exact segment check above.
        "npm-cache",
        // pnpm's cache directory is named `pnpm-cache` (no dot); only `.pnpm-store` was included before,
        // so users with the default install location could not clean these hundreds of MB.
        "pnpm-cache",
        "cacheddata",
        ".pnpm-store",
        // VS Code's CachedData holds compiled JS and is rebuilt on restart, another explicit cache name.
    ];

    segments
        .iter()
        .take(segments.len().saturating_sub(1))
        .any(|segment| {
            cache_names
                .iter()
                .any(|name| segment.eq_ignore_ascii_case(name))
        })
}

/// Known rebuildable cache directories of third-party apps (whitelist combination matching)
/// In deep scanning, AppCache only matches exact directory segments such as cache; here we add
/// combinations of "known app directory + explicit cache directory name" to cover unconventional names
/// such as Steam htmlcache and Epic webcache. Cache directories are automatically rebuilt after deletion and persistent data directories are not touched.
fn is_third_party_app_cache(path: &str) -> bool {
    // Do a quick app-directory filter first to avoid pointless cache-word matching across all disk paths (deep scan hot path)
    let app_dir = contains_any(
        path,
        &[
            "\\discord\\",
            "\\slack\\",
            "\\microsoft\\teams\\",
            "\\steam\\",
            "\\epicgameslauncher\\saved\\",
            "\\netease\\cloudmusic\\",
            "\\tencent\\qqmusic\\",
        ],
    );
    if !app_dir {
        return false;
    }
    contains_any(
        path,
        &[
            "\\cache\\",
            "\\code cache\\",
            "\\gpucache\\",
            "\\shadercache\\",
            "\\htmlcache\\",
            "\\webcache\\",
        ],
    )
}

fn is_thumbnail_cache(path: &str) -> bool {
    if !path.contains("\\appdata\\local\\microsoft\\windows\\explorer\\") {
        return false;
    }
    let Some(name) = Path::new(path).file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    // IconCache.db is the older icon cache without an underscore and is just as rebuildable as thumbcache_*.db.
    name == "iconcache.db"
        || ((name.starts_with("thumbcache_") || name.starts_with("iconcache_"))
            && name.ends_with(".db"))
}

fn is_windows_log(path: &str) -> bool {
    if !path.contains("\\windows\\logs\\") || path.contains("\\windows\\logs\\cbs\\") {
        return false;
    }
    matches!(
        Path::new(path)
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .as_deref(),
        Some("log" | "etl" | "evtx")
    )
}

#[cfg(test)]
mod tests {
    use super::{
        could_be_junk_path, create_session, get_category_page, get_paths_for_categories,
        is_deep_junk_path, match_deep_junk_category, DeepJunkScanResult,
    };
    use crate::scanner::{CategoryScanResult, FileInfo, JunkCategory};

    /// The coarse filter is a pure performance optimization: if it ever misses a path category, that junk will never be found.
    ///
    /// Here a representative path is taken for **every rule**; the coarse filter must pass it and the full rule must also match.
    /// If a rule is added later and `JUNK_PATH_HINTS` is forgotten, this test will fail first.
    #[test]
    fn junk_hint_filter_covers_every_rule() {
        let samples = [
            // Shader cache / Defender cache / old system leftovers
            r"C:\Windows\System32\d3d_cache\a",
            r"C:\Users\a\AppData\Local\NVIDIA\DXCache\b",
            r"C:\ProgramData\Microsoft\Windows Defender\Scans\History\Service\c",
            r"C:\Windows.old\Windows\System32\d.dll",
            r"C:\$WINDOWS.~BT\e",
            r"D:\$WINDOWS.~WS\f",
            // Temp directories / Delivery Optimization / IE cache / system cache
            r"C:\Windows\Temp\g.tmp",
            r"C:\Users\a\AppData\Local\Temp\h.tmp",
            r"C:\Windows\SystemTemp\i",
            r"C:\Windows\SoftwareDistribution\DeliveryOptimization\j",
            r"C:\Users\a\AppData\Local\Microsoft\Windows\INetCache\k",
            r"C:\Windows\Prefetch\l.pf",
            r"C:\ProgramData\NVIDIA Corporation\NV_Cache\m",
            // Browser / user directory caches / third-party app caches
            r"C:\Users\a\AppData\Local\Google\Chrome\User Data\Default\Cache\n",
            r"C:\Users\a\AppData\Roaming\Mozilla\Firefox\Profiles\x\startupCache\o",
            r"C:\Users\a\AppData\Roaming\Code\Cache\p",
            r"C:\Steam\htmlcache\q",
            r"C:\Users\a\AppData\Roaming\Discord\Cache\r",
            // Update packages / thumbnails / error reports / kernel dumps
            r"C:\Windows\SoftwareDistribution\Download\s.cab",
            r"C:\Users\a\AppData\Local\Microsoft\Windows\Explorer\thumbcache_128.db",
            r"C:\Users\a\AppData\Local\CrashDumps\t.dmp",
            r"C:\Windows\Minidump\u.dmp",
            r"C:\Windows\LiveKernelReports\v.dmp",
            // Logs / installer leftovers / web cache / font and clipboard caches
            r"C:\Windows\Logs\MeasuredBoot\w.log",
            r"C:\Windows\Panther\x.xml",
            r"C:\ProgramData\USOShared\Logs\y",
            r"C:\Users\a\AppData\Local\Downloaded Installations\z",
            r"C:\Windows\Installer\$PatchCache$\1",
            r"C:\Users\a\AppData\Local\Microsoft\Windows\WebCache\2",
            r"C:\Windows\ServiceProfiles\LocalService\AppData\Local\FontCache\3",
            r"C:\Users\a\AppData\Local\Microsoft\Windows\Clipboard\4",
        ];
        for path in samples {
            assert!(could_be_junk_path(path), "粗筛漏掉了这条路径: {path}");
            assert!(
                match_deep_junk_category(path).is_some(),
                "规则没有命中（样本或规则有问题）: {path}"
            );
        }
    }

    /// The coarse filter must not let unrelated paths through too often, nor misjudge them as matches.
    #[test]
    fn junk_hint_filter_rejects_unrelated_paths() {
        for path in [
            r"D:\code\project\src\main.rs",
            r"C:\Users\a\Documents\report.docx",
            r"C:\Program Files\App\app.exe",
            r"F:\backup\photos\1.jpg",
        ] {
            assert!(!could_be_junk_path(path), "无关路径不该通过粗筛: {path}");
            assert!(
                match_deep_junk_category(path).is_none(),
                "无关路径不该命中规则: {path}"
            );
        }
    }

    /// Parallel traversal must collect **exactly the same** set of files as `WalkDir`.
    ///
    /// This change trades correctness for speed: supplement results feed directly into the "files found" total,
    /// so a single missing file changes the number the user sees; multi-level structures are used here to align every depth first.
    #[cfg(windows)]
    #[test]
    fn parallel_walk_collects_the_same_files_as_walkdir() {
        use walkdir::WalkDir;

        let base = std::env::temp_dir().join("lightc-walk-equivalence");
        let _ = std::fs::remove_dir_all(&base);
        for relative in [
            "top.bin",
            "a/one.bin",
            "a/b/two.bin",
            "a/b/c/three.bin",
            "d/deep/four.bin",
        ] {
            let path = base.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"x").unwrap();
        }
        // Empty directories must be created too: parallel traversal must not fail just because it reads nothing
        std::fs::create_dir_all(base.join("empty")).unwrap();

        for depth in 1..=5usize {
            let mut fast = Vec::new();
            super::collect_files_parallel(&base, depth, &mut fast);
            fast.sort();

            let mut slow: Vec<std::path::PathBuf> = WalkDir::new(&base)
                .max_depth(depth)
                .follow_links(false)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_file())
                .map(|entry| entry.path().to_path_buf())
                .collect();
            slow.sort();

            assert_eq!(fast, slow, "max_depth = {depth} 时收集结果不一致");
        }

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn matches_only_high_confidence_junk_paths() {
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\Temp\old.tmp"
        ));
        assert!(is_deep_junk_path(
            r"E:\Google\Chrome\User Data\Default\Cache\data_1"
        ));
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\SomeTool\Cache\data_1"
        ));
        assert!(is_deep_junk_path(r"D:\Windows\Prefetch\OLD.PF"));
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\Microsoft\Windows\INetCache\IE\entry.dat"
        ));
        assert!(!is_deep_junk_path(r"D:\Users\Alice\Downloads\old.tmp"));
        assert!(!is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\Packages\App\EBWebView\Default\Cache\data"
        ));
        assert!(!is_deep_junk_path(r"D:\$Recycle.Bin\S-1-5-21\$R123"));
    }

    /// Directory and cache directory names added in this round: they must match new rebuildable content without pulling in useful data.
    #[test]
    fn matches_newly_added_cache_roots_and_names() {
        // Newly added system-level directories
        assert!(is_deep_junk_path(r"C:\Windows\Panther\setupact.log"));
        assert!(is_deep_junk_path(
            r"C:\Windows\Performance\WinSAT\winsat.wmv"
        ));
        assert!(is_deep_junk_path(
            r"C:\ProgramData\USOShared\Logs\System\UpdateStore.etl"
        ));
        assert!(is_deep_junk_path(
            r"C:\ProgramData\NVIDIA Corporation\NV_Cache\abc.bin"
        ));
        assert!(is_deep_junk_path(
            r"C:\ProgramData\NVIDIA\Downloader\setup.exe"
        ));
        assert!(is_deep_junk_path(
            r"C:\Windows\ServiceProfiles\LocalService\AppData\Local\Temp\tmp.tmp"
        ));
        // Newly included cache directory names (package managers / editors)
        assert!(is_deep_junk_path(
            r"C:\Users\Alice\AppData\Local\npm-cache\_cacache\blob"
        ));
        assert!(is_deep_junk_path(
            r"C:\Users\Alice\AppData\Roaming\npm-cache\index-v5\entry"
        ));
        assert!(is_deep_junk_path(
            r"C:\Users\Alice\AppData\Local\.pnpm-store\v3\files\x"
        ));
        assert!(is_deep_junk_path(
            r"C:\Users\Alice\AppData\Roaming\Code\CachedData\abc\cache.bin"
        ));

        // Counterexample 1: CBS logs are **deliberately kept** (component installation logs are used to troubleshoot update failures) and must not be pulled in by the new rules.
        assert!(!is_deep_junk_path(r"C:\Windows\Logs\CBS\CBS.log"));
        // Counterexample 2: similarly named directories outside cache directories must not be hit (npm's project directory is not a cache).
        assert!(!is_deep_junk_path(
            r"C:\Users\Alice\AppData\Local\npm\my-project\index.js"
        ));
        assert!(!is_deep_junk_path(
            r"C:\Users\Alice\AppData\Roaming\Code\User\settings.json"
        ));
        // Counterexample 3: still within an excluded area.
        assert!(!is_deep_junk_path(
            r"C:\Windows\System32\LogFiles\W3SVC1\log.log"
        ));
        assert!(!is_deep_junk_path(
            r"C:\ProgramData\Package Cache\vc_runtime\a.msi"
        ));
    }

    #[test]
    fn classifies_legacy_internet_cache_as_browser_cache() {
        // The actual target of Temporary Internet Files is INetCache, which should be classified as a browser cache rather than a system cache.
        assert_eq!(
            super::match_deep_junk_category(
                r"D:\Users\Alice\AppData\Local\Microsoft\Windows\INetCache\IE\entry.dat"
            ),
            Some(JunkCategory::BrowserCache)
        );
        // Cookies live in INetCookies and must stay out of the junk scan scope to avoid clearing login state.
        assert!(!is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\Microsoft\Windows\INetCookies\entry.dat"
        ));
    }

    #[test]
    fn keeps_persistent_profile_data_out_of_relaxed_cache_rules() {
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Roaming\SomeTool\Caches\old.bin"
        ));
        assert!(!is_deep_junk_path(
            r"D:\Users\Alice\AppData\Roaming\SomeTool\Local Storage\leveldb\data"
        ));
        assert!(!is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\Packages\Some.App\LocalCache\Cache\data"
        ));
    }

    #[test]
    fn keeps_shader_cache_as_a_known_system_exception() {
        assert!(is_deep_junk_path(
            r"C:\Windows\System32\d3d_cache\shader.bin"
        ));
    }

    #[test]
    fn matches_cache_name_variants() {
        // Cache directory name variants (cache2/.cache/Cache Data) should match AppCache
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\Mozilla\Firefox\Profiles\abc\cache2\entries"
        ));
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\SomeApp\.cache\data"
        ));
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Roaming\SomeApp\Cache Data\file"
        ));
    }

    #[test]
    fn matches_additional_rebuildable_cache_directories() {
        // Chromium's Service Worker / media caches and Firefox's startup cache were missed before,
        // and matching them eliminates the "only half of the app is cleaned" problem.
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\Google\Chrome\User Data\Default\Service Worker\CacheStorage\abc"
        ));
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\Microsoft\Edge\User Data\Default\Media Cache\data_0"
        ));
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\SomeApp\Service Worker\ScriptCache\index"
        ));
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Roaming\Mozilla\Firefox\Profiles\abc.default\startupCache\cache.bin"
        ));
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Roaming\Mozilla\Firefox\Profiles\abc.default\Crash Reports\crash.dmp"
        ));
        // Chromium's newer GPU shader caches and NVIDIA ComputeCache are equally rebuildable.
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\Google\Chrome\User Data\GPUCache\data_0"
        ));
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\NVIDIA\ComputeCache\shader.bin"
        ));
        // Windows built-in diagnostic caches: system temp directory, kernel crash reports, legacy icon cache.
        assert!(is_deep_junk_path(r"D:\Windows\SystemTemp\a.tmp"));
        assert!(is_deep_junk_path(
            r"D:\Windows\LiveKernelReports\WATCHDOG\report.dmp"
        ));
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\Microsoft\Windows\Explorer\iconcache.db"
        ));
    }

    #[test]
    fn keeps_persistent_site_data_out_of_new_cache_rules() {
        // CacheStorage may be cleaned, but WebView2/MSIX app persistent directories must still be excluded as a whole,
        // so the new rules do not pull in session data of apps such as Claude.
        assert!(!is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\Packages\Some.App\LocalCache\Local\app\EBWebView\Default\Cache\data"
        ));
        assert!(!is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\Packages\Some.App\LocalCache\Local\app\EBWebView\Default\Service Worker\CacheStorage\data"
        ));
        // Browser persistent site data (Local Storage / IndexedDB / Session Storage) is out of the cleanup scope.
        assert!(!is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\Google\Chrome\User Data\Default\Local Storage\leveldb\data"
        ));
        assert!(!is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\Google\Chrome\User Data\Default\IndexedDB\abc\data"
        ));
        assert!(!is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\Google\Chrome\User Data\Default\Session Storage\data"
        ));
        // Ordinary user files are not misjudged by the new rules.
        assert!(!is_deep_junk_path(r"D:\Users\Alice\Documents\report.dmp"));
        assert!(!is_deep_junk_path(r"D:\Windows\System32\kernel32.dll"));
    }

    #[test]
    fn matches_known_third_party_app_caches() {
        // Unconventional cache directory names (htmlcache/webcache) match third-party app caches
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\Steam\htmlcache\index.html"
        ));
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\EpicGamesLauncher\Saved\webcache\data"
        ));
        // Standard Cache directories still go to the AppCache category and also match
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Roaming\discord\Cache\data_1"
        ));
        // Persistent data and game files do not match
        assert!(!is_deep_junk_path(
            r"D:\Users\Alice\AppData\Roaming\discord\Local Storage\leveldb\data"
        ));
        assert!(!is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\Steam\steamapps\common\game\data.bin"
        ));
    }

    #[test]
    fn matches_extended_browser_and_shader_caches() {
        // Domestic and international Chromium-based browser caches should match, without harming non-cache data.
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\Tencent\QQBrowser\User Data\Default\Cache\data_1"
        ));
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\360Chrome\Chrome\User Data\Default\Code Cache\js\cache"
        ));
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\Vivaldi\User Data\Default\GPUCache\data_0"
        ));
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\CentBrowser\User Data\Default\Cache\data_1"
        ));
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\YandexBrowser\User Data\Default\Cache\data_1"
        ));
        // Newer Chrome/Edge GPU cache directories match, while non-cache directories do not.
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\Google\Chrome\User Data\Default\GrShaderCache\GPUCache\data_0"
        ));
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\Microsoft\Edge\User Data\Default\DawnGraphiteCache\data_0"
        ));
        assert!(!is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\Google\Chrome\User Data\Default\Local Storage\leveldb\data"
        ));
        // NVIDIA shader cache variants match.
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\NVIDIA\GLCache\shader.bin"
        ));
        assert!(is_deep_junk_path(
            r"D:\Users\Alice\AppData\Local\NVIDIA\DXC\compiled.bin"
        ));
    }

    #[test]
    fn matches_windows_cleanup_wizard_cache_categories() {
        assert!(is_deep_junk_path(
            r"C:\ProgramData\Microsoft\Windows Defender\Support\MPLog-old.log"
        ));
        assert!(is_deep_junk_path(
            r"C:\Windows\ServiceProfiles\NetworkService\AppData\Local\Microsoft\Windows\DeliveryOptimization\Cache\payload.bin"
        ));
        // Defender scan history directories (rebuilt after scanning) should match, while key data such as quarantine remains protected.
        assert!(is_deep_junk_path(
            r"C:\ProgramData\Microsoft\Windows Defender\Scans\History\Service\DetectionHistory\00000000-0000-0000-0000-000000000000\entry.bin"
        ));
        assert!(!is_deep_junk_path(
            r"C:\ProgramData\Microsoft\Windows Defender\Quarantine\entry.bin"
        ));
        // Imitation directory names (extra characters after History) must not match, to avoid allowing nested forged paths.
        assert!(!is_deep_junk_path(
            r"C:\ProgramData\Microsoft\Windows Defender\Scans\History\ServiceEvil\entry.bin"
        ));
    }

    #[test]
    fn supplement_roots_only_cover_high_confidence_cache_dirs() {
        // System-level cache directories covered by supplement roots should match deep rules (aligned with cleanmgr).
        assert!(is_deep_junk_path(
            r"C:\Windows\SoftwareDistribution\Download\update.cab"
        ));
        assert!(is_deep_junk_path(
            r"C:\Windows\SoftwareDistribution\DeliveryOptimization\payload.bin"
        ));
        assert!(is_deep_junk_path(r"C:\Windows\Temp\old.tmp"));
        assert!(is_deep_junk_path(
            r"C:\Windows\Logs\setupapi\setupapi.dev.log"
        ));
        // Supplement roots cover only explicit cache subdirectories, not all of AppData\Local.
        assert!(!is_deep_junk_path(
            r"C:\Users\Alice\AppData\Local\Microsoft\Windows\Explorer\data\something.bin"
        ));
    }

    #[test]
    fn pages_large_deep_results_without_changing_total_count() {
        let mut category = CategoryScanResult::new(JunkCategory::WindowsTemp);
        for index in 0..501 {
            category.add_file(FileInfo::new(
                format!(r"D:\Users\Test\AppData\Local\Temp\{}.tmp", index),
                format!("{}.tmp", index),
                1,
                1,
                false,
                JunkCategory::WindowsTemp,
            ));
        }
        let result = DeepJunkScanResult {
            scan_mode: "deep".to_string(),
            scan_id: String::new(),
            categories: vec![category],
            total_size: 501,
            total_file_count: 501,
            scan_duration_ms: 0,
            scan_timestamp: 0,
            drives: Vec::new(),
            outer_stages: Vec::new(),
            emit_wait_ms: 0,
        };

        let first_page = create_session(result).expect("创建深度扫描会话");
        assert_eq!(first_page.categories[0].files.len(), 500);
        assert_eq!(first_page.categories[0].file_count, 501);
        assert!(first_page.categories[0].has_more);

        let second_page = get_category_page(&first_page.scan_id, "Windows临时文件", 500, 500)
            .expect("获取深度扫描第二页");
        assert_eq!(second_page.files.len(), 1);
        assert!(!second_page.has_more);

        let category_names = vec!["Windows临时文件".to_string()];
        let excluded_paths = vec![first_page.categories[0].files[0].path.clone()];
        let full_paths =
            get_paths_for_categories(&first_page.scan_id, &category_names, &excluded_paths)
                .expect("恢复未分页的完整分类路径");
        // The backend should return all 501 files of the category and correctly exclude the 1 path the frontend explicitly cancelled.
        assert_eq!(full_paths.len(), 500);
        assert!(full_paths.iter().any(|path| path.ends_with("500.tmp")));
    }

    #[test]
    fn sorts_deep_category_files_by_size_before_paging() {
        let mut category = CategoryScanResult::new(JunkCategory::WindowsTemp);
        for (name, size) in [
            ("small.tmp", 1_u64),
            ("large.tmp", 900),
            ("medium.tmp", 500),
        ] {
            category.add_file(FileInfo::new(
                format!(r"D:\Users\Test\AppData\Local\Temp\{}", name),
                name.to_string(),
                size,
                1,
                false,
                JunkCategory::WindowsTemp,
            ));
        }
        category.sort_files_by_size_desc();

        let result = DeepJunkScanResult {
            scan_mode: "deep".to_string(),
            scan_id: String::new(),
            categories: vec![category],
            total_size: 1401,
            total_file_count: 3,
            scan_duration_ms: 0,
            scan_timestamp: 0,
            drives: Vec::new(),
            outer_stages: Vec::new(),
            emit_wait_ms: 0,
        };

        // The first page must directly contain the largest files, so the user does not have to page repeatedly to find big files.
        let first_page = create_session(result).expect("创建深度扫描会话");
        let sizes = first_page.categories[0]
            .files
            .iter()
            .map(|file| file.size)
            .collect::<Vec<_>>();
        assert_eq!(sizes, vec![900, 500, 1]);
        assert_eq!(first_page.categories[0].total_size, 1401);
    }

    /// Deep scanning must be a superset of quick scanning, otherwise the user sees "deep scanning found fewer files".
    ///
    /// Note: this uses C drive paths matching this machine, running each quick-scan path from `JunkCategory::get_scan_paths()` through the deep rules;
    /// any path that does not match means the user would miss an entire directory in deep mode.
    /// Thumbnail caches match by filename rule (thumbcache_*.db); representative filenames are given here.
    #[test]
    fn deep_rules_cover_every_quick_scan_path() {
        const QUICK_SCAN_TARGETS: &[(&str, &str)] = &[
            ("WindowsTemp", r"C:\Users\chunyu\AppData\Local\Temp\a.tmp"),
            ("WindowsTemp", r"C:\WINDOWS\Temp\a.tmp"),
            ("SystemCache", r"C:\WINDOWS\Prefetch\APP.PF"),
            (
                "SystemCache",
                r"C:\Users\chunyu\AppData\Local\Microsoft\Windows\Caches\cversion.ini",
            ),
            (
                "BrowserCache",
                r"C:\Users\chunyu\AppData\Local\Microsoft\Windows\INetCache\IE\entry.dat",
            ),
            (
                "BrowserCache",
                r"C:\Users\chunyu\AppData\Local\Google\Chrome\User Data\Default\Cache\data_1",
            ),
            (
                "BrowserCache",
                r"C:\Users\chunyu\AppData\Local\Google\Chrome\User Data\Default\Code Cache\js\index",
            ),
            (
                "BrowserCache",
                r"C:\Users\chunyu\AppData\Local\Google\Chrome\User Data\Default\GPUCache\data_0",
            ),
            (
                "BrowserCache",
                r"C:\Users\chunyu\AppData\Local\Google\Chrome\User Data\Profile 1\Cache\data_1",
            ),
            (
                "BrowserCache",
                r"C:\Users\chunyu\AppData\Local\Google\Chrome\User Data\ShaderCache\data_0",
            ),
            (
                "BrowserCache",
                r"C:\Users\chunyu\AppData\Local\Microsoft\Edge\User Data\Default\Cache\data_1",
            ),
            (
                "BrowserCache",
                r"C:\Users\chunyu\AppData\Local\Mozilla\Firefox\Profiles\abc123.default\cache2\entries\A1B2",
            ),
            (
                "BrowserCache",
                r"C:\Users\chunyu\AppData\Roaming\Mozilla\Firefox\Profiles\abc123.default\cache2\entries\A1B2",
            ),
            (
                "BrowserCache",
                r"C:\Users\chunyu\AppData\Local\BraveSoftware\Brave-Browser\User Data\Default\Cache\data_1",
            ),
            (
                "BrowserCache",
                r"C:\Users\chunyu\AppData\Roaming\Opera Software\Opera Stable\Cache\data_1",
            ),
            (
                "BrowserCache",
                r"C:\Users\chunyu\AppData\Local\Tencent\QQBrowser\User Data\Default\Cache\data_1",
            ),
            (
                "BrowserCache",
                r"C:\Users\chunyu\AppData\Local\360Chrome\Chrome\User Data\Default\Cache\data_1",
            ),
            (
                "BrowserCache",
                r"C:\Users\chunyu\AppData\Local\Vivaldi\User Data\Default\Cache\data_1",
            ),
            (
                "BrowserCache",
                r"C:\Users\chunyu\AppData\Local\Google\Chrome\User Data\Default\Service Worker\CacheStorage\data",
            ),
            (
                "BrowserCache",
                r"C:\Users\chunyu\AppData\Local\Google\Chrome\User Data\Default\Media Cache\data_0",
            ),
            (
                "BrowserCache",
                r"C:\Users\chunyu\AppData\Local\Mozilla\Firefox\Profiles\abc.default\startupCache\cache.bin",
            ),
            (
                "BrowserCache",
                r"C:\Users\chunyu\AppData\Roaming\Mozilla\Firefox\Profiles\abc.default\Crash Reports\a.dmp",
            ),
            (
                "ThumbnailCache",
                r"C:\Users\chunyu\AppData\Local\Microsoft\Windows\Explorer\iconcache.db",
            ),
            (
                "MemoryDump",
                r"C:\WINDOWS\LiveKernelReports\WATCHDOG\report.dmp",
            ),
            ("WindowsTemp", r"C:\WINDOWS\SystemTemp\a.tmp"),
            (
                "WindowsUpdate",
                r"C:\WINDOWS\Logs\WindowsUpdate\WindowsUpdate.20240101.etl",
            ),
            (
                "WindowsUpdate",
                r"C:\WINDOWS\SoftwareDistribution\Download\update.cab",
            ),
            (
                "DeliveryOptimization",
                r"C:\WINDOWS\SoftwareDistribution\DeliveryOptimization\payload.bin",
            ),
            (
                "DeliveryOptimization",
                r"C:\WINDOWS\ServiceProfiles\NetworkService\AppData\Local\Microsoft\Windows\DeliveryOptimization\Cache\payload.bin",
            ),
            (
                "WindowsDefenderCache",
                r"C:\ProgramData\Microsoft\Windows Defender\LocalCopy\copy.bin",
            ),
            (
                "WindowsDefenderCache",
                r"C:\ProgramData\Microsoft\Windows Defender\Support\MPLog.log",
            ),
            (
                "WindowsDefenderCache",
                r"C:\ProgramData\Microsoft\Windows Defender\Scans\History\Service\entry.bin",
            ),
            ("LogFiles", r"C:\WINDOWS\Logs\setupapi\setupapi.dev.log"),
            (
                "LogFiles",
                r"C:\Users\chunyu\AppData\Local\CrashDumps\app.exe.1234.dmp",
            ),
            ("MemoryDump", r"C:\WINDOWS\Minidump\010101-1234-01.dmp"),
            ("MemoryDump", r"C:\WINDOWS\MEMORY.DMP"),
            (
                "OldWindowsInstallation",
                r"C:\Windows.old\Windows\System32\old.dll",
            ),
            (
                "OldWindowsInstallation",
                r"C:\$Windows.~BT\sources\install.wim",
            ),
            (
                "AppCache",
                r"C:\Users\chunyu\AppData\Local\Microsoft\Windows\WebCache\V01.log",
            ),
            (
                "ThirdPartyAppCache",
                r"C:\Users\chunyu\AppData\Roaming\discord\Cache\data_1",
            ),
            (
                "ThirdPartyAppCache",
                r"C:\Users\chunyu\AppData\Local\Steam\htmlcache\index.html",
            ),
            (
                "ThirdPartyAppCache",
                r"C:\Users\chunyu\AppData\Local\EpicGamesLauncher\Saved\webcache\data",
            ),
            (
                "FontCache",
                r"C:\WINDOWS\ServiceProfiles\LocalService\AppData\Local\FontCache\font.bin",
            ),
            (
                "WindowsErrorReports",
                r"C:\Users\chunyu\AppData\Local\Microsoft\Windows\WER\ReportQueue\a.wer",
            ),
            (
                "WindowsErrorReports",
                r"C:\ProgramData\Microsoft\Windows\WER\ReportQueue\a.wer",
            ),
            (
                "InstallerTemp",
                r"C:\WINDOWS\Installer\$PatchCache$\Managed\entry.bin",
            ),
            (
                "InstallerTemp",
                r"C:\Users\chunyu\AppData\Local\Downloaded Installations\setup.msi",
            ),
            (
                "ClipboardCache",
                r"C:\Users\chunyu\AppData\Local\Microsoft\Windows\Clipboard\entry.bin",
            ),
            (
                "ThumbnailCache",
                r"C:\Users\chunyu\AppData\Local\Microsoft\Windows\Explorer\thumbcache_256.db",
            ),
            (
                "ThumbnailCache",
                r"C:\Users\other\AppData\Local\Microsoft\Windows\Explorer\iconcache_48.db",
            ),
            ("ShaderCache", r"C:\WINDOWS\System32\d3d_cache\shader.bin"),
            (
                "ShaderCache",
                r"C:\Users\chunyu\AppData\Local\D3DSCache\shader.bin",
            ),
            (
                "ShaderCache",
                r"C:\Users\chunyu\AppData\Local\NVIDIA\DXCache\shader.bin",
            ),
            (
                "ShaderCache",
                r"C:\Users\chunyu\AppData\Local\Intel\ShaderCache\shader.bin",
            ),
        ];

        let missing = QUICK_SCAN_TARGETS
            .iter()
            .filter(|(_, path)| !is_deep_junk_path(path))
            .map(|(label, path)| format!("{} -> {}", label, path))
            .collect::<Vec<_>>();

        assert!(
            missing.is_empty(),
            "以下快速扫描路径未被深度扫描覆盖，深度结果会少于快速扫描：\n{}",
            missing.join("\n")
        );
    }
}
