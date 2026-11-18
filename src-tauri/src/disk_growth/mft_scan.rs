// ============================================================================
// Standalone MFT full-disk directory scanner
//
// Use FSCTL_ENUM_USN_DATA to enumerate NTFS file records and rebuild paths, then read file
// metadata in parallel to obtain sizes. This avoids the repeated directory re-entry of traditional recursive traversal and suits full-disk change analysis.
// ============================================================================

#![cfg(target_os = "windows")]

use rayon::prelude::*;
use rayon::ThreadPoolBuilder;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Instant, UNIX_EPOCH};

use winapi::um::fileapi::GetDriveTypeW;
use winapi::um::winbase::DRIVE_FIXED;


use crate::scanner::big_files_engine::mft_core;

const DEFAULT_MAX_DEPTH: u8 = 4;
const METADATA_PROGRESS_STEP: usize = 10_000;
/// Fixed record number of the volume root directory in NTFS: the first 16 MFT records are metafiles, and `$Root` is record 5.
const ROOT_MFT_ID: u64 = 5;

static DISK_GROWTH_SCAN_CANCELLED: AtomicBool = AtomicBool::new(false);

pub fn reset_disk_growth_cancelled() {
    DISK_GROWTH_SCAN_CANCELLED.store(false, Ordering::SeqCst);
}

pub fn cancel_disk_growth_scan() {
    log::info!("收到取消全盘分析请求");
    DISK_GROWTH_SCAN_CANCELLED.store(true, Ordering::SeqCst);
}

fn is_disk_growth_cancelled() -> bool {
    DISK_GROWTH_SCAN_CANCELLED.load(Ordering::SeqCst)
}

fn ensure_not_cancelled() -> Result<(), String> {
    if is_disk_growth_cancelled() {
        Err("扫描已取消".to_string())
    } else {
        Ok(())
    }
}

/// A single MFT record. This directly uses the parse result from `mft_core` - one sequential read yields
/// the name, parent-child relationship, size, and time; no separate enumeration or on-demand seek of FILE records is needed.
type RawEntry = mft_core::MftRecordInfo;

#[derive(Debug, Clone)]
struct FileRecord {
    path: String,
    parent_id: u64,
    size: u64,
    modified: i64,
    has_data_stream: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct FileSizeRecord {
    pub(crate) parent_id: u64,
    pub(crate) path: String,
    pub(crate) size: u64,
    pub(crate) modified: i64,
}

struct FileSizeCollection {
    records: Vec<FileSizeRecord>,
    mft_size_count: usize,
    metadata_fallback_count: usize,
}

#[derive(Debug, Clone, Copy)]
struct FileRecordMetadata {
    size: u64,
    modified: i64,
}

#[derive(Debug, Clone)]
struct DirectoryInfo {
    parent_id: u64,
    path: String,
    depth: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirSizeEntry {
    pub path: String,
    pub size: u64,
    pub depth: u8,
    pub modified: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSnapshotEntry {
    pub path: String,
    pub size: u64,
    #[serde(default)]
    pub modified: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskGrowthPhaseDuration {
    pub stage: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskGrowthScanProgress {
    pub stage: String,
    pub message: String,
    pub processed: usize,
    pub total: Option<usize>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone)]
pub struct FullDiskScanResult {
    pub entries: Vec<DirSizeEntry>,
    // File-level details are only used when the backend writes snapshot parts; they do not enter the frontend response, avoiding a second copy of the path list on huge disks.
    pub(crate) file_records: Vec<FileSizeRecord>,
    pub total_size: u64,
    pub total_files_scanned: usize,
    pub scan_duration_ms: u64,
    pub root_path: String,
    pub backend: String,
    pub phase_durations: Vec<DiskGrowthPhaseDuration>,
    pub mft_size_count: usize,
    pub metadata_fallback_count: usize,
    /// The number of `$MFT` data runs, disk read time, and parse time: used to determine whether the bottleneck is the disk or the CPU.
    pub mft_fragments: usize,
    pub mft_io_ms: u64,
    pub mft_parse_ms: u64,
}

pub fn scan_drive_with_progress<F>(
    drive_letter: &str,
    progress: &F,
) -> Result<FullDiskScanResult, String>
where
    F: Fn(DiskGrowthScanProgress) + Sync,
{
    let drive_letter = normalize_drive_letter(drive_letter)?;
    let root = format!("{}:\\", drive_letter);
    scan_disk_with_progress(&root, DEFAULT_MAX_DEPTH, progress)
}

fn scan_disk_with_progress<F>(
    root: &str,
    max_depth: u8,
    progress: &F,
) -> Result<FullDiskScanResult, String>
where
    F: Fn(DiskGrowthScanProgress) + Sync,
{
    let start = Instant::now();
    let mut phase_durations = Vec::new();
    let drive_letter = root
        .chars()
        .find(|c| c.is_ascii_alphabetic())
        .ok_or_else(|| "无效的磁盘路径，无法识别盘符".to_string())?
        .to_ascii_uppercase();
    let drive_root = format!("{}:\\", drive_letter);

    ensure_fixed_drive(&drive_root)?;

    emit_progress(progress, &start, "mft", "正在读取 NTFS 主文件表", 0, None);
    let phase_start = Instant::now();
    // One sequential read of $MFT yields names ($FILE_NAME), parent-child relationships, sizes ($DATA), and modification times.
    // Previously, USN enumeration got names first, then another reader randomly sought by mft_id for sizes -
    // the same data was processed twice, while measurements showed enumeration alone took 36.9s and a single full-table read only 3.9s.
    let metadata_reader = mft_core::NtfsFileMetadataReader::open(drive_letter)?;
    let (raw_entries, mft_stats) = metadata_reader.read_all_records(&|processed| {
        if is_disk_growth_cancelled() {
            return false;
        }
        if processed % 100_000 == 0 {
            emit_progress(
                progress,
                &start,
                "mft",
                "正在读取 NTFS 主文件表",
                processed,
                None,
            );
        }
        true
    })?;
    push_phase_duration(&mut phase_durations, "mft", phase_start);
    ensure_not_cancelled()?;

    emit_progress(
        progress,
        &start,
        "path",
        "正在重建文件路径",
        raw_entries.len(),
        None,
    );
    let phase_start = Instant::now();
    // Build the directory index in one pass: paths are assembled into their final form directly during BFS (forward slashes + lowercase),
    // and file paths just append a filename to the parent path - the original built paths for directories and files separately then normalized,
    // costing nearly three million string allocations for 710k records.
    let directory_index = build_directory_index(&raw_entries, &drive_root);
    ensure_not_cancelled()?;
    let file_records = collect_file_records(&raw_entries, &directory_index);
    push_phase_duration(&mut phase_durations, "path", phase_start);
    ensure_not_cancelled()?;

    emit_progress(
        progress,
        &start,
        "metadata",
        "正在读取文件大小",
        0,
        Some(file_records.len()),
    );
    let phase_start = Instant::now();
    let file_size_collection = collect_file_sizes(file_records, progress, &start);
    push_phase_duration(&mut phase_durations, "metadata", phase_start);
    ensure_not_cancelled()?;

    emit_progress(
        progress,
        &start,
        "aggregate",
        "正在聚合目录大小",
        file_size_collection.records.len(),
        None,
    );
    let phase_start = Instant::now();
    let entries = aggregate_directories(&file_size_collection.records, &directory_index, max_depth);
    ensure_not_cancelled()?;
    let total_size = file_size_collection
        .records
        .iter()
        .map(|record| record.size)
        .sum();
    push_phase_duration(&mut phase_durations, "aggregate", phase_start);
    let total_files_scanned = file_size_collection.records.len();

    Ok(FullDiskScanResult {
        entries,
        file_records: file_size_collection.records,
        total_size,
        total_files_scanned,
        scan_duration_ms: start.elapsed().as_millis() as u64,
        root_path: drive_root,
        backend: "mft".to_string(),
        phase_durations,
        mft_size_count: file_size_collection.mft_size_count,
        metadata_fallback_count: file_size_collection.metadata_fallback_count,
        mft_fragments: mft_stats.fragments,
        mft_io_ms: mft_stats.io_ms,
        mft_parse_ms: mft_stats.parse_ms,
    })
}

fn ensure_fixed_drive(root: &str) -> Result<(), String> {
    let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    let drive_type = unsafe { GetDriveTypeW(wide.as_ptr()) };
    if drive_type != DRIVE_FIXED {
        return Err(format!("仅支持固定磁盘扫描: {}", root));
    }
    Ok(())
}

fn normalize_drive_letter(value: &str) -> Result<char, String> {
    let Some(letter) = value
        .chars()
        .find(|character| character.is_ascii_alphabetic())
    else {
        return Err("无效的磁盘盘符".to_string());
    };
    // The frontend may pass D, D:, or D:\; keep only the drive letter here to ensure one disk maps to only one set of snapshots.
    Ok(letter.to_ascii_uppercase())
}

/// Build the directory index in a single traversal: while BFS descends, paths are assembled in **final form** (forward slashes + lowercase).
///
/// The original took three steps: build a backslash path for each directory -> clone it and normalize the whole thing -> then build a separate path for each file.
/// Now directory paths are allocated only once and depth accumulates along BFS (no longer counting separators in strings),
/// and cycle detection uses "whether this path is in the index" instead of an extra `HashSet`.
fn build_directory_index(entries: &[RawEntry], drive_root: &str) -> HashMap<u64, DirectoryInfo> {
    // Directories make up roughly one sixth of all valid records
    let mut children: HashMap<u64, Vec<u32>> = HashMap::with_capacity(entries.len() / 6);
    for (index, entry) in entries.iter().enumerate() {
        // Files have no children; putting them in children would only make BFS do a wasted pass
        if entry.mft_id == 0 || !entry.is_dir {
            continue;
        }
        children.entry(entry.parent_id).or_default().push(index as u32);
    }

    let mut directories: HashMap<u64, DirectoryInfo> = HashMap::with_capacity(children.len() + 1);
    directories.insert(
        ROOT_MFT_ID,
        DirectoryInfo {
            parent_id: 0,
            path: normalize_path(drive_root),
            depth: 0,
        },
    );

    let mut queue = VecDeque::new();
    if let Some(root_children) = children.get(&ROOT_MFT_ID) {
        queue.extend(root_children.iter().copied());
    }

    while let Some(index) = queue.pop_front() {
        let entry = &entries[index as usize];
        // Skip directories that already have a path built: this also blocks infinite loops caused by abnormal parent chains
        if entry.mft_id == 0 || directories.contains_key(&entry.mft_id) {
            continue;
        }
        // If the parent is not in the index, the chain breaks at a location unreachable from the root, so the whole branch cannot be built
        let Some(parent) = directories.get(&entry.parent_id) else {
            continue;
        };
        let depth = parent.depth.saturating_add(1);
        let path = join_normalized_path(&parent.path, &entry.name);
        // The immutable borrow of directories above ends here, so insertion is safe
        directories.insert(
            entry.mft_id,
            DirectoryInfo {
                parent_id: entry.parent_id,
                path,
                depth,
            },
        );

        if let Some(next_children) = children.get(&entry.mft_id) {
            queue.extend(next_children.iter().copied());
        }
    }

    directories
}

fn collect_file_records(
    entries: &[RawEntry],
    directory_index: &HashMap<u64, DirectoryInfo>,
) -> Vec<FileRecord> {
    // Reserve capacity up front by record count: relying on collect's automatic growth would repeatedly move a tens-of-MB array several times
    let mut records = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry.is_dir || entry.mft_id == 0 {
            continue;
        }
        // The parent path in the directory index is **already in normalized form** (lowercase + forward slashes)
        let Some(parent) = directory_index.get(&entry.parent_id) else {
            continue;
        };
        records.push(FileRecord {
            path: join_normalized_path(&parent.path, &entry.name),
            parent_id: entry.parent_id,
            // Size and time were already obtained during the sequential $MFT read, just carry them through here
            size: entry.size,
            modified: entry.modified,
            has_data_stream: entry.has_data_stream,
        });
    }
    records
}

/// Assemble the full file path and normalize it to `normalize_path`'s form: **forward slashes + lowercase**.
///
/// The parent path comes from the directory index and is already normalized; so here only add a separator, append the filename,
/// then lowercase in place. This allocates a file path only once - the original did it three times ("build full path -> clone -> normalize").
///
/// Lowercasing happens in two steps: first `make_ascii_lowercase` (in place, zero allocation); only when non-ASCII uppercase letters remain
/// does it fall back to `to_lowercase()`. This keeps the result byte-for-byte identical to the old implementation (direct `to_lowercase()`),
/// with no extra allocation for Chinese or Japanese filenames.
fn join_normalized_path(parent: &str, name: &str) -> String {
    // Callers pass only paths from the directory index, which are already normalized; passing a raw path would produce mixed separators
    debug_assert!(!parent.contains('\\'), "父路径应先规范化: {parent}");
    let mut path = String::with_capacity(parent.len() + 1 + name.len());
    path.push_str(parent);
    if !parent.ends_with('/') {
        path.push('/');
    }
    path.push_str(name);
    path.make_ascii_lowercase();
    if path.chars().any(char::is_uppercase) {
        // Only non-ASCII uppercase letters (such as Ö, Д) reach here
        path = path.to_lowercase();
    }
    path
}

fn collect_file_sizes<F>(
    file_records: Vec<FileRecord>,
    progress: &F,
    scan_start: &Instant,
) -> FileSizeCollection
where
    F: Fn(DiskGrowthScanProgress) + Sync,
{
    let total = file_records.len();
    let processed = AtomicUsize::new(0);
    let mft_size_count = AtomicUsize::new(0);
    let metadata_fallback_count = AtomicUsize::new(0);
    let threads = metadata_thread_count();
    let read_sizes = || {
        file_records
            .into_par_iter()
            .filter_map(|record| {
                if is_disk_growth_cancelled() {
                    return None;
                }
                let current = processed.fetch_add(1, Ordering::Relaxed) + 1;
                if current % METADATA_PROGRESS_STEP == 0 || current == total {
                    emit_progress(
                        progress,
                        scan_start,
                        "metadata",
                        "正在整理文件大小",
                        current,
                        Some(total),
                    );
                }

                // Size and time were already parsed during the sequential $MFT read; here we **do not read the disk again**,
                // just move the record into the structure needed for aggregation (the original rescanned $MFT for this).
                // Only records whose `$DATA` was not parsed fall back to one stat, keeping the numbers consistent with before.
                let (metadata, from_mft) = if record.has_data_stream {
                    (
                        FileRecordMetadata {
                            size: record.size,
                            modified: record.modified,
                        },
                        true,
                    )
                } else {
                    let metadata = fs::metadata(&record.path).ok()?;
                    let modified = metadata
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                        .map(|duration| duration.as_secs() as i64)
                        .unwrap_or(0);
                    (
                        FileRecordMetadata {
                            size: metadata.len(),
                            modified,
                        },
                        false,
                    )
                };
                // Empty files (including directory placeholders) are not included in results, so they should not count toward "size source" statistics
                if metadata.size == 0 {
                    return None;
                }
                if from_mft {
                    mft_size_count.fetch_add(1, Ordering::Relaxed);
                } else {
                    metadata_fallback_count.fetch_add(1, Ordering::Relaxed);
                }
                Some(FileSizeRecord {
                    parent_id: record.parent_id,
                    // collect_file_records already produced normalized paths; no need to process again here
                    path: record.path,
                    size: metadata.size,
                    modified: metadata.modified,
                })
            })
            .collect()
    };

    let records = match ThreadPoolBuilder::new().num_threads(threads).build() {
        Ok(pool) => pool.install(read_sizes),
        Err(_) => read_sizes(),
    };

    FileSizeCollection {
        records,
        mft_size_count: mft_size_count.load(Ordering::Relaxed),
        metadata_fallback_count: metadata_fallback_count.load(Ordering::Relaxed),
    }
}

fn aggregate_directories(
    file_sizes: &[FileSizeRecord],
    directory_index: &HashMap<u64, DirectoryInfo>,
    max_depth: u8,
) -> Vec<DirSizeEntry> {
    let mut size_by_dir: HashMap<u64, u64> = HashMap::new();
    let mut modified_by_dir: HashMap<u64, i64> = HashMap::new();

    for record in file_sizes {
        let mut current_id = record.parent_id;
        let mut guard = 0usize;

        while let Some(directory) = directory_index.get(&current_id) {
            if directory.depth <= max_depth {
                *size_by_dir.entry(current_id).or_insert(0) += record.size;
                // A directory's own mtime on Windows does not necessarily reflect child file changes; aggregate the newest file modification time within the subtree instead, closer to the "change time" users care about.
                let modified = modified_by_dir.entry(current_id).or_insert(0);
                if record.modified > *modified {
                    *modified = record.modified;
                }
            }

            if directory.parent_id == 0 || directory.parent_id == current_id {
                break;
            }

            current_id = directory.parent_id;
            guard += 1;
            if guard > 128 {
                // Extremely abnormal MFT parent chains may cycle or be too deep; truncate directly to ensure the whole scan can finish.
                break;
            }
        }
    }

    let mut entries: Vec<DirSizeEntry> = size_by_dir
        .into_iter()
        .filter_map(|(directory_id, size)| {
            let directory = directory_index.get(&directory_id)?;
            Some(DirSizeEntry {
                depth: directory.depth,
                path: directory.path.clone(),
                size,
                modified: modified_by_dir.get(&directory_id).copied().unwrap_or(0),
            })
        })
        .collect();

    entries.sort_by(|left, right| {
        right
            .size
            .cmp(&left.size)
            .then_with(|| left.path.cmp(&right.path))
    });
    entries
}

pub fn normalize_path(path: &str) -> String {
    path.to_lowercase().replace('\\', "/")
}

fn metadata_thread_count() -> usize {
    // metadata reads are random IO; too many threads make the SSD, Defender, and filesystem filter drivers compete for resources.
    num_cpus::get().clamp(4, 8)
}

fn push_phase_duration(
    phases: &mut Vec<DiskGrowthPhaseDuration>,
    stage: &str,
    started_at: Instant,
) {
    phases.push(DiskGrowthPhaseDuration {
        stage: stage.to_string(),
        duration_ms: started_at.elapsed().as_millis() as u64,
    });
}

fn emit_progress<F>(
    progress: &F,
    scan_start: &Instant,
    stage: &str,
    message: &str,
    processed: usize,
    total: Option<usize>,
) where
    F: Fn(DiskGrowthScanProgress) + Sync,
{
    progress(DiskGrowthScanProgress {
        stage: stage.to_string(),
        message: message.to_string(),
        processed,
        total,
        elapsed_ms: scan_start.elapsed().as_millis() as u64,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_path_uses_forward_slash_and_lowercase() {
        assert_eq!(normalize_path("C:\\Users\\Evan"), "c:/users/evan");
    }

    /// Determining the size source: records with `$DATA` use the size in the record; only those without `$DATA` fall back to stat.
    /// Truly empty files (with `$DATA` but size 0) must be discarded directly - do not stat just because `size == 0`,
    /// which would stat tens of thousands of empty files one by one.
    #[test]
    fn collect_file_sizes_only_stats_records_without_data_stream() {
        let dir = std::env::temp_dir().join("lightc-disk-growth-sizes");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let real_file = dir.join("payload.bin");
        std::fs::write(&real_file, vec![7u8; 128]).unwrap();

        let records = vec![
            // Size is right in the record: do not touch the disk
            FileRecord {
                path: "C:////fake////from-mft.bin".to_string(),
                parent_id: 26,
                size: 4096,
                modified: 100,
                has_data_stream: true,
            },
            // No $DATA: fall back to stat to get the real size
            FileRecord {
                path: real_file.to_string_lossy().to_string(),
                parent_id: 26,
                size: 0,
                modified: 0,
                has_data_stream: false,
            },
            // Truly empty file: has $DATA, size 0; discard directly, no stat
            FileRecord {
                path: "C:////fake////empty.bin".to_string(),
                parent_id: 26,
                size: 0,
                modified: 100,
                has_data_stream: true,
            },
        ];

        let started = Instant::now();
        let collection = collect_file_sizes(records, &|_| {}, &started);

        assert_eq!(collection.records.len(), 2, "空文件应该被丢掉");
        assert_eq!(collection.mft_size_count, 1, "只有带 $DATA 的非空记录算 MFT 来源");
        assert_eq!(collection.metadata_fallback_count, 1, "只有缺 $DATA 的记录才回退");
        let sizes: Vec<u64> = collection.records.iter().map(|item| item.size).collect();
        assert!(sizes.contains(&4096) && sizes.contains(&128), "大小不对: {sizes:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One-step assembly must be exactly identical to "assemble the path first, then normalize the whole thing".
    ///
    /// This is the equivalence basis for merging three path constructions into one: paths are shown directly to the user
    /// and are also used for stat fallback; a single wrong character would turn into "file does not exist".
    #[test]
    fn join_normalized_path_matches_the_two_step_version() {
        let cases = [
            ("c:\\users\\evan", "Report.TXT"),
            ("c:\\", "A.BIN"),
            ("c:\\users\\张三\\appdata\\local", "中文文件.LOG"),
            ("d:\\program files\\app", "MixedCase.Dll"),
            ("c:\\users\\Ömer", "Ärger.TXT"),
        ];
        for (raw_parent, name) in cases {
            // Old chain: raw parent path + name -> assemble -> normalize the whole thing
            let joined = if raw_parent.ends_with('\\') {
                format!("{raw_parent}{name}")
            } else {
                format!("{raw_parent}\\{name}")
            };
            let legacy = normalize_path(&joined);
            // New chain: normalize the parent path first (that is what the directory index stores) -> normalize in place while assembling
            let normalized_parent = normalize_path(raw_parent);
            assert_eq!(
                join_normalized_path(&normalized_parent, name),
                legacy,
                "parent={raw_parent} name={name}"
            );
        }
    }

    /// Directory index paths and depth: depth now accumulates level by level along BFS (no longer counting separators in strings),
    /// so it must be verified to match "how many segments are in the path" exactly - the frontend aggregates by depth level,
    /// and one level off would attribute a directory's usage to its parent.
    #[test]
    fn build_directory_index_matches_path_segments() {
        let entry = |mft_id: u64, parent_id: u64, name: &str, is_dir: bool| RawEntry {
            mft_id,
            parent_id,
            name: name.to_string(),
            is_dir,
            size: 0,
            modified: 0,
            has_data_stream: is_dir,
        };
        let entries = vec![
            entry(ROOT_MFT_ID, ROOT_MFT_ID, ".", true),
            entry(26, ROOT_MFT_ID, "Users", true),
            entry(30, 26, "Evan", true),
            entry(31, 30, "Desktop", true),
            entry(40, 31, "report.txt", false),
            // Parent not in the table (unreachable from root): no path can be built for the whole branch
            entry(50, 999, "orphan", true),
            entry(51, 50, "inside", true),
        ];

        let index = build_directory_index(&entries, "C:\\");
        assert_eq!(index[&ROOT_MFT_ID].depth, 0);
        assert_eq!(index[&26].path, "c:/users");
        assert_eq!(index[&26].depth, 1);
        assert_eq!(index[&30].depth, 2);
        assert_eq!(index[&31].depth, 3);
        assert!(
            !index.contains_key(&50) && !index.contains_key(&51),
            "不可达目录不该进索引"
        );

        // File paths are assembled from "parent path + filename"; the parent is already in the index
        let files = collect_file_records(&entries, &index);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "c:/users/evan/desktop/report.txt");
    }
}
