// ============================================================================
// MFT core — sequential $MFT reading and record parsing (shared by junk cleanup / disk changes / big directories / big files)
// ============================================================================

#![cfg(target_os = "windows")]

use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::sync::atomic::{AtomicUsize, Ordering};
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

/// One record whose unnamed resident `$DATA` content was extracted (recycle-bin `$I` reader use case).
///
/// A dedicated type instead of an extra field on `MftRecordInfo`: existing callers should not
/// pay an `Option<Vec<u8>>` per record for a feature only one consumer needs (只增不改 contract §3.2).
pub struct ResidentDataRecord {
    /// Same parse product as `read_all_records` yields.
    pub info: MftRecordInfo,
    /// Unnamed resident `$DATA` content bytes. An `$I` file is at most ~550 bytes, so this is
    /// bounded by the record size (1KB/4KB). **Empty means the content could not be extracted**
    /// (non-resident stream, zero length, or malformed attribute) — callers fall back to
    /// reading the file from disk.
    pub data: Vec<u8>,
}

/// Statistics for one full read with resident-content extraction.
#[derive(Debug, Clone, Copy, Default)]
pub struct ResidentDataReadStats {
    /// Active records parsed (before `filter`).
    pub records_seen: usize,
    /// Records that passed `filter` **and** yielded non-empty resident content.
    pub resident_extracted: usize,
    /// Time spent waiting for the disk to return data (same accounting as `MftReadStats`).
    pub io_ms: u64,
    /// Time spent parsing records and copying resident content.
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
    /// Human-readable summary of the `$MFT` layout: data runs, derived record count,
    /// and record/cluster sizes. Diagnostics for the known "position-based mft_id vs
    /// content-based parent reference" mismatch on some drives — compare the derived
    /// record count against the real `$MFT` size (`\\.\X:\$MFT` metadata) to spot a
    /// misparsed or truncated run chain.
    pub fn layout_summary(&self) -> String {
        let total_bytes: u64 = self
            .mft_runs
            .iter()
            .map(|run| run.cluster_count * self.cluster_size)
            .sum();
        let runs: Vec<String> = self
            .mft_runs
            .iter()
            .map(|run| {
                format!(
                    "[lcn {} + {} clusters = {} bytes @ offset {}]",
                    run.start_lcn,
                    run.cluster_count,
                    run.cluster_count * self.cluster_size,
                    (run.start_lcn as u64) * self.cluster_size
                )
            })
            .collect();
        format!(
            "cluster_size {} / record_size {} / runs {} 个共 {} 字节 → 推导记录 {} 条: {}",
            self.cluster_size,
            self.file_record_size,
            self.mft_runs.len(),
            total_bytes,
            total_bytes / self.file_record_size as u64,
            runs.join(" ")
        )
    }

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

        let (io_nanos, parse_nanos, records) = self.read_and_parse_pipelined(
            &chunks,
            capacity_hint,
            progress_cb,
            // The generic pipeline's default callback: exactly the behavior read_all_records
            // has always had. Equivalence is proven by the existing test suite.
            &|record: &[u8], record_id: u64| parse_record_info(record, record_id),
        )?;

        Ok((
            records,
            MftReadStats {
                fragments: self.mft_runs.len(),
                io_ms: (io_nanos / 1_000_000) as u64,
                parse_ms: (parse_nanos / 1_000_000) as u64,
            },
        ))
    }

    /// Read the entire `$MFT` like `read_all_records`, additionally extracting the unnamed
    /// resident `$DATA` content of records selected by `filter`.
    ///
    /// `filter` runs on the parse thread right after `parse_record_info`, so records it rejects
    /// never get their content copied — the pipeline stays allocation-free for the ~99% of
    /// records the caller does not care about. Records that pass the filter but yield no
    /// resident content (non-resident stream, zero length, malformed attribute) come back with
    /// an **empty** `data` for the caller to fall back to.
    pub fn read_all_records_with_resident_data(
        &self,
        filter: &(impl Fn(&MftRecordInfo) -> bool + Sync),
        progress_cb: &(impl Fn(usize) -> bool + Sync),
    ) -> Result<(Vec<ResidentDataRecord>, ResidentDataReadStats), String> {
        let records_seen = AtomicUsize::new(0);
        let resident_extracted = AtomicUsize::new(0);

        let parse = |record: &[u8], record_id: u64| -> Option<ResidentDataRecord> {
            let info = parse_record_info(record, record_id)?;
            records_seen.fetch_add(1, Ordering::Relaxed);
            if !filter(&info) {
                return None;
            }
            let data = extract_resident_data(record).unwrap_or_default();
            if !data.is_empty() {
                resident_extracted.fetch_add(1, Ordering::Relaxed);
            }
            Some(ResidentDataRecord { info, data })
        };

        let chunks = self.plan_read_chunks()?;
        let capacity_hint = chunks
            .last()
            .map(|chunk| chunk.base_record_id + chunk.len as u64 / self.file_record_size as u64)
            .unwrap_or(0) as usize
            / 2;

        let (io_nanos, parse_nanos, records) =
            self.read_and_parse_pipelined(&chunks, capacity_hint, progress_cb, &parse)?;

        Ok((
            records,
            ResidentDataReadStats {
                records_seen: records_seen.load(Ordering::Relaxed),
                resident_extracted: resident_extracted.load(Ordering::Relaxed),
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
    /// `parse_record` decides what one fixed-up record becomes (`T`). The default entry point
    /// `read_all_records` passes `parse_record_info`; `read_all_records_with_resident_data`
    /// passes a callback that also copies resident content — the pipeline itself (two threads,
    /// 16MB buffer pair, backpressure, buffer recycling) is shared and unchanged.
    ///
    /// Previously it was serial "read a chunk → parse a chunk": measured 1.9 s reading, 0.9 s parsing, 2.8 s combined;
    /// there is no data dependency between the two, so overlapping them makes this section take only the 1.9 s of reading.
    /// Keep only two buffers (16MB × 2); more just wastes memory.
    fn read_and_parse_pipelined<T: Send>(
        &self,
        chunks: &[MftReadChunk],
        capacity_hint: usize,
        progress_cb: &(impl Fn(usize) -> bool + Sync),
        parse_record: &(impl Fn(&[u8], u64) -> Option<T> + Sync),
    ) -> Result<(u128, u128, Vec<T>), String> {
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

            let parser = scope.spawn(move || -> Result<(Vec<T>, u128), String> {
                let mut records: Vec<T> = Vec::with_capacity(capacity_hint);
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
                            if let Some(item) = parse_record(record, record_id) {
                                records.push(item);
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

// ============================================================================
// [P6] 记录级取证视图（`#[cfg(test)]`：生产构建中不存在，零行为影响）
//
// 用途：给定一个**权威记录号**（来自 OS 文件句柄的文件索引），把该记录从卷上单独
// 读出来、应用 fixup、逐属性 dump，回答"这条记录到底长什么样、为什么没进结果集"。
// 与 `read_and_parse_pipelined` 是两条独立的读取路径（单记录 seek_read 而非 16MB 块），
// 因此它同时能判别"记录本身有问题"和"块读取/记录号映射有问题"。
// ============================================================================

#[cfg(test)]
#[derive(Debug, Default, Clone)]
pub struct ProbeRecordView {
    pub record_id: u64,
    pub offset: u64,
    pub magic_ok: bool,
    pub magic: [u8; 4],
    pub in_use: bool,
    pub is_dir: bool,
    pub hardlink_count: u16,
    pub used_size: u32,
    pub allocated_size: u32,
    pub fixup_ok: bool,
    /// 属性链上的 (type, attribute_length) 序列
    pub attributes: Vec<(u32, usize)>,
    /// $FILE_NAME: (parent_id, namespace, name)
    pub file_names: Vec<(u64, u8, String)>,
    /// $STANDARD_INFORMATION 的 file attributes 位（bit 0x400 = reparse point，解析器会丢弃）
    pub si_file_attributes: u32,
    /// $DATA: (是否非 resident, run 数)
    pub data_attributes: Vec<(bool, usize)>,
    /// $ATTRIBUTE_LIST: (attribute_type, 扩展记录号)
    pub attribute_list: Vec<(u32, u64)>,
    pub parse_ok: bool,
    pub parsed_name: Option<String>,
    pub parsed_parent: Option<u64>,
    /// 属性链遍历中途失败的说明（越界/长度非法），正常为 None
    pub walk_error: Option<String>,
}

#[cfg(test)]
impl ProbeRecordView {
    pub fn describe(&self) -> String {
        let attrs: Vec<String> = self
            .attributes
            .iter()
            .map(|(kind, length)| format!("0x{kind:X}({length})"))
            .collect();
        let names: Vec<String> = self
            .file_names
            .iter()
            .map(|(parent, namespace, name)| format!("(parent={parent},ns={namespace},{name:?})"))
            .collect();
        let data: Vec<String> = self
            .data_attributes
            .iter()
            .map(|(non_resident, runs)| {
                format!("({},runs={runs})", if *non_resident { "非resi" } else { "resi" })
            })
            .collect();
        let attr_list: Vec<String> = self
            .attribute_list
            .iter()
            .map(|(kind, record)| format!("0x{kind:X}->{record}"))
            .collect();
        format!(
            "rid={} off={} magic={} in_use={} dir={} links={} used={} alloc={} si_attr=0x{:X} fixup={} attrs=[{}] names=[{}] data=[{}] attrlist=[{}] parse={} parsed=({:?},{:?}) walk_err={:?}",
            self.record_id,
            self.offset,
            String::from_utf8_lossy(&self.magic),
            self.in_use,
            self.is_dir,
            self.hardlink_count,
            self.used_size,
            self.allocated_size,
            self.si_file_attributes,
            self.fixup_ok,
            attrs.join(" "),
            names.join(" "),
            data.join(" "),
            attr_list.join(" "),
            self.parse_ok,
            self.parsed_name,
            self.parsed_parent,
            self.walk_error,
        )
    }
}

/// 记录号 → 卷内字节偏移（按 run 链累计）。与 `plan_read_chunks` 口径一致。
#[cfg(test)]
impl NtfsFileMetadataReader {
    pub fn probe_record_offset(&self, record_id: u64) -> Option<u64> {
        let record_size = self.file_record_size as u64;
        let mut base = 0u64;
        for run in &self.mft_runs {
            let run_records = run.cluster_count * self.cluster_size / record_size;
            if record_id < base + run_records {
                return Some(
                    (run.start_lcn as u64) * self.cluster_size
                        + (record_id - base) * record_size,
                );
            }
            base += run_records;
        }
        None
    }

    /// 单独读一条记录并 dump 全貌。**不经过** 16MB 块流水线。
    pub fn probe_record(&self, record_id: u64) -> Result<ProbeRecordView, String> {
        let offset = self
            .probe_record_offset(record_id)
            .ok_or_else(|| format!("记录号 {record_id} 超出 run 链覆盖范围"))?;
        let mut raw = vec![0u8; self.file_record_size];
        let read = self
            .volume
            .seek_read(&mut raw, offset)
            .map_err(|error| format!("单记录读取失败: {error}"))?;
        if read != raw.len() {
            return Err(format!("单记录短读 {read}/{}", raw.len()));
        }

        let mut view = ProbeRecordView {
            record_id,
            offset,
            magic: [raw[0], raw[1], raw[2], raw[3]],
            ..ProbeRecordView::default()
        };
        view.magic_ok = &raw[0..4] == b"FILE";
        if !view.magic_ok {
            return Ok(view);
        }

        view.in_use = read_u16(&raw, 22).is_some_and(|flags| flags & FILE_RECORD_FLAG_IN_USE != 0);
        view.is_dir = read_u16(&raw, 22).is_some_and(|flags| flags & FILE_RECORD_FLAG_DIRECTORY != 0);
        view.hardlink_count = read_u16(&raw, 18).unwrap_or(0);
        view.used_size = read_u32(&raw, 24).unwrap_or(0);
        view.allocated_size = read_u32(&raw, 28).unwrap_or(0);

        let mut fixed = raw;
        view.fixup_ok = self.apply_fixup(&mut fixed).is_some();
        if !view.fixup_ok {
            return Ok(view);
        }

        let attributes_offset = read_u16(&fixed, 20).unwrap_or(0) as usize;
        let mut offset = attributes_offset;
        while offset + 16 <= fixed.len() {
            let Some(attribute_type) = read_u32(&fixed, offset) else {
                view.walk_error = Some(format!("读 type 越界 @{offset}"));
                break;
            };
            if attribute_type == 0xFFFF_FFFF {
                break;
            }
            let Some(attribute_length) = read_u32(&fixed, offset + 4).map(|v| v as usize) else {
                view.walk_error = Some(format!("读 length 越界 @{offset}"));
                break;
            };
            if attribute_length == 0 || offset + attribute_length > fixed.len() {
                view.walk_error = Some(format!(
                    "属性长度非法 0x{attribute_type:X} len={attribute_length} @{offset} (record_len={})",
                    fixed.len()
                ));
                break;
            }

            view.attributes.push((attribute_type, attribute_length));
            match attribute_type {
                0x10 => {
                    if let Some((_, file_attributes)) = parse_standard_info(&fixed, offset) {
                        view.si_file_attributes = file_attributes;
                    }
                }
                0x20 => {
                    view.attribute_list =
                        probe_attribute_list_entries(&fixed, offset).unwrap_or_default();
                }
                0x30 => {
                    if let Some((parent_id, name, namespace)) =
                        parse_file_name_attribute(&fixed, offset)
                    {
                        view.file_names.push((parent_id, namespace, name));
                    }
                }
                0x80 => {
                    let non_resident = *fixed.get(offset + 8).unwrap_or(&0) != 0;
                    let runs = if non_resident {
                        probe_data_run_count(&fixed, offset).unwrap_or(0)
                    } else {
                        0
                    };
                    view.data_attributes.push((non_resident, runs));
                }
                _ => {}
            }

            offset += attribute_length;
        }

        if let Some(info) = parse_record_info(&fixed, record_id) {
            view.parse_ok = true;
            view.parsed_name = Some(info.name);
            view.parsed_parent = Some(info.parent_id);
        }
        Ok(view)
    }
}

/// $ATTRIBUTE_LIST（resident）的条目：每条 26 字节，file_reference 低 48 位是扩展记录号。
#[cfg(test)]
fn probe_attribute_list_entries(record: &[u8], attribute_offset: usize) -> Option<Vec<(u32, u64)>> {
    if *record.get(attribute_offset + 8)? != 0 {
        return None; // 非 resident 属性列表：本议题场景不出现
    }
    let value_length = read_u32(record, attribute_offset + 16)? as usize;
    let value_offset = read_u16(record, attribute_offset + 20)? as usize;
    let start = attribute_offset.checked_add(value_offset)?;
    let end = start.checked_add(value_length)?.min(record.len());
    let value = record.get(start..end)?;

    let mut entries = Vec::new();
    let mut cursor = 0usize;
    while cursor + 26 <= value.len() {
        let attribute_type = read_u32(value, cursor)?;
        let length = read_u16(value, cursor + 4)? as usize;
        if length < 26 || cursor + length > value.len() {
            break;
        }
        let file_reference = read_u64(value, cursor + 16)? & 0x0000_FFFF_FFFF_FFFF;
        entries.push((attribute_type, file_reference));
        cursor += length;
    }
    Some(entries)
}

#[cfg(test)]
fn probe_data_run_count(record: &[u8], attribute_offset: usize) -> Option<usize> {
    let data_run_offset = read_u16(record, attribute_offset + 32)? as usize;
    let start = attribute_offset + data_run_offset;
    let length = read_u32(record, attribute_offset + 4)? as usize;
    let end = attribute_offset + length;
    parse_data_runs(record.get(start..end)?).map(|runs| runs.len())
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
                if let Some((parent_id, name, namespace)) =
                    parse_file_name_attribute(record, offset)
                {
                    let should_replace = match &name_entry {
                        None => true,
                        Some((_, _, existing_namespace)) => {
                            *existing_namespace == FILE_NAME_NAMESPACE_DOS
                                && namespace != FILE_NAME_NAMESPACE_DOS
                        }
                    };
                    if should_replace {
                        name_entry = Some((parent_id, name, namespace));
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

/// Parse the `$FILE_NAME` attribute: return (parent directory reference, file name, namespace).
fn parse_file_name_attribute(record: &[u8], attribute_offset: usize) -> Option<(u64, String, u8)> {
    let value_offset = read_u16(record, attribute_offset + 20)? as usize;
    let value_start = attribute_offset + value_offset;
    let parent_reference = read_u64(record, value_start)? & 0x0000_FFFF_FFFF_FFFF;
    let name_length = *record.get(value_start + 64)? as usize;
    let namespace = *record.get(value_start + 65)?;
    if name_length == 0 {
        return None;
    }
    let name_start = value_start + 66;
    let name_end = name_start + name_length * 2;
    if name_end > record.len() {
        return None;
    }
    // The file name is UTF-16LE; decode by slicing u16 directly, without extra copying
    let units: Vec<u16> = record[name_start..name_end]
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    Some((
        parent_reference,
        String::from_utf16_lossy(&units),
        namespace,
    ))
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

/// Extract the content bytes of an unnamed **resident** `$DATA` attribute from a fixup-ed record.
///
/// Offsets follow the same convention as `parse_data_attribute_size`'s resident branch: a resident
/// attribute header is 24 bytes, `value_length` (u32) sits at +16 and `value_offset` (u16) at +20.
/// `None` = no usable unnamed resident `$DATA` (attribute absent, non-resident stream, zero length,
/// or an attribute chain that fails bounds checks). `parse_record_info` itself is untouched — this
/// walks the attribute chain a second time on an already-fixed-up record, at negligible cost.
fn extract_resident_data(record: &[u8]) -> Option<Vec<u8>> {
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

        if attribute_type == 0x80 && *record.get(offset + 9)? == 0 {
            // Unnamed $DATA found; named streams (ADS) keep scanning past this branch.
            if *record.get(offset + 8)? != 0 {
                return None; // non-resident: content lives in external clusters
            }
            let value_length = read_u32(record, offset + 16)? as usize;
            if value_length == 0 {
                return None;
            }
            let value_offset = read_u16(record, offset + 20)? as usize;
            let value_start = offset.checked_add(value_offset)?;
            let value_end = value_start.checked_add(value_length)?;
            if value_end > record.len() {
                return None;
            }
            return Some(record[value_start..value_end].to_vec());
        }

        offset += attribute_length;
    }

    None
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

    /// Resident `$DATA` content extraction is the whole point of the recycle-bin `$I` reader:
    /// content lives directly in the record, replacing one random open/read per file.
    #[test]
    fn extract_resident_data_returns_content_bytes() {
        let mut record = vec![0u8; 1024];
        record[0..4].copy_from_slice(b"FILE");
        record[20..22].copy_from_slice(&56u16.to_le_bytes());
        record[22..24].copy_from_slice(&FILE_RECORD_FLAG_IN_USE.to_le_bytes());

        let data = 56;
        let content = b"$I-file-content-15B";
        let length = 24 + content.len();
        write_attribute_header(&mut record, data, 0x80, length, 24);
        record[data + 16..data + 20].copy_from_slice(&(content.len() as u32).to_le_bytes());
        record[data + 24..data + 24 + content.len()].copy_from_slice(content);
        record[data + length..data + length + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());

        let extracted = extract_resident_data(&record).expect("resident 内容应可提取");
        assert_eq!(extracted, content);
    }

    /// A non-resident stream has no content inside the record: `None` (caller falls back to
    /// reading the file from disk), never garbage bytes.
    #[test]
    fn extract_resident_data_rejects_non_resident_stream() {
        let mut record = vec![0u8; 1024];
        record[0..4].copy_from_slice(b"FILE");
        record[20..22].copy_from_slice(&56u16.to_le_bytes());
        record[22..24].copy_from_slice(&FILE_RECORD_FLAG_IN_USE.to_le_bytes());

        let data = 56;
        write_attribute_header(&mut record, data, 0x80, 48, 24);
        record[data + 8] = 1; // non-resident

        assert!(extract_resident_data(&record).is_none());
    }

    /// A named stream (ADS) before the unnamed one must be skipped, not mistaken for content.
    #[test]
    fn extract_resident_data_skips_named_streams() {
        let mut record = vec![0u8; 1024];
        record[0..4].copy_from_slice(b"FILE");
        record[20..22].copy_from_slice(&56u16.to_le_bytes());
        record[22..24].copy_from_slice(&FILE_RECORD_FLAG_IN_USE.to_le_bytes());

        // ADS first: named (name_length at +9 != 0), resident, with decoy content
        let ads = 56;
        write_attribute_header(&mut record, ads, 0x80, 48, 24);
        record[ads + 9] = 1;
        record[ads + 16..ads + 20].copy_from_slice(&16u32.to_le_bytes());
        record[ads + 24..ads + 40].copy_from_slice(b"ADS-decoy-bytes!");

        // Unnamed resident $DATA after it
        let data = ads + 48;
        let content = b"real-content";
        let length = 24 + content.len();
        write_attribute_header(&mut record, data, 0x80, length, 24);
        record[data + 16..data + 20].copy_from_slice(&(content.len() as u32).to_le_bytes());
        record[data + 24..data + 24 + content.len()].copy_from_slice(content);
        record[data + length..data + length + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());

        let extracted = extract_resident_data(&record).expect("应跳过 ADS 后提取未命名流");
        assert_eq!(extracted, content);
    }
}
