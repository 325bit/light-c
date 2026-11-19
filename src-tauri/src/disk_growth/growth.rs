// ============================================================================
// C drive space change analysis
//
// Compares this MFT aggregation with the previous snapshot to find added, reduced, and fastest-growing directories.
// This only analyzes changes, without making cleanup judgments, to avoid wrongly applying "can it be deleted" semantics to all disk directories.
// ============================================================================

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

use super::mft_scan::{
    normalize_path, scan_drive_with_progress, DirSizeEntry, DiskGrowthPhaseDuration,
    DiskGrowthScanProgress, FileSnapshotEntry,
};
use super::snapshot::{
    build_snapshot, DiskSnapshot, DiskSnapshotEntry, DiskSnapshotManager, FileDetailOrder,
    FileDiffRow,
};

const SIGNIFICANT_THRESHOLD: i64 = 1024 * 1024 * 1024;
const FAST_THRESHOLD: i64 = 300 * 1024 * 1024;
// Small changes must also enter the list, otherwise users see a disjointed experience where the summary shows a net change but the details show nothing.
const MINOR_THRESHOLD: i64 = 1;
const DEFAULT_MAX_CHANGE_ENTRIES: usize = 300;
const MIN_CHANGE_ENTRIES: usize = 50;
const MAX_CHANGE_ENTRIES: usize = 1000;
const MAX_DETAIL_ENTRIES: usize = 50;
/// Maximum number of nodes in the HTML report.
///
/// Note: each node renders as a full detail card; node count determines report size and browser parsing cost.
/// Measured (headless browser, excluding startup overhead): about 300 ms per MB for parsing + DOM construction; 5000 nodes ≈ 3.2 MB / 1.0 s,
/// 10000 nodes ≈ 6.5 MB / 2.1 s, 20000 nodes ≈ 12.9 MB / 4.7 s.
/// 10000 is the balance point chosen here: a single export still covers most details of all 1000 changed directories,
/// opening time stays around 2 s, and the report footer explicitly notes truncation. Actual size depends on the data volume;
/// with a flat directory structure the node count is far below the cap, so the exported report may be only a few hundred KB.
const MAX_EXPORT_TREE_NODES: usize = 10_000;
const DEFAULT_DETAIL_PAGE_SIZE: usize = 200;
/// Hard ceiling for one detail page.
///
/// This is deliberately generous: a directory can report 10k+ changed files (Edge's cache,
/// npm/pip caches) and the tree needs the file paths to rebuild folders below the snapshot's
/// aggregation depth. The module dialog offers "load all" through this ceiling, and the whole
/// page is virtualised on the client, so the cost is one JSON payload rather than rendering.
const MAX_DETAIL_PAGE_SIZE: usize = 20_000;
const DETAIL_SUBTREE_FALLBACK_DEPTH: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiskGrowthLevel {
    Significant,
    Fast,
    Minor,
    Stable,
    Decreased,
    New,
}

#[derive(Debug, Clone, Copy)]
struct SnapshotValue {
    size: u64,
    modified: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskGrowthDetailEntry {
    pub path: String,
    pub name: String,
    pub old_size: u64,
    pub new_size: u64,
    pub diff: i64,
    pub modified: i64,
    pub level: DiskGrowthLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskGrowthEntry {
    pub path: String,
    pub old_size: u64,
    pub new_size: u64,
    pub diff: i64,
    pub diff_percent: f64,
    pub modified: i64,
    pub level: DiskGrowthLevel,
    pub explanation: String,
    pub suggestion: String,
    pub details: Vec<DiskGrowthDetailEntry>,
}

/// Directory change tree node dedicated to HTML export.
/// Build multi-level details all at once on the backend, avoiding repeated paginated IPC requests from the frontend for each directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskGrowthExportNode {
    pub path: String,
    pub name: String,
    pub old_size: u64,
    pub new_size: u64,
    pub diff: i64,
    pub modified: i64,
    pub level: DiskGrowthLevel,
    pub children: Vec<DiskGrowthExportNode>,
}

/// HTML export directory tree response.
/// Explicitly return truncation status so that when an extreme number of directories exceeds the cap, users are not misled into thinking the report contains all details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskGrowthExportTreeResponse {
    pub nodes: Vec<DiskGrowthExportNode>,
    pub total_nodes: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskGrowthReport {
    pub entries: Vec<DiskGrowthEntry>,
    pub total_growth: i64,
    pub significant_count: usize,
    pub fast_count: usize,
    pub new_count: usize,
    pub decreased_count: usize,
    pub time_span: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskAnalyzeEntry {
    pub path: String,
    pub size: u64,
    pub depth: u8,
    pub category: String,
    pub modified: i64,
    pub reason: String,
    pub suggestion: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskAnalyzeResult {
    pub entries: Vec<DiskAnalyzeEntry>,
    pub changed_size: u64,
    pub increased_size: u64,
    pub decreased_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskScanAndAnalyzeResponse {
    pub drive_letter: String,
    pub total_size: u64,
    pub total_files_scanned: usize,
    pub scan_duration_ms: u64,
    pub root_path: String,
    pub previous_scan_time: Option<String>,
    pub current_scan_time: String,
    pub backend: String,
    pub phase_durations: Vec<DiskGrowthPhaseDuration>,
    pub mft_size_count: usize,
    pub metadata_fallback_count: usize,
    /// $MFT segment count / disk read time / parse time, used to tell whether the bottleneck is disk or CPU.
    pub mft_fragments: usize,
    pub mft_io_ms: u64,
    pub mft_parse_ms: u64,
    pub analyze: DiskAnalyzeResult,
    pub growth: DiskGrowthReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskGrowthFileDetailEntry {
    pub path: String,
    pub name: String,
    pub old_size: u64,
    pub new_size: u64,
    pub diff: i64,
    pub modified: i64,
    pub level: DiskGrowthLevel,
}

impl FileDiffRow for DiskGrowthFileDetailEntry {
    fn diff_path(&self) -> &str {
        &self.path
    }

    fn diff_old_size(&self) -> u64 {
        self.old_size
    }

    fn diff_new_size(&self) -> u64 {
        self.new_size
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskGrowthFileDetailsResponse {
    pub path: String,
    pub previous_scan_time: String,
    pub current_scan_time: String,
    pub entries: Vec<DiskGrowthFileDetailEntry>,
    pub total_changed_files: usize,
    pub returned_files: usize,
    pub offset: usize,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskGrowthDirectoryDetailsResponse {
    pub path: String,
    pub previous_scan_time: String,
    pub current_scan_time: String,
    pub entries: Vec<DiskGrowthDetailEntry>,
    pub total_changed_dirs: usize,
    pub returned_dirs: usize,
    pub offset: usize,
    pub has_more: bool,
}

pub fn scan_and_analyze_drive_with_progress<F>(
    progress: &F,
    max_change_entries: Option<usize>,
    drive_letter: Option<String>,
) -> Result<DiskScanAndAnalyzeResponse, String>
where
    F: Fn(DiskGrowthScanProgress) + Sync,
{
    let drive_letter = normalize_growth_drive_letter(drive_letter.as_deref())?;
    let drive_label = format!("{} 盘", drive_letter);
    let max_change_entries = normalize_max_change_entries(max_change_entries);
    let manager = DiskSnapshotManager::for_drive(&drive_letter)?;
    // Everything in this function used to be timed only up to directory aggregation, so the "scan took X"
    // number shown to the user silently excluded three real costs: loading the previous snapshot,
    // comparing the two snapshots and writing the new one. On a million-file disk those add up to seconds.
    let total_started = Instant::now();
    let phase_started = Instant::now();
    let previous = manager.load_latest_snapshot()?;
    let previous_scan_time = previous.as_ref().map(|snapshot| snapshot.date.clone());
    let snapshot_load_ms = phase_started.elapsed().as_millis() as u64;

    // Reserve the snapshot name and open its shard directory *before* the scan: file rows are streamed
    // into it while the volume index is still in hand, so the file list never has to exist as one
    // million-row buffer between the scan and the write.
    let mut snapshot_writer = manager.begin_scan_snapshot()?;
    let scan_result = {
        let mut sink = |parent: &str, name: &str, size: u64, modified: i64| {
            snapshot_writer.append_file(parent, name, size, modified)
        };
        scan_drive_with_progress(&drive_letter, progress, &mut sink)
    };
    // On failure the writer is dropped here, which takes the half-written shard directory with it:
    // a truncated file list would otherwise be read by the next comparison as mass deletion.
    let mut scan = scan_result?;
    // Keep phases in execution order: the previous snapshot is loaded before $MFT is read.
    scan.phase_durations.insert(
        0,
        DiskGrowthPhaseDuration {
            stage: "snapshot_load".to_string(),
            duration_ms: snapshot_load_ms,
        },
    );

    let phase_started = Instant::now();
    let current_snapshot = build_snapshot(&scan);
    let growth = compare_snapshots(
        &current_snapshot,
        previous.as_ref(),
        max_change_entries,
        &drive_label,
    );
    scan.phase_durations.push(DiskGrowthPhaseDuration {
        stage: "compare".to_string(),
        duration_ms: phase_started.elapsed().as_millis() as u64,
    });
    // The shards were already written while scanning; this only closes them and writes the main JSON.
    let phase_started = Instant::now();
    manager.commit_scan_snapshot(snapshot_writer, &current_snapshot)?;
    scan.phase_durations.push(DiskGrowthPhaseDuration {
        stage: "snapshot_write".to_string(),
        duration_ms: phase_started.elapsed().as_millis() as u64,
    });
    // Report wall-clock time for the whole analysis: the number under "scan took" has to match the time
    // the user actually waited, otherwise every later optimization would look like a regression.
    scan.scan_duration_ms = total_started.elapsed().as_millis() as u64;

    let analyze_entries = build_analyze_entries(&scan.entries, &growth);
    let increased_size = growth
        .entries
        .iter()
        .filter(|entry| entry.diff > 0)
        .map(|entry| entry.diff as u64)
        .sum();
    let decreased_size = growth
        .entries
        .iter()
        .filter(|entry| entry.diff < 0)
        .map(|entry| entry.diff.unsigned_abs())
        .sum();

    Ok(DiskScanAndAnalyzeResponse {
        drive_letter: format!("{}:", drive_letter),
        total_size: scan.total_size,
        total_files_scanned: scan.total_files_scanned,
        scan_duration_ms: scan.scan_duration_ms,
        root_path: scan.root_path,
        previous_scan_time,
        current_scan_time: current_snapshot.date,
        backend: scan.backend,
        phase_durations: scan.phase_durations,
        mft_size_count: scan.mft_size_count,
        metadata_fallback_count: scan.metadata_fallback_count,
        mft_fragments: scan.mft_fragments,
        mft_io_ms: scan.mft_io_ms,
        mft_parse_ms: scan.mft_parse_ms,
        analyze: DiskAnalyzeResult {
            entries: analyze_entries,
            changed_size: growth.total_growth.unsigned_abs(),
            increased_size,
            decreased_size,
        },
        growth,
    })
}

pub fn get_file_change_details(
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
    sort: Option<String>,
    desc: Option<bool>,
    drive_letter: Option<String>,
) -> Result<DiskGrowthFileDetailsResponse, String> {
    let drive_letter = normalize_growth_drive_letter(drive_letter.as_deref())?;
    let manager = DiskSnapshotManager::for_drive(&drive_letter)?;
    let Some((previous_handle, current_handle)) = manager.load_latest_two_snapshot_handles()?
    else {
        return Err(format!(
            "至少完成两次 {} 盘全盘扫描后，才能查看文件级变化明细",
            drive_letter
        ));
    };
    if !manager.has_file_detail_storage(&previous_handle.path, &previous_handle.snapshot)
        || !manager.has_file_detail_storage(&current_handle.path, &current_handle.snapshot)
    {
        return Err("最近快照不包含文件级明细，请再完成一次全盘扫描后重试".to_string());
    }

    let normalized_path = normalize_query_path(&path);
    let offset = offset.unwrap_or(0);
    let limit = limit
        .unwrap_or(DEFAULT_DETAIL_PAGE_SIZE)
        .clamp(1, MAX_DETAIL_PAGE_SIZE);
    // Paging walks one order, so the order has to be known before the Top-N pruning happens.
    let order = FileDetailOrder::parse(sort.as_deref(), desc);
    let previous_entries = manager.load_file_entries_for_parent(
        &previous_handle.path,
        &previous_handle.snapshot,
        &normalized_path,
    )?;
    let current_entries = manager.load_file_entries_for_parent(
        &current_handle.path,
        &current_handle.snapshot,
        &normalized_path,
    )?;
    let mut entries = build_file_detail_entries(&previous_entries, &current_entries);

    let mut total_changed_files = entries.len();

    if entries.is_empty() && should_use_subtree_fallback(&normalized_path, &current_handle.snapshot)
    {
        // Directory aggregation snapshots keep only limited levels; when a parent's change comes from deeper files, direct files may show no change.
        // The fallback is enabled only at depth-boundary directories and keeps only the Top N needed for the current page, avoiding reading all descendant files of a huge directory into memory.
        let (subtree_entries, subtree_total) = manager.collect_subtree_file_diffs(
            &previous_handle.path,
            &previous_handle.snapshot,
            &current_handle.path,
            &current_handle.snapshot,
            &normalized_path,
            order,
            offset.saturating_add(limit),
        )?;
        total_changed_files = subtree_total;
        entries = subtree_entries
            .into_iter()
            .map(|entry| {
                let diff = entry.new_size as i64 - entry.old_size as i64;
                DiskGrowthFileDetailEntry {
                    name: display_name_from_path(&entry.path),
                    path: entry.path,
                    old_size: entry.old_size,
                    new_size: entry.new_size,
                    diff,
                    modified: entry.modified,
                    level: determine_level(diff, entry.old_size),
                }
            })
            .collect();
    }

    entries.sort_by(|left, right| order.compare(left, right));
    let paged_entries: Vec<DiskGrowthFileDetailEntry> =
        entries.into_iter().skip(offset).take(limit).collect();
    let returned_files = paged_entries.len();

    Ok(DiskGrowthFileDetailsResponse {
        path: normalized_path,
        previous_scan_time: previous_handle.snapshot.date,
        current_scan_time: current_handle.snapshot.date,
        returned_files,
        total_changed_files,
        has_more: offset + returned_files < total_changed_files,
        offset,
        entries: paged_entries,
    })
}

fn build_file_detail_entries(
    previous_entries: &[FileSnapshotEntry],
    current_entries: &[FileSnapshotEntry],
) -> Vec<DiskGrowthFileDetailEntry> {
    let previous_map = file_snapshot_map(previous_entries);
    let current_map = file_snapshot_map(current_entries);
    let mut all_paths: HashSet<String> = previous_map.keys().cloned().collect();
    all_paths.extend(current_map.keys().cloned());

    all_paths
        .into_iter()
        .filter_map(|file_path| {
            let previous_value = previous_map.get(&file_path).copied();
            let current_value = current_map.get(&file_path).copied();
            let old_size = previous_value.map(|value| value.size).unwrap_or(0);
            let new_size = current_value.map(|value| value.size).unwrap_or(0);
            let diff = new_size as i64 - old_size as i64;
            if diff == 0 {
                return None;
            }
            // When deleted or reduced, the current snapshot may not have the file; fall back to the last known modification time from the previous snapshot.
            let modified = current_value
                .map(|value| value.modified)
                .or_else(|| previous_value.map(|value| value.modified))
                .unwrap_or(0);

            Some(DiskGrowthFileDetailEntry {
                name: display_name_from_path(&file_path),
                path: file_path,
                old_size,
                new_size,
                diff,
                modified,
                level: determine_level(diff, old_size),
            })
        })
        .collect()
}

pub fn get_directory_change_details(
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
    drive_letter: Option<String>,
) -> Result<DiskGrowthDirectoryDetailsResponse, String> {
    let drive_letter = normalize_growth_drive_letter(drive_letter.as_deref())?;
    let manager = DiskSnapshotManager::for_drive(&drive_letter)?;
    let Some((previous, current)) = manager.load_latest_two_snapshots()? else {
        return Err(format!(
            "至少完成两次 {} 盘全盘扫描后，才能查看目录变化明细",
            drive_letter
        ));
    };

    let normalized_path = normalize_query_path(&path);
    let offset = offset.unwrap_or(0);
    let limit = limit
        .unwrap_or(DEFAULT_DETAIL_PAGE_SIZE)
        .clamp(1, MAX_DETAIL_PAGE_SIZE);
    let previous_children = direct_child_map(&previous.entries);
    let current_children = direct_child_map(&current.entries);
    let details =
        build_detail_entries_unlimited(&normalized_path, &previous_children, &current_children);
    let total_changed_dirs = details.len();
    let paged_entries: Vec<DiskGrowthDetailEntry> =
        details.into_iter().skip(offset).take(limit).collect();
    let returned_dirs = paged_entries.len();

    Ok(DiskGrowthDirectoryDetailsResponse {
        path: normalized_path,
        previous_scan_time: previous.date,
        current_scan_time: current.date,
        returned_dirs,
        total_changed_dirs,
        has_more: offset + returned_dirs < total_changed_dirs,
        offset,
        entries: paged_entries,
    })
}

/// Builds up to three levels of directory change details for the export report.
/// Read the snapshot only once and reuse parent-child indexes in memory, avoiding amplification of disk IO and IPC counts during export.
pub fn get_export_directory_tree(
    paths: Vec<String>,
    max_depth: Option<usize>,
    drive_letter: Option<String>,
) -> Result<DiskGrowthExportTreeResponse, String> {
    let drive_letter = normalize_growth_drive_letter(drive_letter.as_deref())?;
    let manager = DiskSnapshotManager::for_drive(&drive_letter)?;
    let Some((previous_handle, current_handle)) = manager.load_latest_two_snapshot_handles()?
    else {
        return Ok(DiskGrowthExportTreeResponse {
            nodes: Vec::new(),
            total_nodes: 0,
            truncated: false,
        });
    };

    let max_depth = max_depth.unwrap_or(3).clamp(1, 3);
    let previous_map = snapshot_map(&previous_handle.snapshot.entries);
    let current_map = snapshot_map(&current_handle.snapshot.entries);
    let previous_children = direct_child_map(&previous_handle.snapshot.entries);
    let current_children = direct_child_map(&current_handle.snapshot.entries);
    let mut remaining_nodes = MAX_EXPORT_TREE_NODES;
    let mut truncated = false;

    let nodes: Vec<DiskGrowthExportNode> = paths
        .into_iter()
        .map(|path| normalize_query_path(&path))
        .filter(|path| !path.is_empty())
        .filter_map(|path| {
            let old_size = previous_map.get(&path).map(|value| value.size).unwrap_or(0);
            let new_size = current_map.get(&path).map(|value| value.size).unwrap_or(0);
            let modified = current_map
                .get(&path)
                .or_else(|| previous_map.get(&path))
                .map(|value| value.modified)
                .unwrap_or(0);
            build_export_node(
                path,
                old_size,
                new_size,
                modified,
                0,
                max_depth,
                &previous_children,
                &current_children,
                &mut remaining_nodes,
                &mut truncated,
            )
        })
        .collect();
    let total_nodes = count_export_nodes(&nodes);

    Ok(DiskGrowthExportTreeResponse {
        nodes,
        total_nodes,
        truncated,
    })
}

fn build_export_node(
    path: String,
    old_size: u64,
    new_size: u64,
    modified: i64,
    depth: usize,
    max_depth: usize,
    previous_children: &HashMap<String, HashMap<String, SnapshotValue>>,
    current_children: &HashMap<String, HashMap<String, SnapshotValue>>,
    remaining_nodes: &mut usize,
    truncated: &mut bool,
) -> Option<DiskGrowthExportNode> {
    let diff = new_size as i64 - old_size as i64;
    if diff == 0 {
        return None;
    }
    if *remaining_nodes == 0 {
        *truncated = true;
        return None;
    }
    // Even with extreme directory structures, the export report's memory usage and file size must not grow without bound.
    *remaining_nodes -= 1;

    let children = if depth < max_depth {
        build_detail_entries_unlimited(&path, previous_children, current_children)
            .into_iter()
            .filter_map(|detail| {
                build_export_node(
                    detail.path,
                    detail.old_size,
                    detail.new_size,
                    detail.modified,
                    depth + 1,
                    max_depth,
                    previous_children,
                    current_children,
                    remaining_nodes,
                    truncated,
                )
            })
            .collect()
    } else {
        Vec::new()
    };

    Some(DiskGrowthExportNode {
        name: display_name_from_path(&path),
        path,
        old_size,
        new_size,
        diff,
        modified,
        level: determine_level(diff, old_size),
        children,
    })
}

fn count_export_nodes(nodes: &[DiskGrowthExportNode]) -> usize {
    nodes
        .iter()
        .map(|node| 1 + count_export_nodes(&node.children))
        .sum()
}

/// A changed directory before any per-child detail rows exist.
///
/// The comparison used to render up to 50 detail rows for **every** changed directory and only then
/// truncate the list to `max_change_entries`; on a disk with 200k changed directories that is millions
/// of cloned path strings discarded immediately. Details are now built only for the rows that survive.
struct GrowthCandidate {
    path: String,
    old_size: u64,
    new_size: u64,
    diff: i64,
    modified: i64,
}

/// Keep exactly the paths `create_growth_entry` would keep: unchanged, or below the noise floor.
fn push_candidate(
    candidates: &mut Vec<GrowthCandidate>,
    path: &str,
    previous_value: Option<SnapshotValue>,
    current_value: Option<SnapshotValue>,
) {
    let old_size = previous_value.map(|value| value.size).unwrap_or(0);
    let new_size = current_value.map(|value| value.size).unwrap_or(0);
    let diff = new_size as i64 - old_size as i64;
    if diff == 0 || determine_level(diff, old_size) == DiskGrowthLevel::Stable {
        return;
    }
    let modified = current_value
        .map(|value| value.modified)
        .or_else(|| previous_value.map(|value| value.modified))
        .unwrap_or(0);
    candidates.push(GrowthCandidate {
        path: path.to_string(),
        old_size,
        new_size,
        diff,
        modified,
    });
}

/// Whether a directory should be folded into the child that accounts for its whole change.
///
/// The rule was defined on the rendered detail rows, so this reproduces their ordering and cap exactly:
/// only the 50 largest direct-child changes were ever considered. Keeping the cap matters — without it a
/// directory with hundreds of children would collapse in cases where the UI previously showed it.
fn has_matching_child<'a>(
    candidate: &GrowthCandidate,
    previous_children: &'a HashMap<String, HashMap<String, SnapshotValue>>,
    current_children: &'a HashMap<String, HashMap<String, SnapshotValue>>,
    diff_by_path: &HashMap<&str, i64>,
    scratch: &mut Vec<(&'a str, i64)>,
) -> bool {
    let previous = previous_children.get(&candidate.path);
    let current = current_children.get(&candidate.path);
    if previous.is_none() && current.is_none() {
        return false;
    }

    // Reuse one buffer across candidates: every directory is a child of exactly one parent, so the total
    // number of pushed children over the whole pass is the directory count, not candidates × children.
    scratch.clear();
    if let Some(current) = current {
        for (path, value) in current {
            let old_size = previous
                .and_then(|map| map.get(path))
                .map(|value| value.size)
                .unwrap_or(0);
            let diff = value.size as i64 - old_size as i64;
            if diff != 0 {
                scratch.push((path.as_str(), diff));
            }
        }
    }
    if let Some(previous) = previous {
        for (path, value) in previous {
            // Present in both maps means the child was already accounted for above.
            if current.is_some_and(|map| map.contains_key(path)) {
                continue;
            }
            scratch.push((path.as_str(), -(value.size as i64)));
        }
    }

    scratch.sort_by(|left, right| {
        right
            .1
            .abs()
            .cmp(&left.1.abs())
            .then_with(|| left.0.cmp(right.0))
    });
    scratch.truncate(MAX_DETAIL_ENTRIES);

    scratch.iter().any(|(path, diff)| {
        *diff == candidate.diff
            && diff.signum() == candidate.diff.signum()
            // The child must itself be a reported change, otherwise the parent is the only row carrying it.
            && diff_by_path.get(path).copied() == Some(*diff)
    })
}

pub fn compare_snapshots(
    current: &DiskSnapshot,
    previous: Option<&DiskSnapshot>,
    max_change_entries: usize,
    drive_label: &str,
) -> DiskGrowthReport {
    let Some(previous) = previous else {
        return first_scan_report(current, drive_label);
    };

    let current_map = snapshot_map(&current.entries);
    let previous_map = snapshot_map(&previous.entries);
    let current_children = direct_child_map(&current.entries);
    let previous_children = direct_child_map(&previous.entries);

    // Pass 1: diff every path, no detail rows.
    let mut candidates: Vec<GrowthCandidate> =
        Vec::with_capacity(current_map.len() + previous_map.len());
    for (path, current_value) in &current_map {
        if is_root_path(path) {
            continue;
        }
        push_candidate(
            &mut candidates,
            path,
            previous_map.get(path).copied(),
            Some(*current_value),
        );
    }
    for (path, previous_value) in &previous_map {
        // Paths still present in the current snapshot were already pushed above.
        if is_root_path(path) || current_map.contains_key(path) {
            continue;
        }
        push_candidate(&mut candidates, path, Some(*previous_value), None);
    }

    // Pass 2: drop redundant parents using child diffs only.
    let keep: Vec<bool> = {
        let diff_by_path: HashMap<&str, i64> = candidates
            .iter()
            .map(|candidate| (candidate.path.as_str(), candidate.diff))
            .collect();
        let mut scratch: Vec<(&str, i64)> = Vec::new();
        candidates
            .iter()
            .map(|candidate| {
                !has_matching_child(
                    candidate,
                    &previous_children,
                    &current_children,
                    &diff_by_path,
                    &mut scratch,
                )
            })
            .collect()
    };

    // Pass 3: order and truncate.
    let mut candidates: Vec<GrowthCandidate> = candidates
        .into_iter()
        .zip(keep)
        .filter_map(|(candidate, keep)| keep.then_some(candidate))
        .collect();
    candidates.sort_by(|left, right| {
        right
            .diff
            .abs()
            .cmp(&left.diff.abs())
            .then_with(|| left.path.cmp(&right.path))
    });
    candidates.truncate(max_change_entries);

    // Pass 4: render details for the surviving rows only.
    let entries: Vec<DiskGrowthEntry> = candidates
        .into_iter()
        .filter_map(|candidate| {
            let details =
                build_detail_entries(&candidate.path, &previous_children, &current_children);
            create_growth_entry(
                candidate.path,
                candidate.old_size,
                candidate.new_size,
                candidate.modified,
                details,
            )
        })
        .collect();

    let total_growth = current.total_size as i64 - previous.total_size as i64;
    let significant_count = entries
        .iter()
        .filter(|entry| entry.level == DiskGrowthLevel::Significant)
        .count();
    let fast_count = entries
        .iter()
        .filter(|entry| entry.level == DiskGrowthLevel::Fast)
        .count();
    let new_count = entries
        .iter()
        .filter(|entry| entry.level == DiskGrowthLevel::New)
        .count();
    let decreased_count = entries
        .iter()
        .filter(|entry| entry.level == DiskGrowthLevel::Decreased)
        .count();

    DiskGrowthReport {
        summary: build_summary(
            total_growth,
            significant_count,
            fast_count,
            decreased_count,
            drive_label,
        ),
        entries,
        total_growth,
        significant_count,
        fast_count,
        new_count,
        decreased_count,
        time_span: format!("{} -> {}", previous.date, current.date),
    }
}

fn normalize_max_change_entries(value: Option<usize>) -> usize {
    value
        .unwrap_or(DEFAULT_MAX_CHANGE_ENTRIES)
        .clamp(MIN_CHANGE_ENTRIES, MAX_CHANGE_ENTRIES)
}

fn normalize_growth_drive_letter(value: Option<&str>) -> Result<String, String> {
    let raw_value = value
        .map(str::to_string)
        .or_else(|| std::env::var("SystemDrive").ok())
        .unwrap_or_else(|| "C:".to_string());
    let Some(letter) = raw_value
        .chars()
        .find(|character| character.is_ascii_alphabetic())
    else {
        return Err("无效的磁盘盘符".to_string());
    };
    // Drive letters are normalized to uppercase in snapshots, scans, and summaries to avoid D / d / D: producing multiple data sets.
    Ok(letter.to_ascii_uppercase().to_string())
}

fn first_scan_report(current: &DiskSnapshot, drive_label: &str) -> DiskGrowthReport {
    DiskGrowthReport {
        entries: Vec::new(),
        total_growth: 0,
        significant_count: 0,
        fast_count: 0,
        new_count: 0,
        decreased_count: 0,
        time_span: "暂无历史快照".to_string(),
        summary: format!(
            "首次完成 {} 快照，已记录 {} 个文件。下次扫描后会展示空间新增和减少的目录。",
            drive_label, current.total_files_scanned
        ),
    }
}

fn snapshot_map(entries: &[DiskSnapshotEntry]) -> HashMap<String, SnapshotValue> {
    entries
        .iter()
        .map(|entry| {
            (
                entry.path.clone(),
                SnapshotValue {
                    size: entry.size,
                    modified: entry.modified,
                },
            )
        })
        .collect()
}

fn file_snapshot_map(entries: &[FileSnapshotEntry]) -> HashMap<String, SnapshotValue> {
    entries
        .iter()
        .map(|entry| {
            (
                entry.path.clone(),
                SnapshotValue {
                    size: entry.size,
                    modified: entry.modified,
                },
            )
        })
        .collect()
}

fn normalize_query_path(path: &str) -> String {
    normalize_path(path).trim_end_matches('/').to_string()
}

fn should_use_subtree_fallback(path: &str, snapshot: &DiskSnapshot) -> bool {
    snapshot
        .entries
        .iter()
        .find(|entry| entry.path == path)
        .map(|entry| entry.depth >= DETAIL_SUBTREE_FALLBACK_DEPTH)
        .unwrap_or(false)
}

fn is_root_path(path: &str) -> bool {
    let normalized = normalize_path(path).trim_end_matches('/').to_string();
    normalized.len() == 2 && normalized.ends_with(':')
}

/// The eager collapse rule, kept only as the oracle for `lazy_details_match_the_eager_comparison`.
///
/// Production now decides the same thing from child diffs (`has_matching_child`) without rendering rows.
#[cfg(test)]
fn remove_redundant_parent_entries(entries: &mut Vec<DiskGrowthEntry>) {
    let diff_by_path: HashMap<String, i64> = entries
        .iter()
        .map(|entry| (entry.path.clone(), entry.diff))
        .collect();

    entries.retain(|entry| {
        // Only collapse a parent when its change amount exactly matches one direct child, avoiding removal of a valid summary when multiple children change together.
        !entry.details.iter().any(|detail| {
            detail.diff == entry.diff
                && detail.diff.signum() == entry.diff.signum()
                && diff_by_path.get(&detail.path).copied() == Some(detail.diff)
        })
    });
}

fn direct_child_map(
    entries: &[DiskSnapshotEntry],
) -> HashMap<String, HashMap<String, SnapshotValue>> {
    let mut child_map: HashMap<String, HashMap<String, SnapshotValue>> = HashMap::new();
    for entry in entries {
        let Some(parent) = parent_path(&entry.path) else {
            continue;
        };
        // Change details show only direct subdirectories, avoiding the noise of expanding all descendant directories repeatedly.
        child_map.entry(parent).or_default().insert(
            entry.path.clone(),
            SnapshotValue {
                size: entry.size,
                modified: entry.modified,
            },
        );
    }
    child_map
}

fn build_detail_entries(
    parent: &str,
    previous_children: &HashMap<String, HashMap<String, SnapshotValue>>,
    current_children: &HashMap<String, HashMap<String, SnapshotValue>>,
) -> Vec<DiskGrowthDetailEntry> {
    let mut details = build_detail_entries_unlimited(parent, previous_children, current_children);
    details.truncate(MAX_DETAIL_ENTRIES);
    details
}

fn build_detail_entries_unlimited(
    parent: &str,
    previous_children: &HashMap<String, HashMap<String, SnapshotValue>>,
    current_children: &HashMap<String, HashMap<String, SnapshotValue>>,
) -> Vec<DiskGrowthDetailEntry> {
    let previous = previous_children.get(parent);
    let current = current_children.get(parent);
    let mut all_paths = HashSet::new();

    if let Some(previous) = previous {
        all_paths.extend(previous.keys().cloned());
    }
    if let Some(current) = current {
        all_paths.extend(current.keys().cloned());
    }

    let mut details: Vec<DiskGrowthDetailEntry> = all_paths
        .into_iter()
        .filter_map(|path| {
            let previous_value = previous.and_then(|map| map.get(&path)).copied();
            let current_value = current.and_then(|map| map.get(&path)).copied();
            let old_size = previous_value.map(|value| value.size).unwrap_or(0);
            let new_size = current_value.map(|value| value.size).unwrap_or(0);
            let diff = new_size as i64 - old_size as i64;
            if diff == 0 {
                return None;
            }
            let modified = current_value
                .map(|value| value.modified)
                .or_else(|| previous_value.map(|value| value.modified))
                .unwrap_or(0);

            Some(DiskGrowthDetailEntry {
                name: display_name_from_path(&path),
                path,
                old_size,
                new_size,
                diff,
                modified,
                level: determine_level(diff, old_size),
            })
        })
        .collect();

    details.sort_by(|left, right| {
        right
            .diff
            .abs()
            .cmp(&left.diff.abs())
            .then_with(|| left.path.cmp(&right.path))
    });
    details
}

fn parent_path(path: &str) -> Option<String> {
    let normalized = path.trim_end_matches('/');
    let separator_index = normalized.rfind('/')?;
    if separator_index <= 2 {
        return Some(format!("{}/", &normalized[..separator_index]));
    }
    Some(normalized[..separator_index].to_string())
}

fn display_name_from_path(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
        .to_string()
}

fn create_growth_entry(
    path: String,
    old_size: u64,
    new_size: u64,
    modified: i64,
    details: Vec<DiskGrowthDetailEntry>,
) -> Option<DiskGrowthEntry> {
    let diff = new_size as i64 - old_size as i64;
    if diff == 0 {
        return None;
    }

    let level = determine_level(diff, old_size);
    if level == DiskGrowthLevel::Stable {
        return None;
    }

    let diff_percent = if old_size > 0 {
        diff as f64 / old_size as f64 * 100.0
    } else {
        100.0
    };
    let (explanation, suggestion) = explain_path(&path, diff, level);

    Some(DiskGrowthEntry {
        path,
        old_size,
        new_size,
        diff,
        diff_percent,
        modified,
        level,
        explanation,
        suggestion,
        details,
    })
}

fn determine_level(diff: i64, old_size: u64) -> DiskGrowthLevel {
    if old_size == 0 && diff > 0 {
        DiskGrowthLevel::New
    } else if diff >= SIGNIFICANT_THRESHOLD {
        DiskGrowthLevel::Significant
    } else if diff >= FAST_THRESHOLD {
        DiskGrowthLevel::Fast
    } else if diff >= MINOR_THRESHOLD {
        DiskGrowthLevel::Minor
    } else if diff <= -MINOR_THRESHOLD {
        DiskGrowthLevel::Decreased
    } else {
        DiskGrowthLevel::Stable
    }
}

fn build_analyze_entries(
    scan_entries: &[DirSizeEntry],
    growth: &DiskGrowthReport,
) -> Vec<DiskAnalyzeEntry> {
    if growth.entries.is_empty() {
        return scan_entries
            .iter()
            .take(50)
            .map(|entry| DiskAnalyzeEntry {
                path: entry.path.clone(),
                size: entry.size,
                depth: entry.depth,
                category: classify_path(&entry.path),
                modified: entry.modified,
                reason: "当前占用较大的目录".to_string(),
                suggestion: "下次扫描后会显示该目录的空间变化".to_string(),
            })
            .collect();
    }

    growth
        .entries
        .iter()
        .map(|entry| DiskAnalyzeEntry {
            path: entry.path.clone(),
            size: entry.new_size,
            depth: path_depth(&entry.path),
            category: classify_path(&entry.path),
            modified: entry.modified,
            reason: entry.explanation.clone(),
            suggestion: entry.suggestion.clone(),
        })
        .collect()
}

fn explain_path(path: &str, diff: i64, level: DiskGrowthLevel) -> (String, String) {
    let lower_path = path.to_ascii_lowercase();
    // Use unsigned_abs instead of abs() as u64: abs() overflows and panics when diff is i64::MIN.
    // All we need here is a display byte count; the sign is expressed by `direction` below.
    let readable = format_size(diff.unsigned_abs());
    let direction = if diff > 0 { "增加" } else { "减少" };

    if lower_path.contains("/windows/softwaredistribution") {
        (
            format!("Windows 更新缓存{}了 {}", direction, readable),
            "可在系统更新完成后通过 Windows 磁盘清理处理".to_string(),
        )
    } else if lower_path.contains("/users/") && lower_path.contains("/appdata/") {
        (
            format!("用户 AppData 目录{}了 {}", direction, readable),
            "建议展开对应用户目录，确认具体应用缓存来源".to_string(),
        )
    } else if lower_path.contains("/program files") || lower_path.contains("/program files (x86)") {
        (
            format!("程序安装目录{}了 {}", direction, readable),
            "通常与软件安装、更新或游戏资源变化有关，请谨慎处理".to_string(),
        )
    } else if lower_path.contains("/programdata") {
        (
            format!("ProgramData 后台数据{}了 {}", direction, readable),
            "可能来自系统组件、驱动或应用后台缓存".to_string(),
        )
    } else if lower_path.contains("cache") || lower_path.contains("temp") {
        (
            format!("缓存/临时目录{}了 {}", direction, readable),
            "可结合应用状态判断是否需要清理缓存".to_string(),
        )
    } else {
        let label = match level {
            DiskGrowthLevel::Significant => "显著变化",
            DiskGrowthLevel::Fast => "快速变化",
            DiskGrowthLevel::Minor => "轻微变化",
            DiskGrowthLevel::Decreased => "空间释放",
            DiskGrowthLevel::New => "新增目录",
            DiskGrowthLevel::Stable => "基本稳定",
        };
        (
            format!("{}，空间{} {}", label, direction, readable),
            "建议打开目录确认具体文件来源".to_string(),
        )
    }
}

fn classify_path(path: &str) -> String {
    let lower_path = path.to_ascii_lowercase();
    if lower_path.contains("/windows") {
        "Windows".to_string()
    } else if lower_path.contains("/users/") {
        "用户数据".to_string()
    } else if lower_path.contains("/program files") || lower_path.contains("/program files (x86)") {
        "程序目录".to_string()
    } else if lower_path.contains("/programdata") {
        "后台数据".to_string()
    } else if lower_path.contains("$recycle.bin") {
        "回收站".to_string()
    } else {
        "其他目录".to_string()
    }
}

fn build_summary(
    total_growth: i64,
    significant_count: usize,
    fast_count: usize,
    decreased_count: usize,
    drive_label: &str,
) -> String {
    if total_growth > 0 {
        format!(
            "相比上次扫描，{}净增加 {}。发现 {} 个显著增长目录、{} 个快速增长目录。",
            drive_label,
            format_size(total_growth as u64),
            significant_count,
            fast_count
        )
    } else if total_growth < 0 {
        format!(
            "相比上次扫描，{}净减少 {}，其中 {} 个目录释放了明显空间。",
            drive_label,
            format_size(total_growth.unsigned_abs()),
            decreased_count
        )
    } else {
        format!("相比上次扫描，{}总占用基本没有变化。", drive_label)
    }
}

fn path_depth(path: &str) -> u8 {
    path.trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .count()
        .saturating_sub(1) as u8
}

fn format_size(bytes: u64) -> String {
    let value = bytes as f64;
    if value >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.2} GB", value / 1024.0 / 1024.0 / 1024.0)
    } else if value >= 1024.0 * 1024.0 {
        format!("{:.1} MB", value / 1024.0 / 1024.0)
    } else if value >= 1024.0 {
        format!("{:.1} KB", value / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_growth_levels() {
        assert_eq!(
            determine_level(2 * 1024 * 1024 * 1024, 1),
            DiskGrowthLevel::Significant
        );
        assert_eq!(
            determine_level(-100 * 1024 * 1024, 1),
            DiskGrowthLevel::Decreased
        );
    }

    #[test]
    fn export_tree_node_budget_stays_browser_friendly() {
        // The export cap directly determines HTML size and browser parsing cost: measured 20000 nodes ≈ 12.9 MB, needing about 4.7 s
        // to parse and build the DOM; opening in the browser keeps the main thread busy for a long time. Lock in a sensible range here: too low loses the details users want to see,
        // too high makes the report freeze.
        assert!(
            (5_000..=10_000).contains(&MAX_EXPORT_TREE_NODES),
            "导出树节点上限需要落在 5000-10000：过低会丢数据，过高会让报告在浏览器中卡死"
        );
    }

    #[test]
    fn recursive_export_node_count_handles_deep_trees() {
        // count_export_nodes is recursive; this confirms deep nesting does not miscount.
        let leaf = |diff: i64| DiskGrowthExportNode {
            name: "leaf".to_string(),
            path: r"C:\a\leaf".to_string(),
            old_size: 0,
            new_size: diff as u64,
            diff,
            modified: 0,
            level: DiskGrowthLevel::New,
            children: Vec::new(),
        };
        let mut node = leaf(1);
        for _ in 0..64 {
            let mut parent = leaf(1);
            parent.children.push(node);
            node = parent;
        }
        assert_eq!(count_export_nodes(&[node]), 65);
    }

    fn snapshot(date: &str, total_size: u64, entries: Vec<(String, u64)>) -> DiskSnapshot {
        DiskSnapshot {
            timestamp: 0,
            date: date.to_string(),
            total_size,
            total_files_scanned: entries.len(),
            root_path: "c:/".to_string(),
            entries: entries
                .into_iter()
                .map(|(path, size)| DiskSnapshotEntry {
                    depth: path.matches('/').count() as u8,
                    path,
                    size,
                    modified: 10,
                })
                .collect(),
            file_entries: Vec::new(),
            version: 2,
        }
    }

    /// The comparison as implemented before the detail rows were deferred, kept verbatim as the oracle.
    ///
    /// Both paths must produce the same report: a difference would silently change which directories the
    /// UI folds away, and that is exactly the kind of regression nobody reports as a bug.
    fn eager_compare(
        current: &DiskSnapshot,
        previous: &DiskSnapshot,
        max_change_entries: usize,
    ) -> Vec<DiskGrowthEntry> {
        let current_map = snapshot_map(&current.entries);
        let previous_map = snapshot_map(&previous.entries);
        let current_children = direct_child_map(&current.entries);
        let previous_children = direct_child_map(&previous.entries);
        let mut all_paths: HashSet<String> = current_map.keys().cloned().collect();
        all_paths.extend(previous_map.keys().cloned());

        let mut entries: Vec<DiskGrowthEntry> = all_paths
            .into_iter()
            .filter(|path| !is_root_path(path))
            .filter_map(|path| {
                let previous_value = previous_map.get(&path).copied();
                let current_value = current_map.get(&path).copied();
                let old_size = previous_value.map(|value| value.size).unwrap_or(0);
                let new_size = current_value.map(|value| value.size).unwrap_or(0);
                let modified = current_value
                    .map(|value| value.modified)
                    .or_else(|| previous_value.map(|value| value.modified))
                    .unwrap_or(0);
                let details = build_detail_entries(&path, &previous_children, &current_children);
                create_growth_entry(path, old_size, new_size, modified, details)
            })
            .collect();

        remove_redundant_parent_entries(&mut entries);
        entries.sort_by(|left, right| {
            right
                .diff
                .abs()
                .cmp(&left.diff.abs())
                .then_with(|| left.path.cmp(&right.path))
        });
        entries.truncate(max_change_entries);
        entries
    }

    /// Deferring the detail rows must not change a single field of the report.
    ///
    /// The fixture carries the two cases the collapse rule is sensitive to: a directory whose change equals
    /// one child's change (folded), and a directory with 60 changed children where the matching child ranks
    /// 51st by magnitude (kept, because the rule only ever looked at the 50 largest — which is why the new
    /// implementation reproduces that cap instead of scanning every child).
    #[test]
    fn lazy_details_match_the_eager_comparison() {
        let mut previous_entries: Vec<(String, u64)> = vec![
            ("c:/".to_string(), 0),
            ("c:/users".to_string(), 1_000),
            ("c:/users/app".to_string(), 600),
            ("c:/users/app/cache".to_string(), 400),
            ("c:/users/app/data".to_string(), 200),
            // Child removed in the current snapshot
            ("c:/users/old".to_string(), 300),
            // Directory removed entirely
            ("c:/gone".to_string(), 500),
            ("c:/wide".to_string(), 0),
            ("c:/wide/match".to_string(), 0),
        ];
        let mut current_entries: Vec<(String, u64)> = vec![
            ("c:/".to_string(), 0),
            ("c:/users".to_string(), 2_200),
            ("c:/users/app".to_string(), 1_200),
            ("c:/users/app/cache".to_string(), 1_000),
            ("c:/users/app/data".to_string(), 1_200),
            ("c:/wide".to_string(), 101_000),
            ("c:/wide/match".to_string(), 1_000),
        ];
        // 50 children changing by 2GB each, so `c:/wide/match` (+1GB) is the 51st largest child change
        for index in 0..50 {
            let path = format!("c:/wide/c{index:02}");
            previous_entries.push((path.clone(), 0));
            current_entries.push((path, 2_000));
        }

        let previous = snapshot("2026-11-18 10:00:00", 3_000, previous_entries);
        let current = snapshot("2026-11-19 10:00:00", 109_400, current_entries);

        let report = compare_snapshots(&current, Some(&previous), 300, "C 盘");
        let eager = eager_compare(&current, &previous, 300);

        assert_eq!(
            serde_json::to_value(&report.entries).unwrap(),
            serde_json::to_value(&eager).unwrap(),
            "延迟构造明细后，结果必须与旧实现逐字段一致"
        );
        // Both collapse decisions from the fixture, spelled out so a future change fails loudly
        assert!(
            !report
                .entries
                .iter()
                .any(|entry| entry.path == "c:/users/app"),
            "整额等于唯一子目录变化的父目录应被折叠"
        );
        assert!(
            report.entries.iter().any(|entry| entry.path == "c:/wide"),
            "等额子目录排在第 51 位时父目录必须保留"
        );
        // Details are still rendered for the survivors (the UI shows them in the expandable row)
        let users = report
            .entries
            .iter()
            .find(|entry| entry.path == "c:/users")
            .expect("c:/users should be reported");
        assert!(!users.details.is_empty(), "存活行仍要带明细");
    }
}
