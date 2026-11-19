// ============================================================================
// MFT core — sequential $MFT reading and record parsing (shared by junk cleanup / disk changes / big directories / big files)
// ============================================================================

#![cfg(target_os = "windows")]

use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::sync::mpsc;
use std::time::Instant;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const MFT_METADATA_MAX: u64 = 25;
const MFT_READ_CHUNK: usize = 16 * 1024 * 1024;
/// Maximum number of buffers that can coexist in the read-and-parse pipeline (16MB × 3 = 48MB).
///
/// On a disk-cache hit, reading is much faster than parsing; without a cap, new buffers keep being allocated and one large `$MFT` can eat all memory.
const MAX_PIPELINE_BUFFERS: usize = 3;
/// Sequential-scan flag: tells the system these reads are consecutively forward so it can pre-read subsequent data.
///
/// Without this flag, each seek_read waits for the previous chunk to finish before issuing the next request,
/// giving an I/O queue depth of only 1; cold reads across multiple disks are over twice as slow (measured 5.78GB across four disks dropped from 35 seconds to normal).
const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
const WINDOWS_TO_UNIX_SECONDS: i64 = 11_644_473_600;
const FILE_RECORD_FLAG_IN_USE: u16 = 0x0001;
const FILE_RECORD_FLAG_DIRECTORY: u16 = 0x0002;
/// Namespace flag of `$FILE_NAME`; 2 means DOS 8.3 short name — prefer the long name.
const FILE_NAME_NAMESPACE_DOS: u8 = 2;
/// Attribute bit in `$STANDARD_INFORMATION` for reparse points (junction / symlink).
const FILE_ATTRIBUTE_REPARSE_POINT_BIT: u32 = 0x0000_0400;

/// All information that can be obtained at once from one `$MFT` record.
///
/// The name comes from the `$FILE_NAME` attribute and the size from `$DATA`; both are in the same record,
/// so one sequential pass yields both — no need for another USN enumeration (which walks the entire MFT in kernel mode,
/// measured 6 times slower than sequential reads in user mode).
#[derive(Clone)]
pub struct MftRecordInfo {
    pub mft_id: u64,
    pub parent_id: u64,
    pub name: String,
    pub is_dir: bool,
    /// Always 0 for directories.
    pub size: u64,
    pub modified: i64,
    /// Whether an unnamed `$DATA` stream was parsed out.
    ///
    /// `size` of 0 has two sources: a genuinely empty file, or `$DATA` not parsed.
    /// When you need to distinguish (e.g. deciding whether to fall back to stat), check this flag, not just size.
    pub has_data_stream: bool,
}

/// Statistics for one full read.
///
/// `io_ms` and `parse_ms` are timed separately: only by splitting them can we tell whether the bottleneck is disk or CPU;
/// previously we looked only at total time and two rounds of changes were blind guesses.
#[derive(Debug, Clone, Copy, Default)]
pub struct MftReadStats {
    /// Number of data runs in `$MFT`
    pub fragments: usize,
    /// Time spent waiting for the disk to return data
    pub io_ms: u64,
    /// Time spent parsing records (fixup + attributes)
    pub parse_ms: u64,
}

#[derive(Clone)]
struct DataRun {
    start_lcn: i64,
    cluster_count: u64,
}

/// One `$MFT` read task: read `len` bytes from `offset` within the volume; these records are numbered starting from `base_record_id`.
///
/// Extracted separately so the read thread only deals with "offset + length" and need not know about data-run division.
struct MftReadChunk {
    offset: u64,
    len: usize,
    base_record_id: u64,
}

/// Split `$MFT` data runs into fixed-size read tasks.
///
/// Record numbers are **continuous across segments**, so each segment starts counting from where the previous one left off;
/// within a segment, chunk by `MFT_READ_CHUNK` and align chunk ends down to a whole record — half a record cannot be parsed.
fn plan_read_chunks(
    runs: &[DataRun],
    cluster_size: u64,
    file_record_size: usize,
) -> Result<Vec<MftReadChunk>, String> {
    let record_size = file_record_size as u64;
    if record_size == 0 {
        return Err("$MFT 记录大小非法".to_string());
    }

    let mut chunks = Vec::new();
    let mut logical_record_id = 0u64;

    for run in runs {
        let run_start = (run.start_lcn as u64)
            .checked_mul(cluster_size)
            .ok_or_else(|| "$MFT data run 起始位置溢出".to_string())?;
        let run_bytes = run
            .cluster_count
            .checked_mul(cluster_size)
            .ok_or_else(|| "$MFT data run 大小溢出".to_string())?;

        let mut read_in_run = 0u64;
        while read_in_run < run_bytes {
            let remaining = (run_bytes - read_in_run) as usize;
            let read_len = remaining.min(MFT_READ_CHUNK);
            // Half a record cannot be parsed, so align the chunk end down to a whole record
            let aligned_len = read_len - read_len % file_record_size;
            if aligned_len == 0 {
                break;
            }
            chunks.push(MftReadChunk {
                offset: run_start + read_in_run,
                len: aligned_len,
                base_record_id: logical_record_id + read_in_run / record_size,
            });
            read_in_run += aligned_len as u64;
        }

        logical_record_id += run_bytes / record_size;
    }

    Ok(chunks)
}

pub struct NtfsFileMetadataReader {
    volume: fs::File,
    bytes_per_sector: usize,
    cluster_size: u64,
    file_record_size: usize,
    mft_runs: Vec<DataRun>,
}

/// Rebuild full paths (including ancestor directories) only for records in `target_ids`.
///
/// Callers are typically **small** target sets like "big files / AI model candidates": among 1.2 million records on a disk
/// we build paths for only tens or hundreds, avoiding millions of string allocations.
pub fn rebuild_paths_for_records(
    records: &[MftRecordInfo],
    drive_letter: char,
    target_ids: &HashSet<u64>,
) -> HashMap<u64, String> {
    let mut record_by_id: HashMap<u64, &MftRecordInfo> = HashMap::with_capacity(records.len());
    for record in records {
        record_by_id.insert(record.mft_id, record);
    }

    let mut paths = HashMap::with_capacity(target_ids.len() + 1);
    let root_path = format!("{}:\\", drive_letter);
    paths.insert(5, root_path);

    for target_id in target_ids {
        let mut visiting = HashSet::new();
        // Lazily parse only candidate paths, avoiding full-path string allocation for every file on the disk.
        let _ = resolve_path(*target_id, &record_by_id, &mut paths, &mut visiting);
    }

    paths
}

fn resolve_path(
    mft_id: u64,
    record_by_id: &HashMap<u64, &MftRecordInfo>,
    paths: &mut HashMap<u64, String>,
    visiting: &mut HashSet<u64>,
) -> Option<()> {
    if paths.contains_key(&mft_id) {
        // On a cache hit, do not clone the full path again; candidate files share the same already-built parent path.
        return Some(());
    }
    if !visiting.insert(mft_id) {
        return None;
    }

    let record = record_by_id.get(&mft_id)?;
    resolve_path(record.parent_id, record_by_id, paths, visiting)?;
    let parent_path = paths.get(&record.parent_id)?.clone();
    let name = &record.name;
    let mut full_path = String::with_capacity(parent_path.len() + 1 + name.len());
    full_path.push_str(&parent_path);
    if !parent_path.ends_with('\\') {
        full_path.push('\\');
    }
    full_path.push_str(name);
    paths.insert(mft_id, full_path);
    visiting.remove(&mft_id);
    Some(())
}

// ============================================================================
// VolumeIndex — one dense, arena-backed view of the parsed $MFT
//
// Landed ahead of its consumers (they switch over in the next step), so the whole section is
// allowed to be unused for now. Both full-disk callers need the same three things after the read:
// parent-child topology, depth per directory, and bottom-up subtree totals. They currently build
// all three separately — two BFS builds, two aggregations (one walking every file up to the root,
// one propagating level by level), and one `Vec` per parent directory inside a `HashMap`.
// ============================================================================

/// Synthesized volume root. NTFS record 5 (`$Root`) is a metafile, so the record pass filters it
/// out (`MFT_METADATA_MAX`); the root node therefore has no record of its own.
const ROOT_MFT_ID: u64 = 5;
/// Directory node id of the volume root.
const ROOT_NODE: u32 = 0;
/// "No node" sentinel for parent references.
pub const NO_NODE: u32 = u32::MAX;
/// Depth of a directory that cannot be reached from the root — not part of this disk.
const UNREACHABLE_DEPTH: u16 = u16::MAX;

/// Slice of the name arena.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NameRef {
    offset: u32,
    length: u32,
}

/// Every directory and file name of one volume packed into a single buffer.
///
/// A 1.2M-record volume holds ~1.1M names. As `String`s that is 1.1M allocations, each carrying a
/// 16-32 byte allocator header — tens of MB of pure overhead — and a fragmented heap. Appending the
/// bytes to one buffer drops both. `(offset, length)` is `u32 + u32` on purpose: with a `u16`
/// length the struct would still be padded to 8 bytes, so the wider field is free.
#[allow(dead_code)]
#[derive(Default)]
pub struct NameArena {
    bytes: Vec<u8>,
}

#[allow(dead_code)]
impl NameArena {
    fn with_capacity(bytes: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(bytes),
        }
    }

    fn intern(&mut self, name: &str) -> NameRef {
        // 4GB of names is far past any real volume; fail loudly in debug rather than silently
        // handing out an offset that wraps.
        debug_assert!(self.bytes.len() + name.len() <= u32::MAX as usize);
        let offset = self.bytes.len() as u32;
        self.bytes.extend_from_slice(name.as_bytes());
        NameRef {
            offset,
            length: name.len() as u32,
        }
    }

    fn get(&self, reference: NameRef) -> &str {
        let start = reference.offset as usize;
        let end = start + reference.length as usize;
        // The arena only ever receives `&str` bytes, so this validation can never fail; it is
        // checked rather than assumed because a wrong name silently produces a path that does
        // not exist on disk, and those paths are handed to the delete / snapshot code.
        std::str::from_utf8(&self.bytes[start..end]).expect("名字 arena 只存 &str 的字节")
    }
}

/// One node of the volume tree. Files and directories share this array; whether a node is a
/// directory is implied by its id, not stored (see `VolumeIndex`).
#[allow(dead_code)]
#[derive(Clone, Copy)]
struct VolumeNode {
    /// Parent directory node, or `NO_NODE` when the parent is not a directory reachable from the root.
    parent: u32,
    name: NameRef,
    size: u64,
    /// Milliseconds since the Unix epoch so both consumers can share one aggregation; directories are 0.
    modified: i64,
    has_data_stream: bool,
}

/// One file as seen by a consumer: enough to build a path and to add its size to a total.
#[allow(dead_code)]
pub struct FileEntry<'a> {
    pub node: u32,
    /// Parent directory node, or `None` when the file is not reachable from the root.
    pub parent: Option<u32>,
    pub name: &'a str,
    pub size: u64,
    /// Milliseconds since the Unix epoch.
    pub modified: i64,
    /// Whether `$DATA` was parsed. A file without it has an unknown size until a stat returns.
    pub has_data_stream: bool,
}

/// Aggregated usage of one directory's whole subtree.
#[allow(dead_code)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct DirectoryTotals {
    pub size: u64,
    /// Newest file modification time in the subtree, in milliseconds since the Unix epoch.
    pub modified: i64,
    /// Number of files with a non-zero size in the subtree.
    pub file_count: u64,
}

#[allow(dead_code)]
impl DirectoryTotals {
    /// Fold one measured file into these totals. Both consumers use this so that "what counts as a
    /// file" cannot drift between them: only files with a non-zero size are counted.
    ///
    /// `modified` is **milliseconds** since the Unix epoch, like the field it updates. A caller that
    /// measured the file with `fs::metadata` gets seconds from `as_secs()` and must scale first;
    /// `DirectoryTotals::modified / 1000` converts back for consumers that report seconds.
    pub fn add_file(&mut self, size: u64, modified: i64) {
        self.size = self.size.saturating_add(size);
        self.file_count += 1;
        if modified > self.modified {
            self.modified = modified;
        }
    }

    fn add_subtree(&mut self, child: Self) {
        self.size = self.size.saturating_add(child.size);
        self.file_count = self.file_count.saturating_add(child.file_count);
        if child.modified > self.modified {
            self.modified = child.modified;
        }
    }

    /// A directory whose subtree holds no countable file. The consumers skip these, which is what
    /// keeps the result set equal to the old "only directories that appear in the stats map" rule.
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }
}

/// Dense, arena-backed index of one volume's `$MFT`.
///
/// Layout invariant: **directories occupy `0..directory_count`** (node 0 is the synthetic root) and
/// files occupy `directory_count..node_count`. That makes a node id double as an index into the
/// depth and totals arrays, and turns "iterate every file" into a slice range.
#[allow(dead_code)]
pub struct VolumeIndex {
    names: NameArena,
    nodes: Vec<VolumeNode>,
    directory_count: u32,
    /// Depth per directory node, `UNREACHABLE_DEPTH` when the branch does not reach the root.
    depth: Vec<u16>,
    /// Every reachable directory, parents before children; root first.
    bfs_order: Vec<u32>,
    deepest_depth: u16,
}

#[allow(dead_code)]
impl VolumeIndex {
    pub fn build(records: &[MftRecordInfo]) -> Self {
        let mut directory_count = 1usize; // the synthetic root
        let mut file_count = 0usize;
        let mut name_bytes = 0usize;
        for record in records {
            if record.mft_id == 0 || record.mft_id == ROOT_MFT_ID {
                continue;
            }
            name_bytes += record.name.len();
            if record.is_dir {
                directory_count += 1;
            } else {
                file_count += 1;
            }
        }

        let node_count = directory_count + file_count;
        let mut nodes: Vec<VolumeNode> = Vec::with_capacity(node_count);
        let mut names = NameArena::with_capacity(name_bytes);

        // Node 0 carries no record: its path is the drive root and its name is empty.
        nodes.push(VolumeNode {
            parent: NO_NODE,
            name: names.intern(""),
            size: 0,
            modified: 0,
            has_data_stream: false,
        });

        // Only directories can be parents, so only they need a by-id entry: ~1/6 of the records
        // instead of all of them.
        let mut by_mft_id: HashMap<u64, u32> = HashMap::with_capacity(directory_count);
        // Record 5 never reaches this function (the read filters metafiles), so the root has to be
        // registered by hand — without it every top-level directory loses its parent.
        by_mft_id.insert(ROOT_MFT_ID, ROOT_NODE);
        let mut pending_parents: Vec<u64> = Vec::with_capacity(directory_count);
        pending_parents.push(0);
        for record in records {
            if record.mft_id == 0 || record.mft_id == ROOT_MFT_ID || !record.is_dir {
                continue;
            }
            by_mft_id.insert(record.mft_id, nodes.len() as u32);
            pending_parents.push(record.parent_id);
            nodes.push(VolumeNode {
                parent: NO_NODE,
                name: names.intern(&record.name),
                size: 0,
                modified: 0,
                has_data_stream: false,
            });
        }

        // Parents are referenced by mft id, which can only be resolved once every directory is registered.
        for (node, parent_mft_id) in pending_parents.iter().enumerate().skip(1) {
            nodes[node].parent = by_mft_id.get(parent_mft_id).copied().unwrap_or(NO_NODE);
        }
        drop(pending_parents);

        for record in records {
            if record.mft_id == 0 || record.mft_id == ROOT_MFT_ID || record.is_dir {
                continue;
            }
            nodes.push(VolumeNode {
                parent: by_mft_id.get(&record.parent_id).copied().unwrap_or(NO_NODE),
                name: names.intern(&record.name),
                size: record.size,
                // Seconds are what the record holds; milliseconds are what the aggregation needs.
                modified: record.modified.saturating_mul(1000),
                has_data_stream: record.has_data_stream,
            });
        }
        debug_assert_eq!(nodes.len(), node_count);

        // Children grouped by parent in two linear passes (count, then fill), producing one flat
        // array. The previous shape allocated a `Vec` per parent directory.
        let mut child_offsets = vec![0u32; directory_count + 1];
        for entry in &nodes[1..directory_count] {
            if entry.parent != NO_NODE {
                child_offsets[entry.parent as usize + 1] += 1;
            }
        }
        for index in 1..child_offsets.len() {
            child_offsets[index] += child_offsets[index - 1];
        }
        let mut child_ids = vec![0u32; child_offsets[directory_count] as usize];
        let mut cursor = child_offsets.clone();
        for (node, entry) in nodes[1..directory_count].iter().enumerate() {
            if entry.parent == NO_NODE {
                continue;
            }
            let slot = &mut cursor[entry.parent as usize];
            child_ids[*slot as usize] = node as u32 + 1;
            *slot += 1;
        }

        // Breadth-first from the root: a directory's depth and its place in `bfs_order` are only
        // known once its parent has been visited, so the two passes cannot be merged.
        let mut depth = vec![UNREACHABLE_DEPTH; directory_count];
        let mut bfs_order = Vec::with_capacity(directory_count);
        depth[ROOT_NODE as usize] = 0;
        bfs_order.push(ROOT_NODE);
        let mut head = 0usize;
        while head < bfs_order.len() {
            let node = bfs_order[head];
            head += 1;
            let child_depth = depth[node as usize].saturating_add(1);
            let from = child_offsets[node as usize] as usize;
            let to = child_offsets[node as usize + 1] as usize;
            for &child in &child_ids[from..to] {
                if depth[child as usize] == UNREACHABLE_DEPTH {
                    depth[child as usize] = child_depth;
                    bfs_order.push(child);
                }
            }
        }
        drop(child_ids);
        drop(child_offsets);

        // Anything whose parent never received a depth sits on an orphan branch / self-parent /
        // cycle and is not part of this disk. Detach it once here so that path building and
        // aggregation never have to re-test reachability.
        for entry in nodes[1..].iter_mut() {
            if entry.parent != NO_NODE && depth[entry.parent as usize] == UNREACHABLE_DEPTH {
                entry.parent = NO_NODE;
            }
        }

        let deepest_depth = bfs_order
            .iter()
            .map(|&node| depth[node as usize])
            .max()
            .unwrap_or(0);

        Self {
            names,
            nodes,
            directory_count: directory_count as u32,
            depth,
            bfs_order,
            deepest_depth,
        }
    }

    /// Node id of the volume root (always 0).
    pub fn root(&self) -> u32 {
        ROOT_NODE
    }

    pub fn directory_count(&self) -> u32 {
        self.directory_count
    }

    pub fn node_count(&self) -> u32 {
        self.nodes.len() as u32
    }

    /// Empty for the root, whose path comes from the drive letter.
    pub fn name_of(&self, node: u32) -> &str {
        self.names.get(self.nodes[node as usize].name)
    }

    pub fn parent_of(&self, node: u32) -> Option<u32> {
        match self.nodes[node as usize].parent {
            NO_NODE => None,
            parent => Some(parent),
        }
    }

    /// `None` when the directory cannot be reached from the root.
    pub fn depth_of(&self, node: u32) -> Option<u16> {
        match self.depth[node as usize] {
            UNREACHABLE_DEPTH => None,
            depth => Some(depth),
        }
    }

    pub fn deepest_depth(&self) -> u16 {
        self.deepest_depth
    }

    /// Every reachable directory, parents before children, root first.
    ///
    /// A consumer builds its own path form by walking this once:
    /// `path[node] = join(path[parent], name_of(node))`.
    pub fn bfs_order(&self) -> &[u32] {
        &self.bfs_order
    }

    pub fn files(&self) -> impl Iterator<Item = FileEntry<'_>> + '_ {
        self.nodes[self.directory_count as usize..]
            .iter()
            .enumerate()
            .map(move |(offset, entry)| FileEntry {
                node: self.directory_count + offset as u32,
                parent: match entry.parent {
                    NO_NODE => None,
                    parent => Some(parent),
                },
                name: self.names.get(entry.name),
                size: entry.size,
                modified: entry.modified,
                has_data_stream: entry.has_data_stream,
            })
    }

    /// Fresh per-directory totals, indexed by node id.
    pub fn empty_totals(&self) -> Vec<DirectoryTotals> {
        vec![DirectoryTotals::default(); self.directory_count as usize]
    }

    /// Add every file whose size the `$MFT` already provided to its direct parent.
    ///
    /// Files without `$DATA` are skipped: their size is unknown until a stat returns, which only
    /// the caller can do (it needs a full path). Callers add those through
    /// `DirectoryTotals::add_file` themselves before calling `propagate_totals`.
    pub fn accumulate_files(&self, totals: &mut [DirectoryTotals]) {
        for entry in &self.nodes[self.directory_count as usize..] {
            if !entry.has_data_stream || entry.size == 0 || entry.parent == NO_NODE {
                continue;
            }
            totals[entry.parent as usize].add_file(entry.size, entry.modified);
        }
    }

    /// Hand each directory's subtree total to its parent, deepest level first.
    ///
    /// By the time a level is visited every child has its complete total, because `bfs_order`
    /// places parents before children and the loop runs in reverse. Cost is O(directories)
    /// regardless of tree depth, unlike the per-file ancestor walk it replaces.
    pub fn propagate_totals(&self, totals: &mut [DirectoryTotals]) {
        for &node in self.bfs_order.iter().rev() {
            let parent = self.nodes[node as usize].parent;
            // The root has no parent; detached nodes were already cleared during the build.
            if parent == NO_NODE {
                continue;
            }
            let child = totals[node as usize];
            totals[parent as usize].add_subtree(child);
        }
    }

    /// `$MFT`-sized files only. Callers that must stat the remaining files assemble the three
    /// steps themselves so they can inject those measurements in between.
    pub fn aggregate(&self) -> Vec<DirectoryTotals> {
        let mut totals = self.empty_totals();
        self.accumulate_files(&mut totals);
        self.propagate_totals(&mut totals);
        totals
    }
}

pub fn is_elevated() -> bool {
    let mut token = HANDLE::default();
    // SAFETY: token is the out-param; the pseudo process handle returned by
    // GetCurrentProcess is owned by the OS and never closed here
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut return_length: u32 = 0;
    // SAFETY: elevation is sized for TokenElevation and lives for the whole
    // call; token closes itself via HANDLE::drop (H4 contract §4.2)
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut core::ffi::c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut return_length,
        )
    };
    ok.is_ok() && elevation.TokenIsElevated != 0
}

pub fn is_ntfs(drive_letter: char) -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetVolumeInformationW;

    let r = format!("{}:\\", drive_letter);
    let w: Vec<u16> = r.encode_utf16().chain(std::iter::once(0)).collect();
    let mut fs: [u16; 16] = [0; 16];
    // SAFETY: w is NUL-terminated; fs is pinned for the duration of the call
    if unsafe {
        GetVolumeInformationW(
            PCWSTR::from_raw(w.as_ptr()),
            None,
            None,
            None,
            None,
            Some(&mut fs[..]),
        )
    }
    .is_err()
    {
        return false;
    }
    String::from_utf16_lossy(&fs)
        .trim_end_matches('\0')
        .eq_ignore_ascii_case("NTFS")
}

impl NtfsFileMetadataReader {
    pub fn open(drive_letter: char) -> Result<Self, String> {
        let volume_path = format!("\\\\.\\{}:", drive_letter);
        let volume = fs::OpenOptions::new()
            .read(true)
            .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE).0)
            // $MFT is read in whole runs in order; letting the system pre-read makes use of the disk's parallel capability
            .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN)
            .open(&volume_path)
            .map_err(|error| format!("打开 NTFS 卷失败: {}", error))?;

        let mut boot_sector = [0u8; 512];
        volume
            .seek_read(&mut boot_sector, 0)
            .map_err(|error| format!("读取 NTFS 引导扇区失败: {}", error))?;

        let bytes_per_sector = read_u16(&boot_sector, 11)
            .ok_or_else(|| "NTFS 引导扇区缺少 bytes_per_sector".to_string())?
            as usize;
        let sectors_per_cluster = boot_sector
            .get(13)
            .copied()
            .ok_or_else(|| "NTFS 引导扇区缺少 sectors_per_cluster".to_string())?
            as usize;
        let cluster_size = (bytes_per_sector * sectors_per_cluster) as u64;
        let mft_lcn =
            read_i64(&boot_sector, 48).ok_or_else(|| "NTFS 引导扇区缺少 MFT LCN".to_string())?;
        let file_record_size = decode_file_record_size(
            *boot_sector
                .get(64)
                .ok_or_else(|| "NTFS 引导扇区缺少 FILE record 大小".to_string())? as i8,
            cluster_size,
        );

        let mut reader = Self {
            volume,
            bytes_per_sector,
            cluster_size,
            file_record_size,
            // First read its own record using the $MFT start from the boot sector, then parse the real data runs.
            mft_runs: vec![DataRun {
                start_lcn: mft_lcn,
                cluster_count: 24,
            }],
        };

        if let Some(runs) = reader.read_mft_data_runs() {
            reader.mft_runs = runs;
        }

        Ok(reader)
    }

    /// Read the entire `$MFT`, parsing each valid record for name, parent-child relations, size and modification time.
    ///
    /// Return records and statistics. **Reading and parsing are timed separately**: only by splitting them can we judge whether the bottleneck is disk or CPU,
    /// otherwise we can only guess (the first two rounds of changes did not split them and picked the wrong direction).
    pub fn read_all_records(
        &self,
        progress_cb: &(impl Fn(usize) -> bool + Sync),
    ) -> Result<(Vec<MftRecordInfo>, MftReadStats), String> {
        let chunks = self.plan_read_chunks()?;
        // The record count of the whole table is roughly "start of last chunk + number of records in that chunk", with free records making up about half
        let capacity_hint = chunks
            .last()
            .map(|chunk| chunk.base_record_id + chunk.len as u64 / self.file_record_size as u64)
            .unwrap_or(0) as usize
            / 2;

        let (io_nanos, parse_nanos, records) =
            self.read_and_parse_pipelined(&chunks, capacity_hint, progress_cb)?;

        Ok((
            records,
            MftReadStats {
                fragments: self.mft_runs.len(),
                io_ms: (io_nanos / 1_000_000) as u64,
                parse_ms: (parse_nanos / 1_000_000) as u64,
            },
        ))
    }

    /// Split the entire `$MFT` into fixed-size read tasks.
    ///
    /// Compute the task list up front (rather than while reading) so the read thread only fetches data per task;
    /// record numbers are also computed here once, avoiding repeated derivation of segment starts for every chunk.
    fn plan_read_chunks(&self) -> Result<Vec<MftReadChunk>, String> {
        plan_read_chunks(&self.mft_runs, self.cluster_size, self.file_record_size)
    }

    /// Run reading and parsing in two threads simultaneously: the read thread loads data into buffers per task,
    /// and the parse thread returns buffers for reuse when done.
    ///
    /// Previously it was serial "read a chunk → parse a chunk": measured 1.9 s reading, 0.9 s parsing, 2.8 s combined;
    /// there is no data dependency between the two, so overlapping them makes this section take only the 1.9 s of reading.
    /// Keep only two buffers (16MB × 2); more just wastes memory.
    fn read_and_parse_pipelined(
        &self,
        chunks: &[MftReadChunk],
        capacity_hint: usize,
        progress_cb: &(impl Fn(usize) -> bool + Sync),
    ) -> Result<(u128, u128, Vec<MftRecordInfo>), String> {
        let (ready_tx, ready_rx) = mpsc::channel::<(Vec<u8>, &MftReadChunk)>();
        let (recycle_tx, recycle_rx) = mpsc::channel::<Vec<u8>>();

        std::thread::scope(|scope| {
            let reader = scope.spawn(move || -> Result<u128, String> {
                let mut io_nanos = 0u128;
                // Buffer cap: on a disk-cache hit reading is much faster than parsing; without a cap, new buffers keep being allocated,
                // and a 900MB $MFT can consume the whole memory block
                let mut buffers_alive = 0usize;

                for chunk in chunks {
                    // Buffers returned by the parse thread are reused directly, saving a fresh 16MB allocation per chunk
                    let mut buffer = if buffers_alive >= MAX_PIPELINE_BUFFERS {
                        // All buffers occupied means parsing has fallen behind; wait here to apply backpressure
                        recycle_rx
                            .recv()
                            .map_err(|_| "解析线程提前退出".to_string())?
                    } else {
                        match recycle_rx.try_recv() {
                            Ok(buffer) => buffer,
                            Err(_) => {
                                buffers_alive += 1;
                                vec![0u8; chunk.len]
                            }
                        }
                    };
                    if buffer.len() < chunk.len {
                        buffer.resize(chunk.len, 0);
                    }

                    let read_started = Instant::now();
                    self.volume
                        .seek_read(&mut buffer[..chunk.len], chunk.offset)
                        .map_err(|error| format!("读取 $MFT data run 失败: {}", error))?;
                    io_nanos += read_started.elapsed().as_nanos();

                    // If the parse thread has exited (cancelled), stop reading
                    if ready_tx.send((buffer, chunk)).is_err() {
                        break;
                    }
                }
                Ok(io_nanos)
            });

            let parser = scope.spawn(move || -> Result<(Vec<MftRecordInfo>, u128), String> {
                let mut records: Vec<MftRecordInfo> = Vec::with_capacity(capacity_hint);
                let mut parse_nanos = 0u128;
                let mut processed = 0usize;

                while let Ok((mut buffer, chunk)) = ready_rx.recv() {
                    let parse_started = Instant::now();
                    // Apply fixup directly on the buffer as read: previously each record was copied into scratch before fixing,
                    // which meant 700MB of needless copying for 710,000 records
                    for (record_index, record) in buffer[..chunk.len]
                        .chunks_exact_mut(self.file_record_size)
                        .enumerate()
                    {
                        let record_id = chunk.base_record_id + record_index as u64;
                        // Coarse-filter by header flags first: free records and system metadata records are the majority and not worth fixup
                        if record_id > MFT_METADATA_MAX
                            && looks_like_active_record(record)
                            && self.apply_fixup(record).is_some()
                        {
                            if let Some(info) = parse_record_info(record, record_id) {
                                records.push(info);
                            }
                        }

                        processed += 1;
                        if processed % 100_000 == 0 && !progress_cb(processed) {
                            return Err("扫描已取消".into());
                        }
                    }
                    parse_nanos += parse_started.elapsed().as_nanos();

                    // Return the buffer to the read thread; if the reader has ended (error or finished reading), discard it
                    let _ = recycle_tx.send(buffer);
                }

                Ok((records, parse_nanos))
            });

            let io_result = reader
                .join()
                .map_err(|_| "读取 $MFT 线程异常退出".to_string())?;
            let parse_result = parser
                .join()
                .map_err(|_| "解析 $MFT 线程异常退出".to_string())?;

            // Errors from the parse thread are closer to the root cause (cancellation returns from here), so report them first
            let (records, parse_nanos) = match parse_result {
                Ok(parsed) => parsed,
                Err(error) => return Err(error),
            };
            let io_nanos = io_result?;
            Ok((io_nanos, parse_nanos, records))
        })
    }

    fn read_mft_data_runs(&self) -> Option<Vec<DataRun>> {
        let mut record = vec![0u8; self.file_record_size];
        let first_run = self.mft_runs.first()?;
        let offset = (first_run.start_lcn as u64).checked_mul(self.cluster_size)?;
        self.volume.seek_read(&mut record, offset).ok()?;
        self.apply_fixup(&mut record)?;
        parse_file_record_data_runs(&record)
    }

    fn apply_fixup(&self, record: &mut [u8]) -> Option<()> {
        if record.len() < 8 || &record[0..4] != b"FILE" {
            return None;
        }

        let fixup_offset = read_u16(record, 4)? as usize;
        let fixup_count = read_u16(record, 6)? as usize;
        if fixup_count == 0 || fixup_offset + fixup_count * 2 > record.len() {
            return None;
        }

        let sectors_in_record = record.len() / self.bytes_per_sector;
        if fixup_count != sectors_in_record + 1 {
            return None;
        }

        let update_sequence = read_u16(record, fixup_offset)?;
        for sector_index in 0..sectors_in_record {
            let sector_tail = (sector_index + 1) * self.bytes_per_sector - 2;
            if read_u16(record, sector_tail)? != update_sequence {
                return None;
            }
            let replacement_offset = fixup_offset + (sector_index + 1) * 2;
            let replacement = read_u16(record, replacement_offset)?;
            write_u16(record, sector_tail, replacement)?;
        }

        Some(())
    }
}

/// Judge whether a record is in use from header flags only, without distinguishing files from directories.
fn looks_like_active_record(record: &[u8]) -> bool {
    if record.len() < 24 || &record[0..4] != b"FILE" {
        return false;
    }
    match read_u16(record, 22) {
        Some(flags) => (flags & FILE_RECORD_FLAG_IN_USE) != 0,
        None => false,
    }
}

/// Parse a record that has already been fixed up, extracting name, parent directory, size and timestamps.
///
/// Difference from `parse_active_file_record_metadata`: that one only cares about file size,
/// while here we also need `$FILE_NAME`, so directory records must be parsed too.
fn parse_record_info(record: &[u8], mft_id: u64) -> Option<MftRecordInfo> {
    let flags = read_u16(record, 22)?;
    if (flags & FILE_RECORD_FLAG_IN_USE) == 0 {
        return None;
    }
    let is_dir = (flags & FILE_RECORD_FLAG_DIRECTORY) != 0;

    let attributes_offset = read_u16(record, 20)? as usize;
    let mut offset = attributes_offset;
    let mut modified = 0i64;
    let mut file_attributes = 0u32;
    let mut name_entry: Option<(u64, String, u8)> = None;
    let mut data_size: Option<u64> = None;

    while offset + 16 <= record.len() {
        let attribute_type = read_u32(record, offset)?;
        if attribute_type == 0xFFFF_FFFF {
            break;
        }
        let attribute_length = read_u32(record, offset + 4)? as usize;
        if attribute_length == 0 || offset + attribute_length > record.len() {
            return None;
        }

        match attribute_type {
            // $STANDARD_INFORMATION: modification time + file attribute bits (to check for reparse point)
            0x10 => {
                if let Some((standard_modified, standard_attributes)) =
                    parse_standard_info(record, offset)
                {
                    modified = standard_modified;
                    file_attributes = standard_attributes;
                }
            }
            // $FILE_NAME: parent directory reference + file name. Hard links produce multiple entries; skip 8.3 short names
            0x30 => {
                if let Some(entry) = parse_file_name_location(record, offset) {
                    // Hard links and the 8.3 alias both produce extra `$FILE_NAME` attributes. Decide from
                    // the namespace first and decode only the winner: decoding a name that is immediately
                    // discarded costs one String per attribute, and a disk has millions of them.
                    let should_replace = match &name_entry {
                        None => true,
                        Some((_, _, existing_namespace)) => {
                            *existing_namespace == FILE_NAME_NAMESPACE_DOS
                                && entry.namespace != FILE_NAME_NAMESPACE_DOS
                        }
                    };
                    if should_replace {
                        if let Some(name) = decode_file_name(record, &entry) {
                            name_entry = Some((entry.parent_id, name, entry.namespace));
                        }
                    }
                }
            }
            // $DATA: only unnamed data stream; named streams are alternate data streams (ADS) and must not be used as file size
            0x80 => {
                if *record.get(offset + 9)? == 0 && data_size.is_none() {
                    data_size = parse_data_attribute_size(record, offset);
                }
            }
            _ => {}
        }

        offset += attribute_length;
    }

    // Reparse points (junction / symlink) are not real local files; keep consistent with filtering in the USN enumeration stage
    if file_attributes & FILE_ATTRIBUTE_REPARSE_POINT_BIT != 0 {
        return None;
    }

    let (parent_id, name, _) = name_entry?;
    Some(MftRecordInfo {
        mft_id,
        parent_id,
        name,
        is_dir,
        size: if is_dir { 0 } else { data_size.unwrap_or(0) },
        modified,
        has_data_stream: data_size.is_some(),
    })
}

/// Where a `$FILE_NAME` attribute's payload lives, plus the cheap fields needed to decide whether to keep it.
///
/// Split out from decoding so the parser can skip names it would discard anyway (8.3 alias of an entry
/// that already has a long name) without paying for a String.
struct FileNameLocation {
    parent_id: u64,
    namespace: u8,
    name_start: usize,
    name_length: usize,
}

/// Read the `$FILE_NAME` attribute header: parent directory reference, namespace and name span.
fn parse_file_name_location(record: &[u8], attribute_offset: usize) -> Option<FileNameLocation> {
    let value_offset = read_u16(record, attribute_offset + 20)? as usize;
    let value_start = attribute_offset + value_offset;
    let parent_id = read_u64(record, value_start)? & 0x0000_FFFF_FFFF_FFFF;
    let name_length = *record.get(value_start + 64)? as usize;
    let namespace = *record.get(value_start + 65)?;
    if name_length == 0 {
        return None;
    }
    let name_start = value_start + 66;
    // Half a UTF-16 unit cannot be decoded, and a truncated attribute must not index past the record.
    let name_end = name_start.checked_add(name_length.checked_mul(2)?)?;
    if name_end > record.len() {
        return None;
    }
    Some(FileNameLocation {
        parent_id,
        namespace,
        name_start,
        name_length,
    })
}

/// Decode a UTF-16LE name straight from the record bytes.
///
/// The previous version collected a `Vec<u16>` and then built a `String` from it: two allocations and two
/// copies per file name, which on a 1.2M-record volume is millions of avoidable heap operations on the
/// single parse thread. Unpaired surrogates still become U+FFFD, exactly like `String::from_utf16_lossy`.
fn decode_file_name(record: &[u8], location: &FileNameLocation) -> Option<String> {
    let name_end = location
        .name_start
        .checked_add(location.name_length.checked_mul(2)?)?;
    let bytes = record.get(location.name_start..name_end)?;
    let units = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]));
    Some(
        char::decode_utf16(units)
            .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect(),
    )
}

fn parse_standard_info(record: &[u8], attribute_offset: usize) -> Option<(i64, u32)> {
    let value_offset = read_u16(record, attribute_offset + 20)? as usize;
    let value_start = attribute_offset + value_offset;
    let modified_filetime = read_u64(record, value_start + 8)?;
    let file_attributes = read_u32(record, value_start + 32)?;
    Some((filetime_to_unix_seconds(modified_filetime), file_attributes))
}

fn parse_data_attribute_size(record: &[u8], attribute_offset: usize) -> Option<u64> {
    let non_resident = *record.get(attribute_offset + 8)?;
    if non_resident == 0 {
        read_u32(record, attribute_offset + 16).map(|size| size as u64)
    } else {
        read_u64(record, attribute_offset + 48)
    }
}

fn parse_file_record_data_runs(record: &[u8]) -> Option<Vec<DataRun>> {
    let attributes_offset = read_u16(record, 20)? as usize;
    let mut offset = attributes_offset;

    while offset + 16 <= record.len() {
        let attribute_type = read_u32(record, offset)?;
        if attribute_type == 0xFFFF_FFFF {
            break;
        }

        let attribute_length = read_u32(record, offset + 4)? as usize;
        if attribute_length == 0 || offset + attribute_length > record.len() {
            return None;
        }

        if attribute_type == 0x80 && *record.get(offset + 8)? == 1 && *record.get(offset + 9)? == 0
        {
            let data_run_offset = read_u16(record, offset + 32)? as usize;
            let data_run_start = offset + data_run_offset;
            let data_run_end = offset + attribute_length;
            return parse_data_runs(record.get(data_run_start..data_run_end)?);
        }

        offset += attribute_length;
    }

    None
}

fn parse_data_runs(bytes: &[u8]) -> Option<Vec<DataRun>> {
    let mut runs = Vec::new();
    let mut offset = 0usize;
    let mut current_lcn = 0i64;

    while offset < bytes.len() {
        let header = *bytes.get(offset)?;
        offset += 1;
        if header == 0 {
            break;
        }

        let length_size = (header & 0x0F) as usize;
        let offset_size = (header >> 4) as usize;
        if length_size == 0
            || length_size > 8
            || offset_size == 0
            || offset_size > 8
            || offset + length_size + offset_size > bytes.len()
        {
            return None;
        }

        let cluster_count = read_unsigned_le(&bytes[offset..offset + length_size]);
        offset += length_size;
        let lcn_delta = read_signed_le(&bytes[offset..offset + offset_size]);
        offset += offset_size;

        current_lcn = current_lcn.checked_add(lcn_delta)?;
        if current_lcn < 0 {
            return None;
        }

        runs.push(DataRun {
            start_lcn: current_lcn,
            cluster_count,
        });
    }

    if runs.is_empty() {
        None
    } else {
        Some(runs)
    }
}

fn decode_file_record_size(raw: i8, cluster_size: u64) -> usize {
    if raw > 0 {
        raw as usize * cluster_size as usize
    } else {
        1usize << (-raw as usize)
    }
}

fn filetime_to_unix_seconds(filetime: u64) -> i64 {
    let seconds = (filetime / 10_000_000) as i64 - WINDOWS_TO_UNIX_SECONDS;
    seconds.max(0)
}

fn read_u16(buffer: &[u8], offset: usize) -> Option<u16> {
    let bytes = buffer.get(offset..offset + 2)?;
    Some(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn write_u16(buffer: &mut [u8], offset: usize, value: u16) -> Option<()> {
    let bytes = buffer.get_mut(offset..offset + 2)?;
    bytes.copy_from_slice(&value.to_le_bytes());
    Some(())
}

fn read_u32(buffer: &[u8], offset: usize) -> Option<u32> {
    let bytes = buffer.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_u64(buffer: &[u8], offset: usize) -> Option<u64> {
    let bytes = buffer.get(offset..offset + 8)?;
    Some(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

fn read_i64(buffer: &[u8], offset: usize) -> Option<i64> {
    let bytes = buffer.get(offset..offset + 8)?;
    Some(i64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

fn read_unsigned_le(bytes: &[u8]) -> u64 {
    let mut value = 0u64;
    for (index, byte) in bytes.iter().enumerate() {
        value |= (*byte as u64) << (index * 8);
    }
    value
}

fn read_signed_le(bytes: &[u8]) -> i64 {
    if bytes.is_empty() {
        return 0;
    }

    let mut value = read_unsigned_le(bytes) as i64;
    let sign_bit = 1i64 << (bytes.len() * 8 - 1);
    if value & sign_bit != 0 {
        value -= 1i64 << (bytes.len() * 8);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read-task splitting must be **seamless, non-overlapping, and continuous in record numbers**.
    ///
    /// The pipeline splits "read" and "parse" into two threads; if a chunk boundary is computed wrong, what is lost is a whole chunk of records or
    /// half of a single record — showing up as a scan result with a few hundred fewer entries than real files, hard to notice.
    #[test]
    fn plan_read_chunks_tiles_every_run_without_gaps() {
        // First segment of 32MB (split into two chunks) plus a tail shorter than one chunk, record size 1KB
        let runs = vec![
            DataRun {
                start_lcn: 2,
                cluster_count: 8192, // 4096-byte cluster → 32MB
            },
            DataRun {
                start_lcn: 100_000,
                cluster_count: 64, // 256KB
            },
        ];
        let cluster_size = 4096u64;
        let chunks = plan_read_chunks(&runs, cluster_size, 1024).unwrap();

        let total_run_bytes: u64 = runs
            .iter()
            .map(|run| run.cluster_count * cluster_size)
            .sum();
        let total_chunk_bytes: u64 = chunks.iter().map(|chunk| chunk.len as u64).sum();
        assert_eq!(total_chunk_bytes, total_run_bytes, "块总长必须覆盖整张表");

        // Every chunk starts on a whole record and its length is an integer multiple of the record size
        for chunk in &chunks {
            assert_eq!(chunk.offset % 1024, 0);
            assert_eq!(chunk.len % 1024, 0);
        }

        // Adjacent chunks: record numbers must continue from the previous chunk's count; switching data run must not restart from 0
        for pair in chunks.windows(2) {
            let (previous, current) = (&pair[0], &pair[1]);
            let records_in_previous = previous.len as u64 / 1024;
            assert_eq!(
                current.base_record_id,
                previous.base_record_id + records_in_previous,
                "记录号断在偏移 {} 处",
                current.offset
            );
        }
        assert_eq!(chunks.first().unwrap().offset, 2 * cluster_size);
    }

    /// Write a resident attribute header. value_offset points to where the attribute value sits in the record.
    fn write_attribute_header(
        record: &mut [u8],
        offset: usize,
        attribute_type: u32,
        length: usize,
        value_offset: u16,
    ) {
        record[offset..offset + 4].copy_from_slice(&attribute_type.to_le_bytes());
        record[offset + 4..offset + 8].copy_from_slice(&(length as u32).to_le_bytes());
        record[offset + 8] = 0; // resident
        record[offset + 9] = 0; // no attribute name
        record[offset + 20..offset + 22].copy_from_slice(&value_offset.to_le_bytes());
    }

    /// Assemble an ordinary file like "report.log" into a minimal $MFT record.
    fn build_file_record(name: &str, size: u32, parent_id: u64, file_attributes: u32) -> Vec<u8> {
        let mut record = vec![0u8; 1024];
        record[0..4].copy_from_slice(b"FILE");
        record[20..22].copy_from_slice(&56u16.to_le_bytes());
        // in use, not a directory
        record[22..24].copy_from_slice(&FILE_RECORD_FLAG_IN_USE.to_le_bytes());

        // $STANDARD_INFORMATION: modification time + file attribute bits
        let standard_info = 56;
        write_attribute_header(&mut record, standard_info, 0x10, 96, 24);
        let filetime = ((1_700_000_000i64 + WINDOWS_TO_UNIX_SECONDS) * 10_000_000) as u64;
        record[standard_info + 24 + 8..standard_info + 24 + 16]
            .copy_from_slice(&filetime.to_le_bytes());
        record[standard_info + 24 + 32..standard_info + 24 + 36]
            .copy_from_slice(&file_attributes.to_le_bytes());

        // $FILE_NAME: parent directory reference + file name (Win32 long name)
        let file_name = standard_info + 96;
        let name_units: Vec<u16> = name.encode_utf16().collect();
        let file_name_length = (24 + 66 + name_units.len() * 2 + 7) / 8 * 8;
        write_attribute_header(&mut record, file_name, 0x30, file_name_length, 24);
        record[file_name + 24..file_name + 32].copy_from_slice(&parent_id.to_le_bytes());
        record[file_name + 24 + 64] = name_units.len() as u8;
        record[file_name + 24 + 65] = 1;
        for (index, unit) in name_units.iter().enumerate() {
            let at = file_name + 24 + 66 + index * 2;
            record[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        }

        // $DATA: unnamed data stream, size in the attribute header
        let data = file_name + file_name_length;
        write_attribute_header(&mut record, data, 0x80, 24, 24);
        record[data + 16..data + 20].copy_from_slice(&size.to_le_bytes());

        // End-of-attribute-list marker
        record[data + 24..data + 28].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        record
    }

    /// One record yields name, parent directory and size at once — the prerequisite for a single-pass read.
    #[test]
    fn parse_record_info_reads_name_parent_and_size() {
        let record = build_file_record("报表.log", 1024, 1234, 0);
        let info = parse_record_info(&record, 42).expect("应该能解析");

        assert_eq!(info.mft_id, 42);
        assert_eq!(info.parent_id, 1234);
        assert_eq!(info.name, "报表.log");
        assert!(!info.is_dir);
        assert_eq!(info.size, 1024);
        assert_eq!(info.modified, 1_700_000_000);
    }

    /// Reparse points (junction / symlink) must be skipped, otherwise links are treated as real files.
    #[test]
    fn reparse_point_is_skipped() {
        let record = build_file_record("link", 0, 1234, FILE_ATTRIBUTE_REPARSE_POINT_BIT);
        assert!(parse_record_info(&record, 42).is_none());
    }

    /// Records without "FILE" in the header (free or corrupted) are discarded directly, without panic.
    #[test]
    fn invalid_record_is_ignored() {
        let mut record = build_file_record("x.txt", 10, 1234, 0);
        record[0..4].copy_from_slice(b"BAAD");
        assert!(!looks_like_active_record(&record));
        record[0..4].copy_from_slice(b"FILE");
        // Clear the "in use" flag
        record[22..24].copy_from_slice(&0u16.to_le_bytes());
        assert!(!looks_like_active_record(&record));
    }

    /// Build a record for the volume-index tests. Files are given a `$DATA` stream unless cleared.
    fn record(
        mft_id: u64,
        parent_id: u64,
        name: &str,
        is_dir: bool,
        size: u64,
        modified: i64,
    ) -> MftRecordInfo {
        MftRecordInfo {
            mft_id,
            parent_id,
            name: name.to_string(),
            is_dir,
            size,
            modified,
            has_data_stream: !is_dir,
        }
    }

    /// ```
    /// 5 ─┬─ 10 Users ── 11 Alice ─┬─ 20 a.txt        100 B
    ///    │                        ├─ 21 b.bin       2000 B
    ///    │                        └─ 22 empty.log      0 B
    ///    ├─ 12 Windows ── 24 nostream.dat   ($DATA missing, size unknown)
    ///    └─ 23 pagefile.sys  5000 B
    /// ```
    fn sample_volume() -> Vec<MftRecordInfo> {
        vec![
            record(10, 5, "Users", true, 0, 0),
            record(11, 10, "Alice", true, 0, 0),
            record(12, 5, "Windows", true, 0, 0),
            record(20, 11, "a.txt", false, 100, 1_700_000_100),
            record(21, 11, "b.bin", false, 2000, 1_700_000_300),
            record(22, 11, "empty.log", false, 0, 1_700_000_500),
            record(23, 5, "pagefile.sys", false, 5000, 1_700_000_200),
            MftRecordInfo {
                has_data_stream: false,
                ..record(24, 12, "nostream.dat", false, 0, 1_700_000_000)
            },
        ]
    }

    /// The replaced implementation, kept as the oracle: every file climbs its parent chain and adds
    /// its size at each ancestor. Quadratic in depth, but obviously correct.
    ///
    /// Returns totals keyed by directory mft id; a directory absent from the map has an empty subtree.
    fn oracle_totals(records: &[MftRecordInfo]) -> HashMap<u64, (u64, i64, u64)> {
        let dir_parent: HashMap<u64, u64> = records
            .iter()
            .filter(|record| record.is_dir && record.mft_id != 0)
            .map(|record| (record.mft_id, record.parent_id))
            .collect();

        // Walk up to the root. `None` means the chain breaks (unknown parent) or loops.
        let ancestor_chain = |start: u64| -> Option<Vec<u64>> {
            let mut chain = vec![start];
            let mut current = start;
            while current != ROOT_MFT_ID {
                if chain.len() > 4096 {
                    return None;
                }
                current = *dir_parent.get(&current)?;
                chain.push(current);
            }
            Some(chain)
        };

        let mut totals: HashMap<u64, (u64, i64, u64)> = HashMap::new();
        for record in records {
            if record.is_dir || !record.has_data_stream || record.size == 0 {
                continue;
            }
            let Some(chain) = ancestor_chain(record.parent_id) else {
                continue;
            };
            for directory in chain {
                let totals = totals.entry(directory).or_default();
                totals.0 += record.size;
                totals.1 = totals.1.max(record.modified.saturating_mul(1000));
                totals.2 += 1;
            }
        }
        totals
    }

    /// Directory mft ids in node order: node 0 is the synthetic root, then directories in record
    /// order. Mirrors the layout invariant that `VolumeIndex` documents.
    fn directory_mft_ids(records: &[MftRecordInfo]) -> Vec<u64> {
        let mut ids = vec![ROOT_MFT_ID];
        ids.extend(
            records
                .iter()
                .filter(|record| record.is_dir && record.mft_id != 0)
                .map(|record| record.mft_id),
        );
        ids
    }

    /// Depth is what both consumers use to cap the result set, and it is derived only by the BFS.
    #[test]
    fn volume_index_assigns_depth_from_the_root() {
        let index = VolumeIndex::build(&sample_volume());
        let ids = directory_mft_ids(&sample_volume());
        let node_of = |mft_id: u64| ids.iter().position(|id| *id == mft_id).unwrap() as u32;

        assert_eq!(index.depth_of(index.root()), Some(0));
        assert_eq!(index.depth_of(node_of(10)), Some(1));
        assert_eq!(index.depth_of(node_of(11)), Some(2));
        assert_eq!(index.depth_of(node_of(12)), Some(1));
        assert_eq!(index.deepest_depth(), 2);

        // The root has no name; its path is the drive letter.
        assert_eq!(index.name_of(index.root()), "");
        assert_eq!(index.name_of(node_of(11)), "Alice");
        assert_eq!(index.parent_of(node_of(11)), Some(node_of(10)));
        assert_eq!(index.parent_of(index.root()), None);
    }

    /// `bfs_order` is the contract consumers build paths on: parents strictly before children.
    #[test]
    fn volume_index_bfs_order_visits_parents_before_children() {
        let records = sample_volume();
        let index = VolumeIndex::build(&records);
        let ids = directory_mft_ids(&records);

        let mut paths = vec![String::new(); index.node_count() as usize];
        paths[index.root() as usize] = "c:".to_string();
        // Every node except the root already has its parent's path built.
        for &node in index.bfs_order().iter().skip(1) {
            let parent = index.parent_of(node).expect("可达目录必须有父目录");
            let mut path = paths[parent as usize].clone();
            path.push('/');
            path.push_str(index.name_of(node));
            paths[node as usize] = path;
        }

        let path_of = |mft_id: u64| paths[ids.iter().position(|id| *id == mft_id).unwrap()].clone();
        assert_eq!(path_of(5), "c:");
        assert_eq!(path_of(10), "c:/Users");
        assert_eq!(path_of(11), "c:/Users/Alice");
        assert_eq!(path_of(12), "c:/Windows");
        // A directory's depth is exactly its number of path segments below the drive root.
        assert_eq!(path_of(11).matches('/').count(), 2);
    }

    /// The bottom-up aggregation must reproduce the per-file ancestor walk it replaces, field by field.
    #[test]
    fn volume_index_totals_match_the_per_file_ancestor_walk() {
        let records = sample_volume();
        let index = VolumeIndex::build(&records);
        let ids = directory_mft_ids(&records);
        let expected = oracle_totals(&records);

        let totals = index.aggregate();
        assert_eq!(totals.len(), ids.len(), "每个目录节点都要有一份 totals");

        for (node, mft_id) in ids.iter().enumerate() {
            let actual = totals[node];
            let want = expected.get(mft_id).copied().unwrap_or_default();
            assert_eq!(
                (actual.size, actual.modified, actual.file_count),
                want,
                "目录 {mft_id} 的聚合结果不一致"
            );
        }

        // Sanity check on the fixture itself, so a broken oracle cannot pass by agreeing on zeros.
        let alice = ids.iter().position(|id| *id == 11).unwrap();
        assert_eq!(totals[alice].size, 2100, "空文件不计入大小");
        assert_eq!(totals[alice].file_count, 2, "nostream.dat 与空文件都不计数");
        let root = index.root() as usize;
        assert_eq!(totals[root].size, 7100);
        assert_eq!(totals[root].file_count, 3);
    }

    /// Branches that do not reach the root are not on this disk: no depth, no parent, no totals.
    ///
    /// The old code expressed this as "the parent is not in the directory index"; here it is a
    /// one-off detach during the build, so every later pass can trust `parent`.
    #[test]
    fn unreachable_branches_are_detached() {
        let mut records = sample_volume();
        // Unknown parent, self-parent, and a two-node cycle.
        records.push(record(30, 99, "Orphan", true, 0, 0));
        records.push(record(31, 30, "x.txt", false, 400, 1_700_000_400));
        records.push(record(40, 40, "Self", true, 0, 0));
        records.push(record(50, 51, "CycleA", true, 0, 0));
        records.push(record(51, 50, "CycleB", true, 0, 0));

        let index = VolumeIndex::build(&records);
        let ids = directory_mft_ids(&records);
        let node_of = |mft_id: u64| ids.iter().position(|id| *id == mft_id).unwrap() as u32;

        for mft_id in [30, 40, 50, 51] {
            assert_eq!(index.depth_of(node_of(mft_id)), None, "{mft_id} 不可达");
            assert_eq!(
                index.parent_of(node_of(mft_id)),
                None,
                "{mft_id} 应被摘除父引用"
            );
            assert!(
                !index.bfs_order().contains(&node_of(mft_id)),
                "{mft_id} 不应出现在 BFS 序列里"
            );
        }

        // Files under an orphan directory are detached as well, so their size never lands anywhere.
        let orphan_file = index
            .files()
            .find(|file| file.name == "x.txt")
            .expect("文件仍应可枚举");
        assert_eq!(orphan_file.parent, None);

        let totals = index.aggregate();
        assert_eq!(totals[node_of(30) as usize], DirectoryTotals::default());
        assert_eq!(
            totals[index.root() as usize].size,
            7100,
            "孤儿分支不得计入根"
        );
    }

    /// Consumers rely on "directories first, files after": node ids index the totals and depth
    /// arrays, and the file range is a plain slice. Breaking this would silently misalign both.
    #[test]
    fn volume_index_lays_out_directories_before_files() {
        let records = sample_volume();
        let index = VolumeIndex::build(&records);
        let directories = records.iter().filter(|record| record.is_dir).count();
        let files = records.len() - directories;

        // Synthetic root + one node per directory record, then every file.
        assert_eq!(index.directory_count(), (directories + 1) as u32);
        assert_eq!(index.node_count(), (directories + 1 + files) as u32);
        // The file range starts exactly where the directory nodes end.
        let names: Vec<&str> = index.files().map(|file| file.name).collect();
        assert_eq!(
            names,
            vec![
                "a.txt",
                "b.bin",
                "empty.log",
                "pagefile.sys",
                "nostream.dat"
            ]
        );
    }

    /// Names feed the rebuilt paths shown to users and used for deletion, so the arena must not
    /// mangle anything the old `String` path preserved.
    #[test]
    fn name_arena_round_trips_non_ascii_names() {
        let records = vec![
            record(10, 5, "项目资料", true, 0, 0),
            record(20, 10, "报表😀.LOG", false, 10, 1),
        ];
        let index = VolumeIndex::build(&records);

        assert_eq!(index.name_of(1), "项目资料");
        let file = index.files().next().expect("应有一个文件");
        assert_eq!(file.name, "报表😀.LOG");
        assert_eq!(file.parent, Some(1));
    }

    /// `propagate_totals` is called after the caller injects stat results, so it must tolerate a
    /// totals array that already carries values (that is exactly how the fallback files get in).
    #[test]
    fn injected_file_sizes_propagate_to_the_root() {
        let records = sample_volume();
        let index = VolumeIndex::build(&records);
        let ids = directory_mft_ids(&records);
        let windows = ids.iter().position(|id| *id == 12).unwrap();

        let mut totals = index.empty_totals();
        index.accumulate_files(&mut totals);
        // Stand-in for the stat pass over `nostream.dat`, newest file on the volume.
        let stat_modified_ms = 1_700_000_700_000i64;
        totals[windows].add_file(4096, stat_modified_ms);
        index.propagate_totals(&mut totals);

        assert_eq!(totals[windows].size, 4096);
        assert_eq!(totals[windows].file_count, 1);
        assert_eq!(totals[index.root() as usize].size, 7100 + 4096);
        assert_eq!(totals[index.root() as usize].file_count, 4);
        assert_eq!(
            totals[index.root() as usize].modified,
            stat_modified_ms,
            "最新的修改时间要冒到根"
        );
    }

    /// The allocation-free decoder must produce exactly what `String::from_utf16_lossy` produced.
    ///
    /// These names become part of the rebuilt full path, which is shown to the user and handed to the
    /// delete paths, so a different replacement character or a truncated pair would turn into a path that
    /// does not exist on disk.
    #[test]
    fn name_decoding_matches_from_utf16_lossy() {
        let cases: Vec<Vec<u16>> = vec![
            "report.txt".encode_utf16().collect(),
            "报表.LOG".encode_utf16().collect(),
            // Emoji: a surrogate pair that must decode as one char
            "😀模型.bin".encode_utf16().collect(),
            // Lone high surrogate: must fall back to U+FFFD, same as from_utf16_lossy
            vec![0x41, 0xD800, 0x2E, 0x74, 0x78, 0x74],
        ];

        for units in cases {
            let mut buffer: Vec<u8> = vec![0u8; 8];
            for unit in &units {
                buffer.extend_from_slice(&unit.to_le_bytes());
            }
            let location = FileNameLocation {
                parent_id: 0,
                namespace: 1,
                name_start: 8,
                name_length: units.len(),
            };
            assert_eq!(
                decode_file_name(&buffer, &location).unwrap(),
                String::from_utf16_lossy(&units),
                "decoded name diverged for {units:?}"
            );
        }
    }
}
