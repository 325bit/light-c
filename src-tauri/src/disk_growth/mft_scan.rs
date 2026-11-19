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
use std::fs;
use std::time::{Instant, UNIX_EPOCH};

use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::GetDriveTypeW;
use windows::Win32::System::WindowsProgramming::DRIVE_FIXED;

use crate::scanner::big_files_engine::mft_core;

const DEFAULT_MAX_DEPTH: u8 = 4;
const METADATA_PROGRESS_STEP: usize = 10_000;
/// Record number of the volume root directory. The index synthesizes the root node itself, so only
/// the test fixtures need to name it.
#[cfg(test)]
const ROOT_MFT_ID: u64 = 5;

// M13 fix: the cancel flag used to be a module-local `static DISK_GROWTH_SCAN_CANCELLED`;
// it now lives in `crate::scan_cancel` alongside the other three scanners' flags.
fn ensure_not_cancelled() -> Result<(), String> {
    crate::scan_cancel::check(crate::scan_cancel::ScanKind::DiskGrowth)
}

/// A single MFT record. This directly uses the parse result from `mft_core` - one sequential read yields
/// the name, parent-child relationship, size, and time; no separate enumeration or on-demand seek of FILE records is needed.
#[cfg(test)]
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
    /// Directory **node id** inside the scan's `VolumeIndex`, not an MFT record number.
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
        if crate::scan_cancel::is_cancelled(crate::scan_cancel::ScanKind::DiskGrowth) {
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
    // One dense index replaces the three ad-hoc structures used before (a `HashMap<u64, Vec<u32>>`
    // of children, a `HashMap<u64, DirectoryInfo>` of paths and depths, and the per-file ancestor
    // walk). Building it copies every name the index needs, so the record set can go immediately —
    // earlier than the old code, which had to keep it alive until all paths existed.
    let volume_index = mft_core::VolumeIndex::build(&raw_entries);
    drop(raw_entries);
    ensure_not_cancelled()?;
    // Directory paths are still assembled in their final form (forward slashes + lowercase) because
    // they are snapshot keys; `build_directory_paths` walks the index's breadth-first order.
    let directory_paths = build_directory_paths(&volume_index, &drive_root);
    ensure_not_cancelled()?;
    let file_records = collect_file_records(&volume_index, &directory_paths);
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
    let entries = aggregate_directories(
        &file_size_collection.records,
        &volume_index,
        &directory_paths,
        max_depth,
    );
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
    // SAFETY: wide is a NUL-terminated wide string and lives for the whole call
    let drive_type = unsafe { GetDriveTypeW(PCWSTR::from_raw(wide.as_ptr())) };
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

/// Assemble one path per directory node, in the index's breadth-first order (parents before children).
///
/// The path form is a **snapshot key** — forward slashes, all lowercase — so it still goes through
/// `join_normalized_path` from an already-normalized parent, byte for byte what the previous
/// `HashMap`-based BFS produced. What changed is only where the traversal comes from: the index
/// already knows the order, the depth and the parent, so this loop has no queue, no visited set and
/// no hash lookups.
fn build_directory_paths(index: &mft_core::VolumeIndex, drive_root: &str) -> Vec<String> {
    let mut paths = vec![String::new(); index.directory_count() as usize];
    paths[index.root() as usize] = normalize_path(drive_root);

    for &node in index.bfs_order().iter().skip(1) {
        let parent = index
            .parent_of(node)
            .expect("bfs order holds only directories reachable from the root");
        let path = join_normalized_path(&paths[parent as usize], index.name_of(node));
        paths[node as usize] = path;
    }

    paths
}

fn collect_file_records(
    index: &mft_core::VolumeIndex,
    directory_paths: &[String],
) -> Vec<FileRecord> {
    // Reserve capacity up front by node count: relying on collect's automatic growth would repeatedly move a tens-of-MB array several times
    let mut records = Vec::with_capacity(index.node_count() as usize);
    for file in index.files() {
        // A parent of `None` means the branch never reaches the root, so no path can be built for it.
        let Some(parent) = file.parent else {
            continue;
        };
        // A parsed $DATA stream with size 0 means a genuinely empty file. The size stage discards those
        // before counting them, so the path string would be allocated only to be thrown away.
        // Files without $DATA must stay: their size is unknown until a stat returns.
        if file.has_data_stream && file.size == 0 {
            continue;
        }
        records.push(FileRecord {
            // The parent path is already in normalized form (lowercase + forward slashes)
            path: join_normalized_path(&directory_paths[parent as usize], file.name),
            // Node id, not mft id: the aggregation below indexes its totals array with it.
            parent_id: u64::from(parent),
            // Size and time were already obtained during the sequential $MFT read, so just carry them through
            size: file.size,
            // The index stores milliseconds (shared with the hotspot scanner); this record is in seconds
            modified: file.modified / 1000,
            has_data_stream: file.has_data_stream,
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
        // Only non-ASCII uppercase letters (such as Ö, Д) reach here.
        // Deliberately NOT converted to ASCII-only (L4): this is a snapshot key,
        // and it has to stay byte-identical to what `normalize_path` produced
        // when the existing snapshots were written.
        path = path.to_lowercase();
    }
    path
}

/// Split the records into "size already known from $MFT" and "needs one stat", then measure only the latter in parallel.
///
/// This stage reads no disk for ~94% of files: the size and modification time were parsed from the same
/// `$MFT` record. It used to run inside a dedicated 4-8 thread rayon pool and bump an `AtomicUsize` once
/// per file just to drive the progress event — a million cross-core read-modify-writes on one cache line
/// cost more than the loop itself. The split is now a plain sequential pass; only the ~6% of records
/// without `$DATA` go through rayon, because those stats are scattered random IO.
fn collect_file_sizes<F>(
    file_records: Vec<FileRecord>,
    progress: &F,
    scan_start: &Instant,
) -> FileSizeCollection
where
    F: Fn(DiskGrowthScanProgress) + Sync,
{
    let total = file_records.len();
    let mut records: Vec<FileSizeRecord> = Vec::with_capacity(total);
    let mut mft_size_count = 0usize;
    // Only these keep their path until the stat batch; the rest move straight into `records`.
    let mut fallback: Vec<FileRecord> = Vec::new();

    for (index, record) in file_records.into_iter().enumerate() {
        if crate::scan_cancel::is_cancelled(crate::scan_cancel::ScanKind::DiskGrowth) {
            continue;
        }
        let current = index + 1;
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

        if record.has_data_stream {
            // Empty files (including directory placeholders) are not part of the results, so they must not
            // count toward the "size source" statistics either.
            if record.size == 0 {
                continue;
            }
            mft_size_count += 1;
            records.push(FileSizeRecord {
                parent_id: record.parent_id,
                // collect_file_records already produced normalized paths; no need to process again here
                path: record.path,
                size: record.size,
                modified: record.modified,
            });
        } else {
            fallback.push(record);
        }
    }

    // Fallback records are appended after the $MFT-sourced ones instead of keeping their original position:
    // nothing downstream depends on file order (aggregation is a sum, and the shard reader filters by parent).
    // Most records carry their size in the MFT; skip building a pool when there is nothing to stat.
    let measured = if fallback.is_empty() {
        Vec::new()
    } else {
        let stat_fallback = || {
            fallback
                .into_par_iter()
                .map(|record| {
                    if crate::scan_cancel::is_cancelled(crate::scan_cancel::ScanKind::DiskGrowth) {
                        return None;
                    }
                    let metadata = fs::metadata(&record.path).ok()?;
                    let size = metadata.len();
                    if size == 0 {
                        return None;
                    }
                    let modified = metadata
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                        .map(|duration| duration.as_secs() as i64)
                        .unwrap_or(0);
                    Some(FileSizeRecord {
                        parent_id: record.parent_id,
                        path: record.path,
                        size,
                        modified,
                    })
                })
                .collect::<Vec<_>>()
        };
        match ThreadPoolBuilder::new()
            .num_threads(metadata_thread_count())
            .build()
        {
            Ok(pool) => pool.install(stat_fallback),
            Err(_) => stat_fallback(),
        }
    };

    let mut metadata_fallback_count = 0usize;
    for record in measured.into_iter().flatten() {
        metadata_fallback_count += 1;
        records.push(record);
    }

    FileSizeCollection {
        records,
        mft_size_count,
        metadata_fallback_count,
    }
}

/// Aggregate file sizes into directory totals, bottom-up.
///
/// The previous implementation walked **from every file up to the root**, adding the size at each
/// ancestor that sat within `max_depth`. Two problems: on a 1M-file volume with an average depth of 8-10
/// that is 8-10 hash lookups plus 8-10 updates per file, and the loop itself was not bounded by
/// `max_depth` — deep files paid the full depth although only their first four levels were kept.
///
/// Now every file touches its direct parent once and the index hands each directory's total to its
/// parent, deepest level first: one write per file and one per directory, no hashing at all.
fn aggregate_directories(
    file_sizes: &[FileSizeRecord],
    index: &mft_core::VolumeIndex,
    directory_paths: &[String],
    max_depth: u8,
) -> Vec<DirSizeEntry> {
    let mut totals = index.empty_totals();

    for record in file_sizes {
        // `collect_file_records` already dropped files whose branch never reaches the root, so every
        // parent id here is a live directory node; the lookup form is kept anyway because a panic in
        // the middle of a full-disk scan costs the user the whole scan.
        let Some(total) = totals.get_mut(record.parent_id as usize) else {
            continue;
        };
        // A directory's own mtime on Windows does not necessarily reflect child file changes; aggregate
        // the newest file modification time within the subtree instead, closer to the "change time"
        // users care about. These records carry seconds, the shared aggregation works in milliseconds.
        total.add_file(record.size, record.modified.saturating_mul(1000));
    }

    // Reverse breadth-first order is a valid reverse topological order of the tree, so by the time a
    // directory is visited every one of its children already carries its complete subtree total.
    index.propagate_totals(&mut totals);

    let mut entries: Vec<DirSizeEntry> = index
        .bfs_order()
        .iter()
        .filter_map(|&node| {
            let totals = totals[node as usize];
            // Only directories that actually hold files are reported, exactly like when the stats map
            // was the source: it only ever gained an entry through a file.
            if totals.is_empty() {
                return None;
            }
            let depth = index.depth_of(node)?;
            if depth > u16::from(max_depth) {
                return None;
            }
            Some(DirSizeEntry {
                depth: depth as u8,
                path: directory_paths[node as usize].clone(),
                size: totals.size,
                // Reported in seconds, the unit `DirSizeEntry` has always used.
                modified: totals.modified / 1000,
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
    // Unicode on purpose (L4): snapshot key. Must produce the same bytes as
    // `join_normalized_path`, and as the snapshots already on disk.
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
    use std::collections::HashMap;

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
        assert_eq!(
            collection.mft_size_count, 1,
            "只有带 $DATA 的非空记录算 MFT 来源"
        );
        assert_eq!(
            collection.metadata_fallback_count, 1,
            "只有缺 $DATA 的记录才回退"
        );
        let sizes: Vec<u64> = collection.records.iter().map(|item| item.size).collect();
        assert!(
            sizes.contains(&4096) && sizes.contains(&128),
            "大小不对: {sizes:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Empty files must be dropped before their path is allocated, but records whose size is still unknown
    /// must survive so the stat fallback can measure them. Dropping the wrong kind silently undercounts
    /// every directory above it, and the number looks plausible enough that nobody notices.
    #[test]
    fn collect_file_records_skips_empty_files_but_keeps_unknown_sizes() {
        let entry =
            |mft_id: u64, parent_id: u64, name: &str, is_dir: bool, size: u64, has_data: bool| {
                RawEntry {
                    mft_id,
                    parent_id,
                    name: name.to_string(),
                    is_dir,
                    size,
                    modified: 0,
                    has_data_stream: has_data,
                }
            };
        let entries = vec![
            entry(ROOT_MFT_ID, ROOT_MFT_ID, ".", true, 0, false),
            entry(26, ROOT_MFT_ID, "Users", true, 0, false),
            entry(40, 26, "nonempty.bin", false, 100, true),
            // Has $DATA and size 0: a genuinely empty file, dropped here
            entry(41, 26, "empty.bin", false, 0, true),
            // No $DATA: size unknown until a stat, must be kept
            entry(42, 26, "unknown.bin", false, 0, false),
        ];

        let index = mft_core::VolumeIndex::build(&entries);
        let directory_paths = build_directory_paths(&index, "C:\\");
        let records = collect_file_records(&index, &directory_paths);
        let paths: Vec<&str> = records.iter().map(|record| record.path.as_str()).collect();

        assert_eq!(paths, vec!["c:/users/nonempty.bin", "c:/users/unknown.bin"]);
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

    /// Directory paths and depth: depth is accumulated by the index's breadth-first walk (no longer
    /// counting separators in strings), so it must be verified to match "how many segments are in the
    /// path" exactly - the frontend aggregates by depth level, and one level off would attribute a
    /// directory's usage to its parent.
    #[test]
    fn build_directory_paths_match_path_segments() {
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

        let index = mft_core::VolumeIndex::build(&entries);
        let paths = build_directory_paths(&index, "C:\\");
        let node_of = |path: &str| -> u32 {
            paths
                .iter()
                .position(|candidate| candidate == path)
                .unwrap_or_else(|| panic!("没有为 {path} 建出路径: {paths:?}")) as u32
        };

        assert_eq!(paths[index.root() as usize], "c:/");
        assert_eq!(index.depth_of(node_of("c:/users")), Some(1));
        assert_eq!(index.depth_of(node_of("c:/users/evan")), Some(2));
        assert_eq!(index.depth_of(node_of("c:/users/evan/desktop")), Some(3));
        // Depth is exactly the number of path segments below the drive root.
        assert_eq!(
            u16::try_from("c:/users/evan/desktop".matches('/').count()).unwrap(),
            3
        );

        // The unreachable branch never got a path and never entered the order.
        assert_eq!(
            paths.iter().filter(|path| path.is_empty()).count(),
            2,
            "orphan 与 inside 都不该被建出路径"
        );
        assert!(!paths.iter().any(|path| path.contains("orphan")));

        // File paths are assembled from "parent path + filename"; the parent is already in the index
        let files = collect_file_records(&index, &paths);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "c:/users/evan/desktop/report.txt");
    }

    /// The previous per-file ancestor walk, kept only as the oracle for the equivalence test below.
    fn aggregate_directories_per_file_walk(
        file_sizes: &[FileSizeRecord],
        index: &mft_core::VolumeIndex,
        directory_paths: &[String],
        max_depth: u8,
    ) -> Vec<DirSizeEntry> {
        let mut size_by_dir: HashMap<u32, u64> = HashMap::new();
        let mut modified_by_dir: HashMap<u32, i64> = HashMap::new();

        for record in file_sizes {
            let mut current = record.parent_id as u32;
            let mut guard = 0usize;

            while let Some(depth) = index.depth_of(current) {
                if depth <= u16::from(max_depth) {
                    *size_by_dir.entry(current).or_insert(0) += record.size;
                    let modified = modified_by_dir.entry(current).or_insert(0);
                    if record.modified > *modified {
                        *modified = record.modified;
                    }
                }

                let Some(parent) = index.parent_of(current) else {
                    break;
                };
                current = parent;
                guard += 1;
                if guard > 128 {
                    break;
                }
            }
        }

        let mut entries: Vec<DirSizeEntry> = size_by_dir
            .into_iter()
            .filter_map(|(node, size)| {
                let depth = index.depth_of(node)?;
                if depth > u16::from(max_depth) {
                    return None;
                }
                Some(DirSizeEntry {
                    depth: depth as u8,
                    path: directory_paths[node as usize].clone(),
                    size,
                    modified: modified_by_dir.get(&node).copied().unwrap_or(0),
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

    /// Bottom-up propagation must produce exactly what the per-file ancestor walk produced.
    ///
    /// This is the gate for swapping the algorithm: these entries become the snapshot that the next scan is
    /// compared against, so one wrong byte turns into phantom growth in the user's UI. The fixture covers
    /// the places where the two implementations could drift: files deeper than `max_depth`, a branch
    /// unreachable from the root, an empty directory, and equal sizes that exercise the path tie-break.
    #[test]
    fn bottom_up_aggregation_matches_the_per_file_walk() {
        let entry =
            |mft_id: u64, parent_id: u64, name: &str, is_dir: bool, size: u64, has_data: bool| {
                RawEntry {
                    mft_id,
                    parent_id,
                    name: name.to_string(),
                    is_dir,
                    size,
                    modified: 0,
                    has_data_stream: has_data,
                }
            };
        let entries = vec![
            entry(ROOT_MFT_ID, ROOT_MFT_ID, ".", true, 0, false),
            entry(26, ROOT_MFT_ID, "Users", true, 0, false),
            entry(30, 26, "Evan", true, 0, false),
            entry(31, 30, "Desktop", true, 0, false),
            entry(32, 31, "Deep", true, 0, false),
            entry(33, 32, "Deeper", true, 0, false),
            entry(34, 33, "DeeperStill", true, 0, false),
            entry(35, 26, "Empty", true, 0, false),
            // Parent 999 is not an MFT record: the whole branch is unreachable from the root
            entry(50, 999, "orphan", true, 0, false),
            entry(40, 31, "report.txt", false, 100, true),
            // Depth 6: only its first four ancestor levels may be emitted
            entry(41, 34, "deep.bin", false, 700, true),
            entry(42, 30, "a.bin", false, 200, true),
            // Same size as a.bin: the tie-break must still be by path
            entry(43, 30, "b.bin", false, 200, true),
            entry(44, 50, "orphan.bin", false, 999, true),
        ];

        let index = mft_core::VolumeIndex::build(&entries);
        let directory_paths = build_directory_paths(&index, "C:\\");
        // Feed both implementations the same input, produced by the production path, with distinct
        // timestamps so a dropped `max` shows up as a diff.
        let collected = collect_file_records(&index, &directory_paths);
        let files: Vec<FileSizeRecord> = collected
            .iter()
            .enumerate()
            .map(|(offset, record)| FileSizeRecord {
                parent_id: record.parent_id,
                path: record.path.clone(),
                size: record.size,
                modified: 1_700_000_000 + offset as i64,
            })
            .collect();
        // `orphan.bin` sits under a branch that never reaches the root, so it must not even be here.
        assert!(
            !files
                .iter()
                .any(|record| record.path.ends_with("orphan.bin")),
            "不可达分支的文件不该进入聚合"
        );

        let legacy = aggregate_directories_per_file_walk(
            &files,
            &index,
            &directory_paths,
            DEFAULT_MAX_DEPTH,
        );
        let bottom_up = aggregate_directories(&files, &index, &directory_paths, DEFAULT_MAX_DEPTH);

        let describe = |entries: &[DirSizeEntry]| -> Vec<(String, u64, u8, i64)> {
            entries
                .iter()
                .map(|entry| (entry.path.clone(), entry.size, entry.depth, entry.modified))
                .collect()
        };
        assert_eq!(describe(&legacy), describe(&bottom_up));

        // Deep files must still roll up into the last emitted level, but not leak past the depth limit.
        assert!(bottom_up
            .iter()
            .all(|entry| entry.depth <= DEFAULT_MAX_DEPTH));
        let deep = bottom_up
            .iter()
            .find(|entry| entry.path == "c:/users/evan/desktop/deep")
            .expect("depth 4 directory should be emitted");
        assert_eq!(deep.size, 700, "文件深度超过上限时仍要汇总到第 4 层");
        let deepest_offset = collected
            .iter()
            .position(|record| record.path.ends_with("/deep.bin"))
            .expect("deep.bin 应该在记录里");
        assert_eq!(
            deep.modified,
            1_700_000_000 + deepest_offset as i64,
            "最深的文件时间要冒泡到祖先"
        );
    }
}
