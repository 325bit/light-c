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

/// A file whose `$MFT` record carried no `$DATA`: the only files that keep a full path, because the
/// fallback `stat` needs one.
struct FallbackFile {
    /// Directory **node id** inside the scan's `VolumeIndex`, not an MFT record number.
    parent: u32,
    path: String,
}

/// What the size passes measured, for the progress line and the response.
#[derive(Default)]
struct FileTally {
    /// Files with a non-zero size — the same set the shards and the totals contain.
    files: usize,
    total_size: u64,
    mft_size_count: usize,
    fallback_count: usize,
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

/// Scan one volume and report every file to `sink` as it is folded into the directory totals.
///
/// `sink` receives `(parent path, name, size, modified seconds)` for exactly the files that end up in
/// the snapshot, so the caller can stream them into the file-level shards. It exists because the
/// alternative — returning a vector of a million rows — cost a full path allocation per file plus a
/// second pass over the whole list just to write it out.
pub fn scan_drive_with_progress<F, S>(
    drive_letter: &str,
    progress: &F,
    sink: &mut S,
) -> Result<FullDiskScanResult, String>
where
    F: Fn(DiskGrowthScanProgress) + Sync,
    S: FnMut(&str, &str, u64, i64) -> Result<(), String>,
{
    let drive_letter = normalize_drive_letter(drive_letter)?;
    let root = format!("{}:\\", drive_letter);
    scan_disk_with_progress(&root, DEFAULT_MAX_DEPTH, progress, sink)
}

fn scan_disk_with_progress<F, S>(
    root: &str,
    max_depth: u8,
    progress: &F,
    sink: &mut S,
) -> Result<FullDiskScanResult, String>
where
    F: Fn(DiskGrowthScanProgress) + Sync,
    S: FnMut(&str, &str, u64, i64) -> Result<(), String>,
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
    push_phase_duration(&mut phase_durations, "path", phase_start);
    ensure_not_cancelled()?;

    emit_progress(progress, &start, "metadata", "正在读取文件大小", 0, None);
    let phase_start = Instant::now();
    let mut totals = volume_index.empty_totals();
    let (mut tally, fallback) = accumulate_file_sizes(
        &volume_index,
        &directory_paths,
        &mut totals,
        sink,
        progress,
        &start,
    )?;
    measure_fallback_files(fallback, &directory_paths, &mut totals, sink, &mut tally)?;
    push_phase_duration(&mut phase_durations, "metadata", phase_start);
    ensure_not_cancelled()?;

    emit_progress(
        progress,
        &start,
        "aggregate",
        "正在聚合目录大小",
        tally.files,
        None,
    );
    let phase_start = Instant::now();
    let entries = build_dir_size_entries(&volume_index, &directory_paths, &mut totals, max_depth);
    push_phase_duration(&mut phase_durations, "aggregate", phase_start);
    ensure_not_cancelled()?;

    Ok(FullDiskScanResult {
        entries,
        total_size: tally.total_size,
        total_files_scanned: tally.files,
        scan_duration_ms: start.elapsed().as_millis() as u64,
        root_path: drive_root,
        backend: "mft".to_string(),
        phase_durations,
        mft_size_count: tally.mft_size_count,
        metadata_fallback_count: tally.fallback_count,
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

/// Fold every file into its directory's totals, reporting each one to `sink` in the same walk.
///
/// This replaces the previous three-step shape: build a `Vec` holding a full path per file, walk it
/// to fold sizes, walk it again to split each path back into parent + name for the shards. On a
/// 1.2M-record volume that vector was ~150MB and had to stay alive through the whole size and
/// aggregate phase. Now a file is folded and reported in one iteration, and a full path is built
/// only for the ~6% without `$DATA`, which the `stat` fallback needs.
///
/// Returns the files that still need a `stat`, so the caller can measure them and fold those in.
fn accumulate_file_sizes<S>(
    index: &mft_core::VolumeIndex,
    directory_paths: &[String],
    totals: &mut [mft_core::DirectoryTotals],
    sink: &mut S,
    progress: &(impl Fn(DiskGrowthScanProgress) + Sync),
    scan_start: &Instant,
) -> Result<(FileTally, Vec<FallbackFile>), String>
where
    S: FnMut(&str, &str, u64, i64) -> Result<(), String>,
{
    let total_files = (index.node_count() - index.directory_count()) as usize;
    let mut tally = FileTally::default();
    // Only the stat fallback keeps a path, so this stays small even on a million-file volume.
    let mut fallback: Vec<FallbackFile> = Vec::new();
    // One buffer for the lowercased name, reused for every file: allocating per file would put a
    // million short-lived strings through the allocator for no reason.
    let mut name_buffer = String::with_capacity(64);
    let mut visited = 0usize;

    for file in index.files() {
        visited += 1;
        if visited.is_multiple_of(METADATA_PROGRESS_STEP) || visited == total_files {
            emit_progress(
                progress,
                scan_start,
                "metadata",
                "正在整理文件大小",
                visited,
                Some(total_files),
            );
        }
        if crate::scan_cancel::is_cancelled(crate::scan_cancel::ScanKind::DiskGrowth) {
            continue;
        }
        // A parent of `None` means the branch never reaches the root, so no path can be built and no
        // directory may count it.
        let Some(parent) = file.parent else {
            continue;
        };

        if !file.has_data_stream {
            // Size unknown until a stat returns; this is the only path string the pipeline keeps.
            fallback.push(FallbackFile {
                parent,
                path: join_normalized_path(&directory_paths[parent as usize], file.name),
            });
            continue;
        }
        // A parsed $DATA stream with size 0 is a genuinely empty file: not part of the results, not
        // in the shards, and not worth a `stat`.
        if file.size == 0 {
            continue;
        }

        if let Some(total) = totals.get_mut(parent as usize) {
            // The index already holds milliseconds, which is the unit the shared aggregation uses.
            total.add_file(file.size, file.modified);
        }
        let name = normalized_name(&mut name_buffer, file.name);
        sink(
            trimmed_parent_path(&directory_paths[parent as usize]),
            name,
            file.size,
            file.modified / 1000,
        )?;
        tally.files += 1;
        tally.total_size = tally.total_size.saturating_add(file.size);
        tally.mft_size_count += 1;
    }

    Ok((tally, fallback))
}

/// Parallel `fs::metadata` for the files whose `$MFT` record carried no `$DATA`.
///
/// Those stats are scattered random I/O, so they go through rayon; the fold back into the totals
/// happens afterwards on this thread, because it needs `&mut` on the totals while the pool reads.
fn measure_fallback_files<S>(
    fallback: Vec<FallbackFile>,
    directory_paths: &[String],
    totals: &mut [mft_core::DirectoryTotals],
    sink: &mut S,
    tally: &mut FileTally,
) -> Result<(), String>
where
    S: FnMut(&str, &str, u64, i64) -> Result<(), String>,
{
    if fallback.is_empty() {
        return Ok(());
    }

    let stat_fallback = || {
        fallback
            .into_par_iter()
            .filter_map(|file| {
                if crate::scan_cancel::is_cancelled(crate::scan_cancel::ScanKind::DiskGrowth) {
                    return None;
                }
                let metadata = fs::metadata(&file.path).ok()?;
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
                Some((file, size, modified))
            })
            .collect::<Vec<_>>()
    };
    let measured = match ThreadPoolBuilder::new()
        .num_threads(metadata_thread_count())
        .build()
    {
        Ok(pool) => pool.install(stat_fallback),
        Err(_) => stat_fallback(),
    };

    for (file, size, modified) in measured {
        if let Some(total) = totals.get_mut(file.parent as usize) {
            total.add_file(size, modified.saturating_mul(1000));
        }
        let name = file
            .path
            .rsplit_once('/')
            .map(|(_, name)| name)
            .unwrap_or(&file.path);
        sink(
            trimmed_parent_path(&directory_paths[file.parent as usize]),
            name,
            size,
            modified,
        )?;
        tally.files += 1;
        tally.total_size = tally.total_size.saturating_add(size);
        tally.fallback_count += 1;
    }

    Ok(())
}

/// Hand each directory's subtree total to its parent, then emit the levels the UI keeps.
///
/// Reverse breadth-first order is a valid reverse topological order of the tree, so by the time a
/// directory is visited every one of its children already carries its complete subtree total.
fn build_dir_size_entries(
    index: &mft_core::VolumeIndex,
    directory_paths: &[String],
    totals: &mut [mft_core::DirectoryTotals],
    max_depth: u8,
) -> Vec<DirSizeEntry> {
    index.propagate_totals(totals);

    let mut entries: Vec<DirSizeEntry> = index
        .bfs_order()
        .iter()
        .filter_map(|&node| {
            let total = totals[node as usize];
            // Only directories that actually hold files are reported, exactly like when the stats map
            // was the source: it only ever gained an entry through a file.
            if total.is_empty() {
                return None;
            }
            let depth = index.depth_of(node)?;
            if depth > u16::from(max_depth) {
                return None;
            }
            Some(DirSizeEntry {
                depth: depth as u8,
                path: directory_paths[node as usize].clone(),
                size: total.size,
                // Reported in seconds, the unit `DirSizeEntry` has always used.
                modified: total.modified / 1000,
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

/// A directory's key as the shards store it: no trailing separator, so `"{parent}/{name}"` rebuilds
/// the full path without a doubled slash. Only the drive root has one.
fn trimmed_parent_path(path: &str) -> &str {
    path.trim_end_matches('/')
}

/// Lowercase a file name into `buffer`, matching the name `join_normalized_path` would have produced.
///
/// The shard stores the snapshot-key form. Writing the original case would leave every detail query
/// looking for a lowercase prefix and silently hiding the row.
fn normalized_name<'a>(buffer: &'a mut String, name: &str) -> &'a str {
    buffer.clear();
    buffer.push_str(name);
    buffer.make_ascii_lowercase();
    if buffer.chars().any(char::is_uppercase) {
        // Same non-ASCII fallback as `join_normalized_path` (L4: the key must stay byte-identical).
        let lowered = buffer.to_lowercase();
        buffer.clear();
        buffer.push_str(&lowered);
    }
    buffer.as_str()
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

    /// Where a file's size comes from: `$DATA` in the record, or a `stat` when the record had none.
    ///
    /// The two paths have to stay disjoint — a file folded twice inflates its directory, a file
    /// dropped from both silently shrinks it, and neither raises an error.
    #[test]
    fn sizes_come_from_the_record_unless_data_is_missing() {
        let dir = std::env::temp_dir().join("lightc-disk-growth-sizes");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("payload.bin"), vec![7u8; 128]).unwrap();

        let file = |mft_id: u64, name: &str, size: u64, has_data: bool| RawEntry {
            mft_id,
            parent_id: ROOT_MFT_ID,
            name: name.to_string(),
            is_dir: false,
            size,
            modified: 1_700_000_000,
            has_data_stream: has_data,
        };
        // The temp directory stands in for the volume root, so the fallback `stat` resolves a real file.
        let entries = vec![
            // Size is right in the record: no disk access at all
            file(40, "from-mft.bin", 4096, true),
            // No $DATA: the only kind that gets a path and a stat
            file(41, "payload.bin", 0, false),
            // Has $DATA and size 0: a genuinely empty file, dropped before either path
            file(42, "empty.bin", 0, true),
        ];

        let index = mft_core::VolumeIndex::build(&entries);
        let root = dir.to_string_lossy().to_string();
        let directory_paths = build_directory_paths(&index, &root);
        let mut totals = index.empty_totals();
        let mut reported: Vec<(String, u64, i64)> = Vec::new();
        let started = Instant::now();

        let (mut tally, fallback) = accumulate_file_sizes(
            &index,
            &directory_paths,
            &mut totals,
            &mut |_parent: &str, name: &str, size: u64, modified: i64| {
                reported.push((name.to_string(), size, modified));
                Ok(())
            },
            &|_| {},
            &started,
        )
        .unwrap();

        assert_eq!(fallback.len(), 1, "只有缺 $DATA 的文件才回退 stat");
        assert_eq!(
            reported.len(),
            1,
            "空文件不参与统计，缺 $DATA 的文件要等 stat 之后才上报"
        );
        assert_eq!(reported[0].0, "from-mft.bin");
        assert_eq!(tally.mft_size_count, 1);
        assert_eq!(tally.files, 1);

        measure_fallback_files(
            fallback,
            &directory_paths,
            &mut totals,
            &mut |_parent: &str, name: &str, size: u64, modified: i64| {
                reported.push((name.to_string(), size, modified));
                Ok(())
            },
            &mut tally,
        )
        .unwrap();

        assert_eq!(tally.fallback_count, 1);
        assert_eq!(tally.files, 2, "空文件从头到尾都不该出现");
        assert_eq!(tally.total_size, 4096 + 128);
        assert_eq!(reported.len(), 2);
        assert_eq!(reported[1].0, "payload.bin");
        assert_eq!(reported[1].1, 128, "回退 stat 读到的是真实大小");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file without `$DATA` keeps a path because the fallback `stat` needs one; an empty file must
    /// not, or the pipeline allocates a path per empty file just to throw it away.
    #[test]
    fn only_files_needing_a_stat_keep_a_full_path() {
        let directory = |mft_id: u64, parent_id: u64, name: &str| RawEntry {
            mft_id,
            parent_id,
            name: name.to_string(),
            is_dir: true,
            size: 0,
            modified: 0,
            has_data_stream: false,
        };
        let file = |mft_id: u64, parent_id: u64, name: &str, size: u64, has_data: bool| RawEntry {
            mft_id,
            parent_id,
            name: name.to_string(),
            is_dir: false,
            size,
            modified: 0,
            has_data_stream: has_data,
        };
        let entries = vec![
            directory(26, ROOT_MFT_ID, "Users"),
            file(40, 26, "nonempty.bin", 100, true),
            // Has $DATA and size 0: a genuinely empty file, dropped here
            file(41, 26, "empty.bin", 0, true),
            // No $DATA: size unknown until a stat, must be kept
            file(42, 26, "unknown.bin", 0, false),
        ];

        let index = mft_core::VolumeIndex::build(&entries);
        let directory_paths = build_directory_paths(&index, "C:\\");
        let mut totals = index.empty_totals();
        let mut reported: Vec<String> = Vec::new();
        let started = Instant::now();

        let (tally, fallback) = accumulate_file_sizes(
            &index,
            &directory_paths,
            &mut totals,
            &mut |_parent: &str, name: &str, _size: u64, _modified: i64| {
                reported.push(name.to_string());
                Ok(())
            },
            &|_| {},
            &started,
        )
        .unwrap();

        assert_eq!(
            reported,
            vec!["nonempty.bin"],
            "上报给分片的只有真正统计到的文件"
        );
        assert_eq!(fallback.len(), 1);
        assert_eq!(fallback[0].path, "c:/users/unknown.bin");
        assert_eq!(tally.files, 1);
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

        // A file's path is still assembled from "parent path + filename", and only files whose size
        // needs a stat get one. `report.txt` has no `$DATA` in this fixture, so it lands in the
        // fallback queue with exactly that path.
        let mut totals = index.empty_totals();
        let (_, fallback) = accumulate_file_sizes(
            &index,
            &paths,
            &mut totals,
            &mut |_parent: &str, _name: &str, _size: u64, _modified: i64| Ok(()),
            &|_| {},
            &Instant::now(),
        )
        .unwrap();
        assert_eq!(fallback.len(), 1);
        assert_eq!(fallback[0].path, "c:/users/evan/desktop/report.txt");
    }

    /// The previous per-file ancestor walk, kept only as the oracle for the equivalence test below.
    ///
    /// Takes the same `(parent node, size, modified seconds)` rows the sink receives, so the two
    /// implementations are fed identical input.
    fn aggregate_directories_per_file_walk(
        file_sizes: &[(u32, u64, i64)],
        index: &mft_core::VolumeIndex,
        directory_paths: &[String],
        max_depth: u8,
    ) -> Vec<DirSizeEntry> {
        let mut size_by_dir: HashMap<u32, u64> = HashMap::new();
        let mut modified_by_dir: HashMap<u32, i64> = HashMap::new();

        for (parent, size, modified_at) in file_sizes {
            let mut current = *parent;
            let mut guard = 0usize;

            while let Some(depth) = index.depth_of(current) {
                if depth <= u16::from(max_depth) {
                    *size_by_dir.entry(current).or_insert(0) += size;
                    let modified = modified_by_dir.entry(current).or_insert(0);
                    if *modified_at > *modified {
                        *modified = *modified_at;
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
        let mut entries = vec![
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
        // Distinct timestamps per file, so a dropped `max` shows up as a diff instead of blending in.
        for record in entries.iter_mut().filter(|record| !record.is_dir) {
            record.modified = 1_700_000_000 + record.mft_id as i64;
        }

        let index = mft_core::VolumeIndex::build(&entries);
        let directory_paths = build_directory_paths(&index, "C:\\");

        // Feed the oracle exactly what the production path reports to the shard sink.
        let mut totals = index.empty_totals();
        let mut reported: Vec<(u32, u64, i64)> = Vec::new();
        let (tally, fallback) = accumulate_file_sizes(
            &index,
            &directory_paths,
            &mut totals,
            &mut |parent: &str, _name: &str, size: u64, modified: i64| {
                let parent_key = parent.trim_end_matches('/');
                let node = directory_paths
                    .iter()
                    .position(|path| path.trim_end_matches('/') == parent_key)
                    .expect("上报的父路径必须来自目录索引");
                reported.push((node as u32, size, modified));
                Ok(())
            },
            &|_| {},
            &Instant::now(),
        )
        .unwrap();

        assert!(fallback.is_empty(), "fixture 里每个文件都带 $DATA");
        // `orphan.bin` sits under a branch that never reaches the root, so it must not even be here.
        assert_eq!(reported.len(), 4, "不可达分支的文件不该进入聚合");
        assert_eq!(tally.total_size, 100 + 700 + 200 + 200);

        let legacy = aggregate_directories_per_file_walk(
            &reported,
            &index,
            &directory_paths,
            DEFAULT_MAX_DEPTH,
        );
        let bottom_up =
            build_dir_size_entries(&index, &directory_paths, &mut totals, DEFAULT_MAX_DEPTH);

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
        // `deep.bin` is record 41, so the fixture gave it 1_700_000_041.
        assert_eq!(deep.modified, 1_700_000_041, "最深的文件时间要冒泡到祖先");
    }
}
