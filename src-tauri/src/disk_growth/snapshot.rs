use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use super::mft_scan::{
    normalize_path, DirSizeEntry, FileSizeRecord, FileSnapshotEntry, FullDiskScanResult,
};

const SNAPSHOT_DIR: &str = "disk_growth_snapshots";
const SNAPSHOT_PREFIX: &str = "disk_growth_";
const SNAPSHOT_SUFFIX: &str = ".json";
// File-level details are far larger than the directory aggregate snapshot; store them separately in a same-named .files shard directory to keep the main JSON small.
const FILE_SHARD_SUFFIX: &str = "files";
// 256 buckets spread millions of files across smaller JSONL files without creating too many directory entries.
const FILE_SHARD_BUCKETS: u64 = 256;
const MAX_SNAPSHOTS: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskSnapshotEntry {
    pub path: String,
    pub size: u64,
    pub depth: u8,
    #[serde(default)]
    pub modified: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskSnapshot {
    pub timestamp: i64,
    pub date: String,
    pub total_size: u64,
    pub total_files_scanned: usize,
    pub root_path: String,
    pub entries: Vec<DiskSnapshotEntry>,
    #[serde(default)]
    pub file_entries: Vec<FileSnapshotEntry>,
    pub version: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskFileShardEntry {
    // Store parent and name separately to reduce snapshot bloat from repeating the full path on every line.
    pub parent: String,
    pub name: String,
    pub size: u64,
    #[serde(default)]
    pub modified: i64,
}

#[derive(Debug, Clone)]
pub struct FileSnapshotDiffEntry {
    pub path: String,
    pub old_size: u64,
    pub new_size: u64,
    pub modified: i64,
}

/// Minimal view of one changed file.
///
/// The snapshot collector works with [`FileSnapshotDiffEntry`] while the IPC response uses
/// the richer detail entry; both must sort identically, so the rules live in one place and
/// are applied through this trait.
pub trait FileDiffRow {
    fn diff_path(&self) -> &str;
    fn diff_old_size(&self) -> u64;
    fn diff_new_size(&self) -> u64;
}

impl FileDiffRow for FileSnapshotDiffEntry {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileDetailSortKey {
    Diff,
    Size,
    Name,
}

impl FileDetailSortKey {
    pub fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("size") => Self::Size,
            Some("name") => Self::Name,
            _ => Self::Diff,
        }
    }

    /// Direction used when the caller sends no explicit one: names read A→Z, sizes read largest first.
    fn natural_descending(self) -> bool {
        !matches!(self, Self::Name)
    }
}

/// Ordering of file-level change details.
///
/// Paging is bound to this key: the caller walks one order page by page. The Top-N pruning of
/// a deep subtree therefore has to prune by the same key, otherwise files that belong on the
/// first page (by the requested key) are dropped before they are ever counted — which is
/// exactly why "sort by current size" used to be impossible at the aggregation depth boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileDetailOrder {
    pub key: FileDetailSortKey,
    pub desc: bool,
}

impl FileDetailOrder {
    pub fn parse(sort: Option<&str>, desc: Option<bool>) -> Self {
        let key = FileDetailSortKey::parse(sort);
        Self {
            key,
            desc: desc.unwrap_or_else(|| key.natural_descending()),
        }
    }

    /// `Less` means "comes first in the list"
    pub fn compare<T: FileDiffRow + ?Sized>(self, left: &T, right: &T) -> Ordering {
        // Each key is expressed ascending here; `desc` then flips it, so the flag means the same
        // thing for every column instead of "natural direction or not".
        let ascending = match self.key {
            // Magnitude of the change, not its direction.
            FileDetailSortKey::Diff => diff_magnitude(left).cmp(&diff_magnitude(right)),
            FileDetailSortKey::Size => left.diff_new_size().cmp(&right.diff_new_size()),
            FileDetailSortKey::Name => compare_names(
                file_name_of(left.diff_path()),
                file_name_of(right.diff_path()),
            ),
        };
        let primary = if self.desc {
            ascending.reverse()
        } else {
            ascending
        };
        primary.then_with(|| left.diff_path().cmp(right.diff_path()))
    }
}

fn diff_magnitude<T: FileDiffRow + ?Sized>(row: &T) -> u64 {
    (row.diff_new_size() as i64 - row.diff_old_size() as i64).unsigned_abs()
}

fn file_name_of(path: &str) -> &str {
    path.rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
}

/// Case-insensitive compare that keeps digit runs in numeric order (`file2` before `file10`).
///
/// Mirrors `Intl.Collator(undefined, { numeric: true, sensitivity: 'base' })`, which the
/// frontend uses for the name column, so both sides agree on where a page boundary falls.
fn compare_names(left: &str, right: &str) -> Ordering {
    let mut left_chars = left.chars().peekable();
    let mut right_chars = right.chars().peekable();
    loop {
        match (left_chars.peek().copied(), right_chars.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left_char), Some(right_char)) => {
                if left_char.is_ascii_digit() && right_char.is_ascii_digit() {
                    let left_run = take_digit_run(&mut left_chars);
                    let right_run = take_digit_run(&mut right_chars);
                    let left_digits = left_run.trim_start_matches('0');
                    let right_digits = right_run.trim_start_matches('0');
                    let order = left_digits
                        .len()
                        .cmp(&right_digits.len())
                        .then_with(|| left_digits.cmp(right_digits));
                    if order != Ordering::Equal {
                        return order;
                    }
                } else {
                    left_chars.next();
                    right_chars.next();
                    // Natural-sort comparison of two names, not a path lookup (L4).
                    let order = left_char.to_lowercase().cmp(right_char.to_lowercase());
                    if order != Ordering::Equal {
                        return order;
                    }
                }
            }
        }
    }
}

fn take_digit_run(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut run = String::new();
    while let Some(character) = chars.next_if(char::is_ascii_digit) {
        run.push(character);
    }
    run
}

#[derive(Debug, Clone, Copy)]
struct ShardSnapshotValue {
    size: u64,
    modified: i64,
}

struct SubtreeDiffCollector {
    entries: Vec<FileSnapshotDiffEntry>,
    max_entries: usize,
    total_changed_files: usize,
    order: FileDetailOrder,
    /// Whether `entries` is currently ordered by `order`
    sorted: bool,
}

pub struct DiskSnapshotManager {
    snapshot_dir: PathBuf,
    snapshot_prefix: String,
}

#[derive(Debug, Clone)]
pub struct DiskSnapshotHandle {
    // Detail queries need both the snapshot content and the snapshot file path in order to locate the side-car shard directory.
    pub snapshot: DiskSnapshot,
    pub path: PathBuf,
}

impl DiskSnapshotManager {
    pub fn for_drive(drive_letter: &str) -> Result<Self, String> {
        let snapshot_dir = crate::data_dir::get_data_dir().join(SNAPSHOT_DIR);
        fs::create_dir_all(&snapshot_dir).map_err(|e| format!("创建全盘快照目录失败: {}", e))?;
        Ok(Self {
            snapshot_dir,
            snapshot_prefix: snapshot_prefix_for_drive(drive_letter)?,
        })
    }

    pub fn save_snapshot(&self, snapshot: &DiskSnapshot) -> Result<PathBuf, String> {
        let filename = format!(
            "{}{}{}",
            self.snapshot_prefix,
            chrono::Local::now().format("%Y%m%d_%H%M%S"),
            SNAPSHOT_SUFFIX
        );
        let path = self.snapshot_dir.join(filename);
        let json =
            serde_json::to_string(snapshot).map_err(|e| format!("序列化全盘快照失败: {}", e))?;

        fs::write(&path, json).map_err(|e| format!("写入全盘快照失败: {}", e))?;
        Ok(path)
    }

    pub fn save_scan_snapshot(
        &self,
        scan: &FullDiskScanResult,
        file_records: Vec<FileSizeRecord>,
    ) -> Result<PathBuf, String> {
        let snapshot = build_snapshot(scan);
        let path = self.save_snapshot(&snapshot)?;
        // Save file-level details in per-directory shards to avoid the main JSON ballooning to hundreds of MB with millions of files.
        if let Err(error) = self.save_file_shards(&path, file_records) {
            // The main snapshot and the file shards must succeed together, otherwise detail queries would get an incomplete new snapshot.
            let _ = fs::remove_file(&path);
            let _ = fs::remove_dir_all(self.file_shard_dir(&path));
            return Err(error);
        }
        // Do cleanup only after the whole snapshot set is written successfully, to avoid deleting old snapshots early on errors such as a full disk.
        self.cleanup_old_snapshots()?;
        Ok(path)
    }

    pub fn load_latest_snapshot(&self) -> Result<Option<DiskSnapshot>, String> {
        let snapshots = self.list_snapshots()?;
        let Some(path) = snapshots.first() else {
            return Ok(None);
        };
        self.load_snapshot(path)
    }

    pub fn load_latest_two_snapshots(
        &self,
    ) -> Result<Option<(DiskSnapshot, DiskSnapshot)>, String> {
        Ok(self
            .load_latest_two_snapshot_handles()?
            .map(|(previous, current)| (previous.snapshot, current.snapshot)))
    }

    pub fn load_latest_two_snapshot_handles(
        &self,
    ) -> Result<Option<(DiskSnapshotHandle, DiskSnapshotHandle)>, String> {
        let snapshots = self.list_snapshots()?;
        if snapshots.len() < 2 {
            return Ok(None);
        }

        let Some(current) = self.load_snapshot_handle(&snapshots[0])? else {
            return Ok(None);
        };
        let Some(previous) = self.load_snapshot_handle(&snapshots[1])? else {
            return Ok(None);
        };
        Ok(Some((previous, current)))
    }

    fn list_snapshots(&self) -> Result<Vec<PathBuf>, String> {
        let mut snapshots = Vec::new();
        let entries =
            fs::read_dir(&self.snapshot_dir).map_err(|e| format!("读取全盘快照目录失败: {}", e))?;

        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with(&self.snapshot_prefix) && name.ends_with(SNAPSHOT_SUFFIX) {
                snapshots.push(path);
            }
        }

        snapshots.sort_by(|left, right| right.file_name().cmp(&left.file_name()));
        Ok(snapshots)
    }

    fn load_snapshot(&self, path: &Path) -> Result<Option<DiskSnapshot>, String> {
        if !path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(path)
            .map_err(|e| format!("读取全盘快照失败 {}: {}", path.display(), e))?;
        let snapshot = serde_json::from_str::<DiskSnapshot>(&content)
            .map_err(|e| format!("解析全盘快照失败 {}: {}", path.display(), e))?;
        Ok(Some(snapshot))
    }

    fn load_snapshot_handle(&self, path: &Path) -> Result<Option<DiskSnapshotHandle>, String> {
        let Some(snapshot) = self.load_snapshot(path)? else {
            return Ok(None);
        };
        Ok(Some(DiskSnapshotHandle {
            snapshot,
            path: path.to_path_buf(),
        }))
    }

    pub fn load_file_entries_for_parent(
        &self,
        snapshot_path: &Path,
        snapshot: &DiskSnapshot,
        parent_path: &str,
    ) -> Result<Vec<FileSnapshotEntry>, String> {
        let parent = normalize_path(parent_path)
            .trim_end_matches('/')
            .to_string();
        let shard_dir = self.file_shard_dir(snapshot_path);
        if shard_dir.exists() {
            let shard_path = self.file_shard_path(snapshot_path, &parent);
            if !shard_path.exists() {
                return Ok(Vec::new());
            }
            return self.load_file_shard(&shard_path, &parent);
        }

        // Legacy snapshot compatibility: older JSON may still contain file_entries; only fall back to them when no shards exist.
        Ok(snapshot
            .file_entries
            .iter()
            .filter(|entry| parent_path_of(&entry.path).as_deref() == Some(parent.as_str()))
            .cloned()
            .collect())
    }

    pub fn collect_subtree_file_diffs(
        &self,
        previous_snapshot_path: &Path,
        previous_snapshot: &DiskSnapshot,
        current_snapshot_path: &Path,
        current_snapshot: &DiskSnapshot,
        parent_path: &str,
        order: FileDetailOrder,
        max_entries: usize,
    ) -> Result<(Vec<FileSnapshotDiffEntry>, usize), String> {
        let parent = normalize_path(parent_path)
            .trim_end_matches('/')
            .to_string();
        let prefix = format!("{}/", parent);
        let previous_shard_dir = self.file_shard_dir(previous_snapshot_path);
        let current_shard_dir = self.file_shard_dir(current_snapshot_path);
        if previous_shard_dir.exists() && current_shard_dir.exists() {
            return self.collect_subtree_shard_diffs(
                &previous_shard_dir,
                &current_shard_dir,
                &prefix,
                order,
                max_entries,
            );
        }

        // Legacy snapshot compatibility: older main JSON has already loaded file details into memory; fall back here only when no shards exist.
        Ok(collect_subtree_legacy_diffs(
            previous_snapshot,
            current_snapshot,
            &prefix,
            order,
            max_entries,
        ))
    }

    pub fn has_file_detail_storage(&self, snapshot_path: &Path, snapshot: &DiskSnapshot) -> bool {
        // New snapshots use side-car shards; older ones may still stuff file-level details into the main JSON; both count as queryable.
        self.file_shard_dir(snapshot_path).exists() || !snapshot.file_entries.is_empty()
    }

    fn cleanup_old_snapshots(&self) -> Result<(), String> {
        let snapshots = self.list_snapshots()?;
        if snapshots.len() <= MAX_SNAPSHOTS {
            return Ok(());
        }

        for path in snapshots.iter().skip(MAX_SNAPSHOTS) {
            if let Err(error) = fs::remove_file(path) {
                log::warn!("删除旧全盘快照失败 {}: {}", path.display(), error);
            }
            if let Err(error) = fs::remove_dir_all(self.file_shard_dir(path)) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("删除旧全盘文件分片失败 {}: {}", path.display(), error);
                }
            }
        }

        Ok(())
    }

    fn save_file_shards(
        &self,
        snapshot_path: &Path,
        entries: Vec<FileSizeRecord>,
    ) -> Result<(), String> {
        let dir = self.file_shard_dir(snapshot_path);
        if dir.exists() {
            fs::remove_dir_all(&dir)
                .map_err(|e| format!("清理旧文件级快照分片失败 {}: {}", dir.display(), e))?;
        }
        fs::create_dir_all(&dir)
            .map_err(|e| format!("创建文件级快照分片目录失败 {}: {}", dir.display(), e))?;

        let mut writers: Vec<Option<BufWriter<File>>> =
            (0..FILE_SHARD_BUCKETS).map(|_| None).collect();

        for entry in entries {
            let Some(parent) = parent_path_of(&entry.path) else {
                continue;
            };
            let name = display_name_from_path(&entry.path);
            let bucket = shard_bucket(&parent) as usize;
            if writers[bucket].is_none() {
                let path = dir.join(format!("{:03}.jsonl", bucket));
                let file = File::create(&path)
                    .map_err(|e| format!("创建文件级快照分片失败 {}: {}", path.display(), e))?;
                writers[bucket] = Some(BufWriter::new(file));
            }

            let Some(writer) = writers[bucket].as_mut() else {
                continue;
            };
            let row = DiskFileShardEntry {
                parent,
                name,
                size: entry.size,
                modified: entry.modified,
            };
            serde_json::to_writer(&mut *writer, &row)
                .map_err(|e| format!("写入文件级快照分片失败: {}", e))?;
            writer
                .write_all(b"\n")
                .map_err(|e| format!("写入文件级快照分片失败: {}", e))?;
        }

        for writer in writers.into_iter().flatten() {
            writer
                .into_inner()
                .map_err(|e| format!("刷新文件级快照分片失败: {}", e))?;
        }

        Ok(())
    }

    fn load_file_shard(
        &self,
        shard_path: &Path,
        parent_path: &str,
    ) -> Result<Vec<FileSnapshotEntry>, String> {
        let file = File::open(shard_path)
            .map_err(|e| format!("读取文件级快照分片失败 {}: {}", shard_path.display(), e))?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();

        for line in reader.lines() {
            let line = line
                .map_err(|e| format!("读取文件级快照分片失败 {}: {}", shard_path.display(), e))?;
            if line.trim().is_empty() {
                continue;
            }
            let row: DiskFileShardEntry = serde_json::from_str(&line)
                .map_err(|e| format!("解析文件级快照分片失败 {}: {}", shard_path.display(), e))?;
            if row.parent == parent_path {
                entries.push(FileSnapshotEntry {
                    path: format!("{}/{}", row.parent.trim_end_matches('/'), row.name),
                    size: row.size,
                    modified: row.modified,
                });
            }
        }

        Ok(entries)
    }

    fn collect_subtree_shard_diffs(
        &self,
        previous_shard_dir: &Path,
        current_shard_dir: &Path,
        parent_prefix: &str,
        order: FileDetailOrder,
        max_entries: usize,
    ) -> Result<(Vec<FileSnapshotDiffEntry>, usize), String> {
        let mut collector = SubtreeDiffCollector::new(max_entries, order);
        for index in 0..FILE_SHARD_BUCKETS {
            let shard_name = format!("{:03}.jsonl", index);
            let previous_shard_path = previous_shard_dir.join(&shard_name);
            let current_shard_path = current_shard_dir.join(&shard_name);
            let mut previous_map = load_shard_prefix_map(&previous_shard_path, parent_prefix)?;

            for (path, current_entry) in load_shard_prefix_map(&current_shard_path, parent_prefix)?
            {
                let previous_entry = previous_map.remove(&path);
                collector.push(
                    path,
                    previous_entry.map(|entry| entry.size).unwrap_or(0),
                    current_entry.size,
                    current_entry.modified,
                );
            }

            for (path, previous_entry) in previous_map {
                collector.push(path, previous_entry.size, 0, previous_entry.modified);
            }
        }
        Ok(collector.finish())
    }

    fn file_shard_dir(&self, snapshot_path: &Path) -> PathBuf {
        snapshot_path.with_extension(FILE_SHARD_SUFFIX)
    }

    fn file_shard_path(&self, snapshot_path: &Path, parent_path: &str) -> PathBuf {
        self.file_shard_dir(snapshot_path)
            .join(format!("{:03}.jsonl", shard_bucket(parent_path)))
    }
}

pub fn build_snapshot(scan: &FullDiskScanResult) -> DiskSnapshot {
    let now = chrono::Local::now();
    DiskSnapshot {
        timestamp: now.timestamp_millis(),
        date: now.format("%Y-%m-%d %H:%M:%S").to_string(),
        total_size: scan.total_size,
        total_files_scanned: scan.total_files_scanned,
        root_path: scan.root_path.clone(),
        entries: scan.entries.iter().map(snapshot_entry_from_dir).collect(),
        // File-level details are split into side-car shard files; the main JSON keeps only the directory aggregate snapshot so it stays small even with extreme file counts.
        file_entries: Vec::new(),
        version: 2,
    }
}

fn snapshot_entry_from_dir(entry: &DirSizeEntry) -> DiskSnapshotEntry {
    DiskSnapshotEntry {
        path: normalize_path(&entry.path),
        size: entry.size,
        depth: entry.depth,
        modified: entry.modified,
    }
}

fn parent_path_of(path: &str) -> Option<String> {
    let normalized = normalize_path(path).trim_end_matches('/').to_string();
    normalized
        .rsplit_once('/')
        .map(|(parent, _)| parent.to_string())
        .filter(|parent| !parent.is_empty())
}

fn display_name_from_path(path: &str) -> String {
    path.rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
        .to_string()
}

impl SubtreeDiffCollector {
    fn new(max_entries: usize, order: FileDetailOrder) -> Self {
        Self {
            entries: Vec::new(),
            max_entries: max_entries.max(1),
            total_changed_files: 0,
            order,
            sorted: false,
        }
    }

    fn push(&mut self, path: String, old_size: u64, new_size: u64, modified: i64) {
        if old_size == new_size {
            return;
        }

        self.total_changed_files += 1;
        self.entries.push(FileSnapshotDiffEntry {
            path,
            old_size,
            new_size,
            modified,
        });
        self.sorted = false;

        // Keeping the vector ordered after EVERY push was quadratic, which did not matter while
        // a page was 200 rows but does once "load all" asks for 20k. Sorting in batches of 2N and
        // truncating back to N keeps the same Top-N guarantee at an amortised O(log N) per row.
        let batch = self.max_entries.saturating_mul(2).max(2);
        if self.entries.len() >= batch {
            self.sort_entries();
            self.entries.truncate(self.max_entries);
        }
    }

    fn sort_entries(&mut self) {
        if self.sorted {
            return;
        }
        let order = self.order;
        self.entries
            .sort_by(|left, right| order.compare(left, right));
        self.sorted = true;
    }

    fn finish(mut self) -> (Vec<FileSnapshotDiffEntry>, usize) {
        self.sort_entries();
        // The buffer may hold up to 2N rows between two batch prunes; the caller only wants N.
        self.entries.truncate(self.max_entries);
        (self.entries, self.total_changed_files)
    }
}

fn load_shard_prefix_map(
    shard_path: &Path,
    parent_prefix: &str,
) -> Result<HashMap<String, ShardSnapshotValue>, String> {
    let mut entries = HashMap::new();
    if !shard_path.exists() {
        return Ok(entries);
    }

    let file = File::open(shard_path)
        .map_err(|e| format!("读取文件级快照分片失败 {}: {}", shard_path.display(), e))?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line =
            line.map_err(|e| format!("读取文件级快照分片失败 {}: {}", shard_path.display(), e))?;
        if line.trim().is_empty() {
            continue;
        }
        let row: DiskFileShardEntry = serde_json::from_str(&line)
            .map_err(|e| format!("解析文件级快照分片失败 {}: {}", shard_path.display(), e))?;
        // The fallback cares only about descendants of the current directory; filtering early keeps the temporary HashMap within a shard local in scope.
        if row.parent.starts_with(parent_prefix) {
            entries.insert(
                format!("{}/{}", row.parent.trim_end_matches('/'), row.name),
                ShardSnapshotValue {
                    size: row.size,
                    modified: row.modified,
                },
            );
        }
    }
    Ok(entries)
}

fn collect_subtree_legacy_diffs(
    previous_snapshot: &DiskSnapshot,
    current_snapshot: &DiskSnapshot,
    parent_prefix: &str,
    order: FileDetailOrder,
    max_entries: usize,
) -> (Vec<FileSnapshotDiffEntry>, usize) {
    let mut collector = SubtreeDiffCollector::new(max_entries, order);
    let mut previous_map: HashMap<String, ShardSnapshotValue> = previous_snapshot
        .file_entries
        .iter()
        .filter(|entry| normalize_path(&entry.path).starts_with(parent_prefix))
        .map(|entry| {
            (
                normalize_path(&entry.path),
                ShardSnapshotValue {
                    size: entry.size,
                    modified: entry.modified,
                },
            )
        })
        .collect();

    for entry in current_snapshot
        .file_entries
        .iter()
        .filter(|entry| normalize_path(&entry.path).starts_with(parent_prefix))
    {
        let path = normalize_path(&entry.path);
        let previous_entry = previous_map.remove(&path);
        collector.push(
            path,
            previous_entry.map(|entry| entry.size).unwrap_or(0),
            entry.size,
            entry.modified,
        );
    }

    for (path, previous_entry) in previous_map {
        collector.push(path, previous_entry.size, 0, previous_entry.modified);
    }

    collector.finish()
}

fn shard_bucket(parent_path: &str) -> u64 {
    // Shard files are persistent data, so do not rely on a hasher with unstable implementation details such as std::DefaultHasher.
    let mut hash = 0xcbf29ce484222325u64;
    for byte in parent_path.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash % FILE_SHARD_BUCKETS
}

fn snapshot_prefix_for_drive(drive_letter: &str) -> Result<String, String> {
    let Some(letter) = drive_letter
        .chars()
        .find(|character| character.is_ascii_alphabetic())
        .map(|character| character.to_ascii_lowercase())
    else {
        return Err("无效的磁盘盘符，无法创建全盘快照".to_string());
    };

    // Drive C: keeps the historical filename prefix so snapshots from released versions remain comparable without migration.
    if letter == 'c' {
        Ok(SNAPSHOT_PREFIX.to_string())
    } else {
        Ok(format!("{}_{}", letter, SNAPSHOT_PREFIX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, old_size: u64, new_size: u64) -> FileSnapshotDiffEntry {
        FileSnapshotDiffEntry {
            path: path.to_string(),
            old_size,
            new_size,
            modified: 0,
        }
    }

    fn ordered(order: FileDetailOrder, rows: &[FileSnapshotDiffEntry]) -> Vec<String> {
        let mut rows = rows.to_vec();
        rows.sort_by(|left, right| order.compare(left, right));
        rows.into_iter().map(|row| row.path).collect()
    }

    #[test]
    fn default_order_is_change_magnitude_descending() {
        let rows = [
            row("C:/a/small.log", 0, 1024),
            row("C:/a/huge.bin", 0, 4096),
            // -8000 bytes: magnitude 8000, so it outranks the +4096 file
            row("C:/a/shrink.bin", 9000, 1000),
        ];
        let order = FileDetailOrder::parse(None, None);
        assert_eq!(
            ordered(order, &rows),
            vec!["C:/a/shrink.bin", "C:/a/huge.bin", "C:/a/small.log"]
        );
    }

    #[test]
    fn size_order_ranks_by_current_size_not_by_change() {
        // The huge file that barely changed has to come first: this is exactly the row the old
        // |diff|-only pruning dropped before the frontend ever saw it.
        let rows = [
            row("C:/a/huge_stable.bin", 5_000, 5_001),
            row("C:/a/grown.bin", 0, 4096),
        ];
        let order = FileDetailOrder::parse(Some("size"), None);
        assert_eq!(
            ordered(order, &rows),
            vec!["C:/a/huge_stable.bin", "C:/a/grown.bin"]
        );
    }

    #[test]
    fn name_order_is_numeric_and_case_insensitive() {
        let rows = [
            row("C:/a/file10.log", 0, 1),
            row("C:/a/File2.log", 0, 1),
            row("C:/a/file1.log", 0, 1),
        ];
        let order = FileDetailOrder::parse(Some("name"), None);
        assert_eq!(
            ordered(order, &rows),
            vec!["C:/a/file1.log", "C:/a/File2.log", "C:/a/file10.log"]
        );
    }

    #[test]
    fn explicit_direction_overrides_the_natural_one() {
        let rows = [row("C:/a/one", 0, 10), row("C:/a/two", 0, 99)];
        assert_eq!(
            ordered(FileDetailOrder::parse(Some("size"), Some(false)), &rows),
            vec!["C:/a/one", "C:/a/two"]
        );
        assert_eq!(
            ordered(FileDetailOrder::parse(Some("name"), Some(true)), &rows),
            vec!["C:/a/two", "C:/a/one"]
        );
    }

    #[test]
    fn collector_prunes_by_the_requested_key() {
        // max_entries = 1 forces the Top-1 decision; pruning by |diff| would keep the grown file
        // and lose the big one that "sort by current size" has to show first.
        let mut collector =
            SubtreeDiffCollector::new(1, FileDetailOrder::parse(Some("size"), Some(true)));
        collector.push("C:/big/huge.bin".to_string(), 9_000, 9_001, 0);
        collector.push("C:/big/grown.bin".to_string(), 0, 4_096, 0);
        let (entries, total) = collector.finish();
        assert_eq!(total, 2);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "C:/big/huge.bin");
    }

    #[test]
    fn collector_keeps_the_true_top_n_across_batches() {
        // 1000 pushed rows against a cap of 50: pruning now happens in batches of 100, so this
        // guards that the batched version still returns exactly the global Top-N.
        let order = FileDetailOrder::parse(Some("size"), Some(true));
        let mut collector = SubtreeDiffCollector::new(50, order);
        for index in 0..1_000u64 {
            // index starts at 1: a 0 → 0 row means "no change" and is skipped by design
            collector.push(format!("C:/big/f{:04}.bin", index), 0, (index + 1) * 100, 0);
        }
        let (entries, total) = collector.finish();
        assert_eq!(total, 1_000);
        assert_eq!(entries.len(), 50);
        assert_eq!(entries[0].path, "C:/big/f0999.bin");
        assert_eq!(entries[49].path, "C:/big/f0950.bin");
        for window in entries.windows(2) {
            assert!(window[0].new_size > window[1].new_size);
        }
    }

    #[test]
    fn unknown_sort_key_falls_back_to_the_default() {
        let order = FileDetailOrder::parse(Some("whatever"), None);
        assert_eq!(order.key, FileDetailSortKey::Diff);
        assert!(order.desc);
    }
}
