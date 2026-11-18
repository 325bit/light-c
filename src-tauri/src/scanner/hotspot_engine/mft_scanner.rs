// ============================================================================
// Big-directory analysis MFT engine
// Reuses big_files_engine::mft_core's sequential read capability: one pass over $MFT yields name,
// parent-child relation and size together. Previously USN enumeration got names first, then $MFT was read again by id set for sizes,
// the same data passed through twice, and USN enumeration walked the whole table in kernel mode, much slower than sequential reads in user mode.
// ============================================================================

#![cfg(windows)]

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Instant, UNIX_EPOCH};

use log::info;
use rayon::prelude::*;

use crate::scanner::big_files_engine::mft_core;
use crate::scanner::hotspot::{is_hotspot_scan_cancelled, FolderStats};

/// Fixed record number of the volume root in NTFS: the first 16 MFT entries are metafiles and `$Root` is 5.
const ROOT_MFT_ID: u64 = 5;
/// Emit a progress event every this many records processed; emitting too densely would crowd the scan thread
const PROGRESS_STEP: usize = 100_000;

#[derive(Clone)]
struct DirectoryInfo {
    parent_id: u64,
    path: PathBuf,
    depth: u16,
}

#[derive(Clone, Copy, Default)]
struct DirectoryStats {
    total_size: u64,
    file_count: usize,
    last_modified: i64,
}

pub struct MftScanProgress {
    pub stage: &'static str,
    pub message: String,
    pub processed: usize,
    pub stage_elapsed_ms: u64,
    /// Key numbers for this stage (segments/reading/parsing, directory count, fallback stat count…).
    ///
    /// For UI only: hotspot scanning previously had only a stage name and elapsed time, so judging the bottleneck was pure guesswork —
    /// with this line one can tell at a glance whether the slow part is reading, parsing or aggregation.
    pub detail: String,
}

struct DirectoryIndex {
    directories: HashMap<u64, DirectoryInfo>,
    deepest_depth: u16,
}

pub fn scan_via_mft(
    drive_letter: char,
    progress_cb: impl Fn(MftScanProgress) + Sync,
) -> Result<HashMap<PathBuf, FolderStats>, String> {
    info!("[Hotspot-MFT] 开始扫描 {}:，单遍读取 $MFT", drive_letter);

    let records = read_all_records(drive_letter, &progress_cb)?;

    // ---------- Directory index ----------
    let stage_start = Instant::now();
    progress_cb(MftScanProgress {
        stage: "index",
        message: "正在建立目录父子索引".to_string(),
        processed: records.len(),
        stage_elapsed_ms: 0,
        detail: String::new(),
    });
    let directory_index = build_directory_index(&records, drive_letter);
    let directories = directory_index.directories;
    progress_cb(MftScanProgress {
        stage: "index",
        message: format!("目录索引完成：{} 个目录", directories.len()),
        processed: directories.len(),
        stage_elapsed_ms: stage_start.elapsed().as_millis() as u64,
        detail: format!("有效记录 {} 条", records.len()),
    });

    // ---------- Accumulate file sizes ----------
    let stage_start = Instant::now();
    let (mut stats_by_dir, fallback, file_count) =
        accumulate_file_stats(&records, &directories);
    let fallback_count = fallback.len();
    progress_cb(MftScanProgress {
        stage: "metadata",
        message: format!("正在读取 {} 个文件的大小", fallback_count),
        processed: 0,
        stage_elapsed_ms: 0,
        detail: String::new(),
    });
    read_fallback_sizes(&directories, &fallback, &mut stats_by_dir);
    progress_cb(MftScanProgress {
        stage: "metadata",
        message: format!("文件大小整理完成：{} 个文件", file_count),
        processed: fallback_count,
        stage_elapsed_ms: stage_start.elapsed().as_millis() as u64,
        detail: format!("其中 {} 个 $DATA 未解析，回退 stat", fallback_count),
    });

    // ---------- Aggregate upward ----------
    let stage_start = Instant::now();
    progress_cb(MftScanProgress {
        stage: "aggregate",
        message: "正在向上聚合目录大小".to_string(),
        processed: stats_by_dir.len(),
        stage_elapsed_ms: 0,
        detail: String::new(),
    });
    propagate_to_ancestors(&directories, directory_index.deepest_depth, &mut stats_by_dir);
    // The directory index is no longer useful now; move paths out directly instead of cloning again
    let folder_map = into_folder_map(directories, &mut stats_by_dir);
    progress_cb(MftScanProgress {
        stage: "aggregate",
        message: format!("目录聚合完成：{} 个目录", folder_map.len()),
        processed: folder_map.len(),
        stage_elapsed_ms: stage_start.elapsed().as_millis() as u64,
        detail: String::new(),
    });

    info!(
        "[Hotspot-MFT] 扫描完成：记录 {} 条，聚合目录 {} 个",
        records.len(),
        folder_map.len()
    );
    Ok(folder_map)
}

/// Convert the intermediate "directory id → stats" result into the external "path → stats".
///
/// Directories are done, move paths straight out; directories with no files are omitted (consistent with the old implementation).
fn into_folder_map(
    directories: HashMap<u64, DirectoryInfo>,
    stats_by_dir: &mut HashMap<u64, DirectoryStats>,
) -> HashMap<PathBuf, FolderStats> {
    directories
        .into_iter()
        .filter_map(|(directory_id, directory)| {
            let stats = stats_by_dir.remove(&directory_id)?;
            Some((
                directory.path,
                FolderStats {
                    total_size: stats.total_size,
                    file_count: stats.file_count,
                    last_modified: stats.last_modified,
                },
            ))
        })
        .collect()
}

/// Read `$MFT` once sequentially and return all valid records.
fn read_all_records(
    drive_letter: char,
    progress_cb: &(impl Fn(MftScanProgress) + Sync),
) -> Result<Vec<mft_core::MftRecordInfo>, String> {
    let stage_start = Instant::now();
    let metadata_reader = mft_core::NtfsFileMetadataReader::open(drive_letter)?;
    let (records, mft_stats) = metadata_reader.read_all_records(&|processed| {
        if is_hotspot_scan_cancelled() {
            return false;
        }
        if processed % PROGRESS_STEP == 0 {
            progress_cb(MftScanProgress {
                stage: "mft",
                message: format!("正在读取 NTFS 主文件表，已处理 {} 条记录", processed),
                processed,
                stage_elapsed_ms: stage_start.elapsed().as_millis() as u64,
                detail: String::new(),
            });
        }
        true
    })?;

    progress_cb(MftScanProgress {
        stage: "mft",
        message: format!("$MFT 读取完成：{} 条有效记录", records.len()),
        processed: records.len(),
        stage_elapsed_ms: stage_start.elapsed().as_millis() as u64,
        detail: format!(
            "MFT 分段 {} · 读盘 {:.1}s、解析 {:.1}s（同时进行）",
            mft_stats.fragments,
            mft_stats.io_ms as f64 / 1000.0,
            mft_stats.parse_ms as f64 / 1000.0,
        ),
    });

    Ok(records)
}

fn build_directory_index(
    entries: &[mft_core::MftRecordInfo],
    drive_letter: char,
) -> DirectoryIndex {
    let mut directories = HashMap::new();
    // Only directories appear in parent-child relations; files have no children
    let mut children: HashMap<u64, Vec<usize>> = HashMap::with_capacity(entries.len() / 6);
    let mut deepest_depth = 0u16;

    for (index, entry) in entries.iter().enumerate() {
        if entry.is_dir && entry.mft_id != 0 {
            children.entry(entry.parent_id).or_default().push(index);
        }
    }

    let root_path = PathBuf::from(format!("{}:\\", drive_letter));
    directories.insert(
        ROOT_MFT_ID,
        DirectoryInfo {
            parent_id: 0,
            path: root_path,
            depth: 0,
        },
    );

    let mut queue = VecDeque::new();
    if let Some(root_children) = children.get(&ROOT_MFT_ID) {
        queue.extend(root_children.iter().copied());
    }

    while let Some(index) = queue.pop_front() {
        let entry = &entries[index];
        // Skip directories whose paths were already built: this also blocks infinite loops from abnormal parent chains
        if directories.contains_key(&entry.mft_id) {
            continue;
        }

        // A parent not in the index means the chain is unreachable from the root, so that whole branch cannot be built
        let Some(parent) = directories.get(&entry.parent_id) else {
            continue;
        };
        let path = parent.path.join(&entry.name);
        let depth = parent.depth.saturating_add(1);
        deepest_depth = deepest_depth.max(depth);
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

    DirectoryIndex {
        directories,
        deepest_depth,
    }
}

/// Record each file's size on its immediate parent directory; return (stats table, files needing fallback stat, total file count).
///
/// Records with `$DATA` use the size in the record; those without (about 6%) are collected first,
/// then handed to `read_fallback_sizes` for a parallel stat pass — skipping this makes directories undercount size.
fn accumulate_file_stats<'a>(
    entries: &'a [mft_core::MftRecordInfo],
    directories: &HashMap<u64, DirectoryInfo>,
) -> (
    HashMap<u64, DirectoryStats>,
    Vec<(u64, &'a str)>,
    usize,
) {
    let mut stats_by_dir: HashMap<u64, DirectoryStats> = HashMap::with_capacity(directories.len());
    let mut fallback: Vec<(u64, &str)> = Vec::new();
    let mut file_count = 0usize;

    for entry in entries {
        if entry.is_dir || entry.mft_id == 0 {
            continue;
        }
        // Files whose parent is not in the index (unreachable from root) are ignored, consistent with the previous rule
        if !directories.contains_key(&entry.parent_id) {
            continue;
        }
        file_count += 1;

        if entry.has_data_stream {
            if entry.size > 0 {
                add_file_stats(
                    &mut stats_by_dir,
                    entry.parent_id,
                    entry.size,
                    entry.modified.saturating_mul(1000),
                );
            }
        } else {
            fallback.push((entry.parent_id, entry.name.as_str()));
        }
    }

    (stats_by_dir, fallback, file_count)
}

/// Parallel fallback `fs::metadata` for files missing `$DATA`.
///
/// Such files number tens of thousands and are scattered; serial stat takes hundreds of ms, while parallel flattens the cost into IO.
fn read_fallback_sizes(
    directories: &HashMap<u64, DirectoryInfo>,
    fallback: &[(u64, &str)],
    stats_by_dir: &mut HashMap<u64, DirectoryStats>,
) {
    if fallback.is_empty() {
        return;
    }

    let measured: Vec<(u64, u64, i64)> = fallback
        .par_iter()
        .filter_map(|(parent_id, name)| {
            if is_hotspot_scan_cancelled() {
                return None;
            }
            let parent = directories.get(parent_id)?;
            let metadata = std::fs::metadata(parent.path.join(name)).ok()?;
            Some((
                *parent_id,
                metadata.len(),
                metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map(|duration| duration.as_millis() as i64)
                    .unwrap_or(0),
            ))
        })
        .collect();

    for (parent_id, size, modified_millis) in measured {
        if size > 0 {
            add_file_stats(stats_by_dir, parent_id, size, modified_millis);
        }
    }
}

/// Add upward from the deepest directory: by the time a level is processed, all its child directories have been aggregated.
fn propagate_to_ancestors(
    directories: &HashMap<u64, DirectoryInfo>,
    deepest_depth: u16,
    stats_by_dir: &mut HashMap<u64, DirectoryStats>,
) {
    let mut ids_by_depth = vec![Vec::<u64>::new(); deepest_depth as usize + 1];
    for (directory_id, directory) in directories {
        if let Some(bucket) = ids_by_depth.get_mut(directory.depth as usize) {
            bucket.push(*directory_id);
        }
    }

    for depth in (0..ids_by_depth.len()).rev() {
        for directory_id in &ids_by_depth[depth] {
            let Some(stats) = stats_by_dir.get(directory_id).copied() else {
                continue;
            };
            let Some(directory) = directories.get(directory_id) else {
                continue;
            };
            if directory.parent_id == 0 || directory.parent_id == *directory_id {
                continue;
            }
            if directories.contains_key(&directory.parent_id) {
                add_directory_stats(stats_by_dir, directory.parent_id, stats);
            }
        }
    }
}

fn add_file_stats(
    stats_by_dir: &mut HashMap<u64, DirectoryStats>,
    directory_id: u64,
    size: u64,
    modified_millis: i64,
) {
    let stats = stats_by_dir.entry(directory_id).or_default();
    stats.total_size = stats.total_size.saturating_add(size);
    stats.file_count += 1;
    // `FolderStats.last_modified` is externally specified in milliseconds; the caller has already converted, we just take the newest here
    if modified_millis > stats.last_modified {
        stats.last_modified = modified_millis;
    }
}

fn add_directory_stats(
    stats_by_dir: &mut HashMap<u64, DirectoryStats>,
    directory_id: u64,
    child_stats: DirectoryStats,
) {
    let stats = stats_by_dir.entry(directory_id).or_default();
    stats.total_size = stats.total_size.saturating_add(child_stats.total_size);
    stats.file_count += child_stats.file_count;
    if child_stats.last_modified > stats.last_modified {
        stats.last_modified = child_stats.last_modified;
    }
}

pub fn is_elevated() -> bool {
    mft_core::is_elevated()
}

pub fn is_ntfs(drive_letter: char) -> bool {
    mft_core::is_ntfs(drive_letter)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a record. Directories have no `$DATA`, matching the shape of real MFT records.
    fn record(
        mft_id: u64,
        parent_id: u64,
        name: &str,
        is_dir: bool,
        size: u64,
        has_data_stream: bool,
    ) -> mft_core::MftRecordInfo {
        mft_core::MftRecordInfo {
            mft_id,
            parent_id,
            name: name.to_string(),
            is_dir,
            size,
            // Fixed timestamp, convenient for asserting last_modified
            modified: 1_700_000_000,
            has_data_stream,
        }
    }

    /// Directory size must be added level by level, and only the part "reachable from the root" counts.
    ///
    /// This is the basis of the UI ranking: undercount one level and the directory size users see is wrong,
    /// and such an error raises no alarm — it only shows up as a smaller number.
    #[test]
    fn file_sizes_are_aggregated_up_to_the_root() {
        let entries = vec![
            record(ROOT_MFT_ID, ROOT_MFT_ID, ".", true, 0, false),
            record(26, ROOT_MFT_ID, "Users", true, 0, false),
            record(30, 26, "Evan", true, 0, false),
            record(40, 30, "a.bin", false, 100, true),
            record(41, 30, "b.bin", false, 200, true),
            // Empty file: usage 0, not counted in size, but still a file
            record(42, 30, "empty.bin", false, 0, true),
            record(50, 26, "Other", true, 0, false),
            record(51, 50, "c.bin", false, 300, true),
            // Parent not in index (unreachable from root): ignore the whole entry
            record(60, 999, "orphan.bin", false, 400, true),
            // File missing $DATA: do not stat this time (the table has no such file, stat would fail too),
            // only verify it is assigned to the fallback queue rather than treated as 0
            record(43, 30, "no-data.bin", false, 0, false),
        ];

        let index = build_directory_index(&entries, 'C');
        // root 0 → Users 1 → Evan/Other 2
        assert_eq!(index.deepest_depth, 2);

        let (mut stats_by_dir, fallback, file_count) =
            accumulate_file_stats(&entries, &index.directories);
        assert_eq!(file_count, 5, "a/b/empty/no-data/c 五个文件，orphan 不算");
        assert_eq!(fallback, vec![(30u64, "no-data.bin")]);

        propagate_to_ancestors(
            &index.directories,
            index.deepest_depth,
            &mut stats_by_dir,
        );
        let folder_map = into_folder_map(index.directories, &mut stats_by_dir);

        let evan = &folder_map[&PathBuf::from("C://Users//Evan")];
        assert_eq!(evan.total_size, 300);
        assert_eq!(evan.file_count, 2);
        assert_eq!(evan.last_modified, 1_700_000_000 * 1000);

        let other = &folder_map[&PathBuf::from("C://Users//Other")];
        assert_eq!(other.total_size, 300);

        // Parent directory is the total of "itself + all child directories"
        assert_eq!(folder_map[&PathBuf::from("C://Users")].total_size, 600);
        assert_eq!(folder_map[&PathBuf::from("C://")].total_size, 600);
        assert_eq!(folder_map[&PathBuf::from("C://")].file_count, 3);
    }
}
