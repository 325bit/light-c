// ============================================================================
// Big-directory analysis MFT engine
// Reuses big_files_engine::mft_core's sequential read capability: one pass over $MFT yields name,
// parent-child relation and size together. Previously USN enumeration got names first, then $MFT was read again by id set for sizes,
// the same data passed through twice, and USN enumeration walked the whole table in kernel mode, much slower than sequential reads in user mode.
// ============================================================================

#![cfg(windows)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Instant, UNIX_EPOCH};

use log::info;
use rayon::prelude::*;

use crate::scan_cancel::{is_cancelled, ScanKind};
use crate::scanner::big_files_engine::mft_core;
use crate::scanner::hotspot::FolderStats;

/// Emit a progress event every this many records processed; emitting too densely would crowd the scan thread
const PROGRESS_STEP: usize = 100_000;

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

pub fn scan_via_mft(
    drive_letter: char,
    progress_cb: impl Fn(MftScanProgress) + Sync,
) -> Result<HashMap<PathBuf, FolderStats>, String> {
    info!("[Hotspot-MFT] 开始扫描 {}:，单遍读取 $MFT", drive_letter);

    let records = read_all_records(drive_letter, &progress_cb)?;
    let record_count = records.len();

    // ---------- Directory index ----------
    let stage_start = Instant::now();
    progress_cb(MftScanProgress {
        stage: "index",
        message: "正在建立目录父子索引".to_string(),
        processed: record_count,
        stage_elapsed_ms: 0,
        detail: String::new(),
    });
    // The index owns a copy of every name, so the parsed records are dead the moment it exists. On a
    // 1.2M-record volume the names alone are ~110MB; disk_growth and mft_bigfiles release their copy
    // at the same point, and hotspot used to hold it through both the size and the aggregate stages.
    let index = mft_core::VolumeIndex::build(&records);
    // Reachable directories only — the same set the old BFS index could walk to.
    let reachable_directories = index.bfs_order().len();
    let paths = build_directory_paths(&index, drive_letter);
    drop(records);
    progress_cb(MftScanProgress {
        stage: "index",
        message: format!("目录索引完成：{} 个目录", reachable_directories),
        processed: reachable_directories,
        stage_elapsed_ms: stage_start.elapsed().as_millis() as u64,
        detail: format!("有效记录 {} 条", record_count),
    });

    // ---------- Accumulate file sizes ----------
    let stage_start = Instant::now();
    let mut totals = index.empty_totals();
    // Files whose $DATA was parsed. Files without it are collected first and stat'ed in parallel,
    // because skipping them would undercount every directory they sit in.
    index.accumulate_files(&mut totals);
    let (fallback, file_count) = collect_fallback_files(&index);
    let fallback_count = fallback.len();
    progress_cb(MftScanProgress {
        stage: "metadata",
        message: format!("正在读取 {} 个文件的大小", fallback_count),
        processed: 0,
        stage_elapsed_ms: 0,
        detail: String::new(),
    });
    read_fallback_sizes(&paths, &fallback, &mut totals);
    // Directories that already hold a file total, i.e. what the old `stats_by_dir.len()` reported.
    // Counted after the fallback pass so the number matches what the user saw before.
    let seeded_directories = totals.iter().filter(|stats| !stats.is_empty()).count();
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
        processed: seeded_directories,
        stage_elapsed_ms: 0,
        detail: String::new(),
    });
    index.propagate_totals(&mut totals);
    let folder_map = into_folder_map(&index, paths, &totals);
    progress_cb(MftScanProgress {
        stage: "aggregate",
        message: format!("目录聚合完成：{} 个目录", folder_map.len()),
        processed: folder_map.len(),
        stage_elapsed_ms: stage_start.elapsed().as_millis() as u64,
        detail: String::new(),
    });

    info!(
        "[Hotspot-MFT] 扫描完成：记录 {} 条，聚合目录 {} 个",
        record_count,
        folder_map.len()
    );
    Ok(folder_map)
}

/// Build one native path per directory node.
///
/// Hotspot reports `PathBuf` (native separators, original case) while disk_growth reports normalized
/// snapshot keys, so the two cannot share the string form — only the traversal. Walking `bfs_order`
/// guarantees a parent's path exists before its children ask for it. Unreachable directories (a branch
/// that never connects to the root, a self-parent, a cycle) keep an empty path and are dropped by the
/// empty-totals filter below, which is the same set the old BFS index could not reach.
fn build_directory_paths(index: &mft_core::VolumeIndex, drive_letter: char) -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::new(); index.directory_count() as usize];
    let root = index.root();
    paths[root as usize] = PathBuf::from(format!("{}:\\", drive_letter));

    for &node in index.bfs_order() {
        if node == root {
            continue;
        }
        // `bfs_order` only holds directories that received a depth, so every one of them has a parent.
        let parent = index
            .parent_of(node)
            .expect("bfs_order 只收录可达目录，必有其父节点");
        paths[node as usize] = paths[parent as usize].join(index.name_of(node));
    }

    paths
}

/// Files whose `$MFT` record carried no `$DATA`, paired with the parent node that must stat them.
///
/// Also returns the total number of files reachable from the root, which is what the progress line
/// reports; the old code counted it in the same pass, and it is not the same as "files with a size".
fn collect_fallback_files(index: &mft_core::VolumeIndex) -> (Vec<(u32, &str)>, usize) {
    let mut fallback = Vec::new();
    let mut file_count = 0usize;

    for entry in index.files() {
        let Some(parent) = entry.parent else {
            // Unreachable from the root: ignored entirely, same as the old `directories` lookup.
            continue;
        };
        file_count += 1;
        if !entry.has_data_stream {
            fallback.push((parent, entry.name));
        }
    }

    (fallback, file_count)
}

/// Parallel fallback `fs::metadata` for files missing `$DATA`.
///
/// Such files number tens of thousands and are scattered; serial stat takes hundreds of ms, while
/// parallel flattens the cost into IO. Results are folded in afterwards because the fold needs `&mut`
/// on the totals while the stat pass borrows the paths.
fn read_fallback_sizes(
    paths: &[PathBuf],
    fallback: &[(u32, &str)],
    totals: &mut [mft_core::DirectoryTotals],
) {
    if fallback.is_empty() {
        return;
    }

    let measured: Vec<(u32, u64, i64)> = fallback
        .par_iter()
        .filter_map(|(parent, name)| {
            if is_cancelled(ScanKind::Hotspot) {
                return None;
            }
            let metadata = std::fs::metadata(paths[*parent as usize].join(name)).ok()?;
            Some((
                *parent,
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

    for (parent, size, modified_millis) in measured {
        // A zero-size file adds nothing and is not counted; `accumulate_files` applies the same rule,
        // so "what counts as a file" cannot drift between the two passes.
        if size > 0 {
            totals[parent as usize].add_file(size, modified_millis);
        }
    }
}

/// Convert the aggregated per-node totals into the external "path → stats".
///
/// Directories whose subtree holds no countable file are omitted (the old code expressed this by
/// only creating a stats entry on the first positive-size file). Paths are moved out, not cloned.
fn into_folder_map(
    index: &mft_core::VolumeIndex,
    paths: Vec<PathBuf>,
    totals: &[mft_core::DirectoryTotals],
) -> HashMap<PathBuf, FolderStats> {
    let mut folder_map = HashMap::with_capacity(index.bfs_order().len());

    for (node, path) in paths.into_iter().enumerate() {
        let stats = totals[node];
        if stats.is_empty() {
            continue;
        }
        debug_assert!(
            !path.as_os_str().is_empty(),
            "只发射可达目录，路径由 build_directory_paths 保证非空"
        );
        folder_map.insert(
            path,
            FolderStats {
                total_size: stats.size,
                file_count: stats.file_count as usize,
                last_modified: stats.modified,
            },
        );
    }

    folder_map
}

/// Read `$MFT` once sequentially and return all valid records.
fn read_all_records(
    drive_letter: char,
    progress_cb: &(impl Fn(MftScanProgress) + Sync),
) -> Result<Vec<mft_core::MftRecordInfo>, String> {
    let stage_start = Instant::now();
    let metadata_reader = mft_core::NtfsFileMetadataReader::open(drive_letter)?;
    let (records, mft_stats) = metadata_reader.read_all_records(&|processed| {
        if is_cancelled(ScanKind::Hotspot) {
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

// ============================================================================
// Legacy pipeline — the oracle for `hotspot_aggregate_equivalence`, compiled for tests only
//
// Verbatim copy of what produced the hotspot ranking before `VolumeIndex` took over: the BFS index,
// the per-file accumulation and the level-by-level propagation. It stays here so the equivalence
// test compares the new implementation against the behaviour users actually saw, quirks included —
// a rewritten oracle could share a bug with the code it is meant to check.
//
// The parallel stat pass is deliberately **not** duplicated here: it did not change, only the place
// it looks up a parent path did (`directories[id].path` → `paths[node]`), and both are the same
// `PathBuf` walked from the drive root, which the equivalence test asserts directly.
// ============================================================================

#[cfg(test)]
use std::collections::VecDeque;

#[cfg(test)]
#[derive(Clone)]
struct DirectoryInfo {
    parent_id: u64,
    path: PathBuf,
    depth: u16,
}

#[cfg(test)]
#[derive(Clone, Copy, Default)]
struct DirectoryStats {
    total_size: u64,
    file_count: usize,
    last_modified: i64,
}

#[cfg(test)]
struct DirectoryIndex {
    directories: HashMap<u64, DirectoryInfo>,
    deepest_depth: u16,
}

/// Fixed record number of the volume root in NTFS: the first 16 MFT entries are metafiles and `$Root` is 5.
#[cfg(test)]
const ROOT_MFT_ID: u64 = 5;

#[cfg(test)]
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
#[cfg(test)]
fn accumulate_file_stats<'a>(
    entries: &'a [mft_core::MftRecordInfo],
    directories: &HashMap<u64, DirectoryInfo>,
) -> (HashMap<u64, DirectoryStats>, Vec<(u64, &'a str)>, usize) {
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

/// Add upward from the deepest directory: by the time a level is processed, all its child directories have been aggregated.
#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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

/// Old tail of the pipeline: move the built paths out of the index and pair them with their stats.
#[cfg(test)]
fn legacy_into_folder_map(
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
        // Fixed timestamp, convenient for asserting last_modified
        record_at(
            mft_id,
            parent_id,
            name,
            is_dir,
            size,
            has_data_stream,
            1_700_000_000,
        )
    }

    /// Same, with an explicit modification time in seconds, the unit `$MFT` stores.
    fn record_at(
        mft_id: u64,
        parent_id: u64,
        name: &str,
        is_dir: bool,
        size: u64,
        has_data_stream: bool,
        modified: i64,
    ) -> mft_core::MftRecordInfo {
        mft_core::MftRecordInfo {
            mft_id,
            parent_id,
            name: name.to_string(),
            is_dir,
            size,
            modified,
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

        propagate_to_ancestors(&index.directories, index.deepest_depth, &mut stats_by_dir);
        let folder_map = legacy_into_folder_map(index.directories, &mut stats_by_dir);

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

    /// The volume both pipelines see in the equivalence test below.
    ///
    /// The shape covers everything the two implementations could disagree about: a branch deeper
    /// than any level-based bookkeeping, an empty file, a file without `$DATA` (the fallback stat
    /// path), an orphan branch whose parent id is not a directory, and a self-parent cycle.
    fn rich_volume() -> Vec<mft_core::MftRecordInfo> {
        vec![
            record_at(ROOT_MFT_ID, ROOT_MFT_ID, ".", true, 0, false, 0),
            record_at(26, ROOT_MFT_ID, "Users", true, 0, false, 0),
            record_at(30, 26, "Evan", true, 0, false, 0),
            record_at(31, 30, "Desktop", true, 0, false, 0),
            record_at(32, 30, "Downloads", true, 0, false, 0),
            record_at(70, 31, "deep", true, 0, false, 0),
            record_at(71, 70, "deeper", true, 0, false, 0),
            record_at(80, 71, "buried.bin", false, 5_000, true, 1_700_000_500),
            record_at(40, 31, "report.bin", false, 100, true, 1_700_000_200),
            // Empty file: contributes 0 and is not counted, but it still exists
            record_at(41, 31, "empty.bin", false, 0, true, 1_700_000_099),
            // No $DATA: size unknown, goes through the parallel stat pass
            record_at(42, 31, "no-data.bin", false, 0, false, 1_700_000_050),
            record_at(50, 32, "movie.bin", false, 900, true, 1_700_000_900),
            record_at(51, 26, "shared.bin", false, 50, true, 1_700_000_100),
            // Parent id 999 is not a directory: the whole branch is unreachable from the root
            record_at(60, 999, "orphan", true, 0, false, 0),
            record_at(61, 60, "inside.bin", false, 700, true, 1_700_000_400),
            // Self-parent: must neither loop nor be emitted
            record_at(90, 90, "selfloop", true, 0, false, 0),
        ]
    }

    /// `hotspot_aggregate_equivalence` — the gate for switching hotspot to `VolumeIndex`.
    ///
    /// Both pipelines are fed the same volume and must produce the same `path → FolderStats` table.
    /// The injected fallback measurement is in **milliseconds**, which is the unit
    /// `FolderStats.last_modified` is specified in: if the new code rounded to seconds anywhere, the
    /// value below would lose its last three digits and this test fails. That precision is not
    /// hypothetical — it comes from `fs::metadata` in `read_fallback_sizes`.
    #[test]
    fn hotspot_aggregate_matches_the_legacy_pipeline() {
        let records = rich_volume();

        // ---- new pipeline ----
        let index = mft_core::VolumeIndex::build(&records);
        let paths = build_directory_paths(&index, 'C');
        let mut totals = index.empty_totals();
        index.accumulate_files(&mut totals);
        let (fallback, new_file_count) = collect_fallback_files(&index);

        let desktop = index
            .bfs_order()
            .iter()
            .copied()
            .find(|&node| index.name_of(node) == "Desktop")
            .expect("Desktop 应当在可达目录里");
        let desktop_path = paths[desktop as usize].clone();
        let root_path = paths[index.root() as usize].clone();
        // Stand-in for the stat pass over `no-data.bin`; deliberately carries sub-second digits.
        totals[desktop as usize].add_file(4_096, 1_700_001_123_456);
        index.propagate_totals(&mut totals);
        let new_deepest = index.deepest_depth();
        let new_map = into_folder_map(&index, paths, &totals);

        // ---- legacy pipeline ----
        let legacy_index = build_directory_index(&records, 'C');
        let legacy_deepest = legacy_index.deepest_depth;
        let (mut stats_by_dir, legacy_fallback, legacy_file_count) =
            accumulate_file_stats(&records, &legacy_index.directories);
        add_file_stats(&mut stats_by_dir, 31, 4_096, 1_700_001_123_456);
        propagate_to_ancestors(
            &legacy_index.directories,
            legacy_index.deepest_depth,
            &mut stats_by_dir,
        );
        let legacy_map = legacy_into_folder_map(legacy_index.directories, &mut stats_by_dir);

        // The fallback queue and the reported file count must not drift: the number is shown to the
        // user, and a file missing from the queue silently loses its size.
        assert_eq!(new_file_count, legacy_file_count);
        assert_eq!(new_file_count, 6, "orphan / selfloop 分支下的文件不算");
        let mut new_fallback: Vec<&str> = fallback.iter().map(|(_, name)| *name).collect();
        let mut old_fallback: Vec<&str> = legacy_fallback.iter().map(|(_, name)| *name).collect();
        new_fallback.sort_unstable();
        old_fallback.sort_unstable();
        assert_eq!(new_fallback, old_fallback);
        assert_eq!(new_fallback, vec!["no-data.bin"]);

        // Same emitted directories, in the same native form — this is what makes the fallback stat
        // (which resolves a name against its parent path) land on the same file in both pipelines.
        let mut new_keys: Vec<&PathBuf> = new_map.keys().collect();
        let mut old_keys: Vec<&PathBuf> = legacy_map.keys().collect();
        new_keys.sort_unstable();
        old_keys.sort_unstable();
        assert_eq!(new_keys, old_keys);

        let describe = |map: &HashMap<PathBuf, FolderStats>| -> Vec<(String, u64, usize, i64)> {
            let mut rows: Vec<(String, u64, usize, i64)> = map
                .iter()
                .map(|(path, stats)| {
                    (
                        path.to_string_lossy().into_owned(),
                        stats.total_size,
                        stats.file_count,
                        stats.last_modified,
                    )
                })
                .collect();
            rows.sort();
            rows
        };
        assert_eq!(describe(&legacy_map), describe(&new_map));

        // Depth bookkeeping is part of the UI contract; the old index tracked it separately.
        assert_eq!(new_deepest, legacy_deepest);
        assert_eq!(new_deepest, 5);

        // Absolute numbers on top of the equality above, so a bug present in both pipelines cannot
        // pass: 50 + 100 + 5000 + 900 + the injected 4096.
        let root = new_map[&root_path];
        assert_eq!(root.total_size, 10_146);
        assert_eq!(root.file_count, 5, "定时注入的 1 个 + 4 个非空文件");
        assert_eq!(
            root.last_modified, 1_700_001_123_456,
            "注入的毫秒时间戳要原样冒到根，不能被截成秒"
        );

        // Milliseconds survive on the directory that received the measurement, too.
        let desktop_stats = new_map[&desktop_path];
        assert_eq!(desktop_stats.last_modified % 1000, 456);
        assert_eq!(desktop_stats.total_size, 100 + 5_000 + 4_096);

        // Unreachable branches never reach the output.
        assert!(!new_map.contains_key(&PathBuf::from("C:\\orphan")));
        assert!(!new_map.contains_key(&PathBuf::from("C:\\selfloop")));
    }
}
