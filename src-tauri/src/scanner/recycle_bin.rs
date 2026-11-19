// ============================================================================
// Recycle bin scanner
// Only returns valid entries the current user can see in the Windows Recycle Bin UI
// ============================================================================

use super::{CategoryScanResult, FileInfo, JunkCategory};
use crate::scanner::big_files_engine::mft_core;
use log::{debug, info, warn};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(windows)]
use std::sync::{Mutex, OnceLock, PoisonError};
#[cfg(windows)]
use std::time::Instant;

/// Scans the recycle bin of the currently logged-in user.
///
/// The recycle bin directory contains multiple SIDs, orphan files, and shell metadata, so normal directory scanning cannot be reused,
/// otherwise it would incorrectly show content that is invisible when the user opens the Recycle Bin manually.
pub fn scan_current_user(category: &JunkCategory, result: &mut CategoryScanResult) {
    #[cfg(windows)]
    {
        let Some(user_sid) = current_user_sid() else {
            warn!("无法获取当前用户 SID，跳过回收站扫描");
            return;
        };

        let mut entries = Vec::new();
        let scan_started = Instant::now();
        let mut timings: Vec<DrivePhaseTiming> = Vec::new();
        for drive_letter in crate::sys_drive::existing_drive_letters() {
            let sid_path = PathBuf::from(format!("{}:\\$Recycle.Bin\\{}", drive_letter, user_sid));
            let (drive_entries, timing) = collect_drive_entries(&sid_path, drive_letter, category);
            timings.push(timing);
            entries.extend(drive_entries);
        }

        entries.sort_by(|left, right| left.path.cmp(&right.path));
        let entry_count = entries.len();
        for entry in entries {
            result.add_file(entry);
        }

        log_scan_phase_timings(&timings, scan_started.elapsed(), entry_count);
    }

    #[cfg(not(windows))]
    {
        let _ = (category, result);
    }
}

/// Validates whether a path belongs to recycle bin data files the current user can act on.
pub fn is_current_user_entry_path(path: &str) -> bool {
    #[cfg(windows)]
    {
        let Some(user_sid) = current_user_sid() else {
            return false;
        };
        let normalized = path.replace('/', "\\").to_ascii_lowercase();
        let marker = format!("\\$recycle.bin\\{}\\$r", user_sid.to_ascii_lowercase());
        return normalized.contains(&marker) && normalized.len() > marker.len();
    }

    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

#[cfg(windows)]
/// Per-drive phase timing of the dedicated recycle-bin scan, collected for the
/// wall-clock diagnosis: enumerate (read_dir + $I/$R pairing) vs parse
/// (parallel $I file reads) have different cost structures and different fixes.
struct DrivePhaseTiming {
    /// `"mft"`（两遍 $MFT 顺序读）或 `"fs"`（read_dir + rayon 并行 $I 读）。
    backend: &'static str,
    drive_letter: char,
    /// fs: read_dir enumeration + $I/$R pairing (serial, one thread).
    /// mft: 第一遍——全表顺序读并定位当前用户 SID 目录。
    enumerate_ms: u128,
    /// fs: parallel $I metadata reads and parsing.
    /// mft: 第二遍——按 filter 提取 $I resident 内容并解析。
    parse_ms: u128,
    /// fs: entries seen by read_dir ($I + $R + anything else in the SID dir).
    /// mft: 全表活跃记录数（口径不同，仅供诊断，勿直接对比）。
    directory_entries: usize,
    /// fs: $I metadata files matched by the pairing.
    /// mft: filter 命中且 resident 内容非空的记录数。
    metadata_files: usize,
    /// Recycle-bin entries emitted for this drive.
    found_entries: usize,
    /// mft: content 缺失（非 resident 等）退化为单独 fs::read 的条目数；fs 恒 0。
    non_resident_fallbacks: usize,
}

#[cfg(windows)]
fn collect_drive_entries(
    sid_path: &Path,
    drive_letter: char,
    category: &JunkCategory,
) -> (Vec<FileInfo>, DrivePhaseTiming) {
    let drive_started = Instant::now();
    let directory_entries = match fs::read_dir(sid_path) {
        Ok(entries) => entries,
        Err(error) => {
            debug!("无法读取回收站目录 {:?}: {}", sid_path, error);
            return (
                Vec::new(),
                DrivePhaseTiming {
                    backend: "fs",
                    drive_letter,
                    enumerate_ms: drive_started.elapsed().as_millis(),
                    parse_ms: 0,
                    directory_entries: 0,
                    metadata_files: 0,
                    found_entries: 0,
                    non_resident_fallbacks: 0,
                },
            );
        }
    };

    // One read_dir collects both $I and $R: the old implementation also did exists() + is_dir() for each entry
    // two stats; on a cold read, thousands of entries across four partitions meant tens of thousands of metadata accesses — measured first scan
    // of the recycle bin took 7.3 s, while a warm scan took only 2 s; the slowness was all here.
    // On Windows, DirEntry::file_type() comes directly from the directory enumeration result, with no extra system call.
    let enumerate_started = Instant::now();
    let mut metadata_files: Vec<(PathBuf, String)> = Vec::new();
    // Lowercase extensions before comparing: NTFS is case-insensitive, so the $I/$R base parts may not share the same casing
    let mut data_dirs: HashMap<String, bool> = HashMap::new();
    let mut seen_entries: usize = 0;

    for directory_entry in directory_entries.filter_map(Result::ok) {
        seen_entries += 1;
        let Ok(file_name) = directory_entry.file_name().into_string() else {
            continue;
        };
        if let Some(suffix) = file_name.strip_prefix("$I") {
            // A Windows recycle bin entry consists of $I metadata and $R data with the same suffix
            if !suffix.is_empty() {
                metadata_files.push((directory_entry.path(), suffix.to_owned()));
            }
        } else if let Some(suffix) = file_name.strip_prefix("$R") {
            if !suffix.is_empty() {
                let is_dir = directory_entry
                    .file_type()
                    .map(|kind| kind.is_dir())
                    .unwrap_or(false);
                data_dirs.insert(suffix.to_ascii_lowercase(), is_dir);
            }
        }
    }
    let enumerate_ms = enumerate_started.elapsed().as_millis();
    let metadata_count = metadata_files.len();

    // Each $I must be opened and read separately; they are fully independent, so they must run in parallel.
    // Reading tens of thousands of small files serially on a cold disk means tens of thousands of random I/Os:
    // measured with 33052 entries in the E drive recycle bin: first scan 16.3 s, warm scan only 1.3 s.
    let parse_started = Instant::now();
    let found: Vec<FileInfo> = metadata_files
        .into_par_iter()
        .filter_map(|(metadata_path, suffix)| {
            // A $I file missing its $R data is orphan metadata; Explorer will not show it as a recycle bin entry.
            let &is_dir = data_dirs.get(&suffix.to_ascii_lowercase())?;

            let (logical_size, original_path, deleted_at) = parse_metadata(&metadata_path)?;

            let visible_name = Path::new(&original_path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| original_path.clone());

            Some(
                FileInfo::new(
                    sid_path
                        .join(format!("$R{}", suffix))
                        .to_string_lossy()
                        .into_owned(),
                    visible_name,
                    logical_size,
                    deleted_at,
                    is_dir,
                    category.clone(),
                )
                .with_original_path(original_path),
            )
        })
        .collect();
    let parse_ms = parse_started.elapsed().as_millis();

    let timing = DrivePhaseTiming {
        backend: "fs",
        drive_letter,
        enumerate_ms,
        parse_ms,
        directory_entries: seen_entries,
        metadata_files: metadata_count,
        found_entries: found.len(),
        non_resident_fallbacks: 0,
    };
    (found, timing)
}

/// Resolve the SID directory's true MFT record number by opening it and asking the OS.
///
/// `GetFileInformationByHandle`'s file index IS the NTFS record reference (low 48 bits
/// record number, high 16 bits sequence) — the same encoding `$FILE_NAME` parent
/// references use, so no name matching is involved.
///
/// 名字匹配在这里被实测证伪：SID 目录的记录里可能只解析出 8.3 短名（C 盘实测
/// "S-1-5-~1"，完整 SID 42 字符的短名），而句柄文件索引是操作系统的唯一权威。
/// 无需管理员权限——当前用户对自己 SID 目录有完全控制权（H1 时已实测）。
#[cfg(windows)]
fn sid_dir_record_id_via_handle(sid_path: &Path) -> Result<u64, String> {
    file_record_id_via_handle(sid_path)
}

/// 任意路径（文件或目录）→ 权威 MFT 记录号（OS 文件索引低 48 位）。
#[cfg(windows)]
fn file_record_id_via_handle(path: &Path) -> Result<u64, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_READ, FILE_SHARE_MODE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let mut units: Vec<u16> = path.as_os_str().encode_wide().collect();
    units.push(0);

    // SAFETY: path is a NUL-terminated wide string valid for the whole call; the
    // handle closes itself via HANDLE::drop (H4 contract §4.2)
    let handle = unsafe {
        CreateFileW(
            PCWSTR::from_raw(units.as_ptr()),
            FILE_GENERIC_READ.0,
            FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0),
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
    .map_err(|error| format!("打开句柄失败: {}", error))?;

    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: info is the out-param and lives for the whole call
    unsafe { GetFileInformationByHandle(handle, &mut info) }
        .map_err(|error| format!("读取文件索引失败: {}", error))?;

    // 64 位文件索引 = MFT 引用：低 48 位记录号 + 高 16 位序号，掩码口径与
    // $FILE_NAME 引用一致
    Ok(((info.nFileIndexHigh as u64 & 0xFFFF) << 32) | info.nFileIndexLow as u64)
}

/// One drive's recycle-bin entries via two sequential `$MFT` passes instead of
/// ~100k random small-file reads.
///
/// 【实验性，未接入生产路径】真机对照（2026-11-19，4 个盘、D 盘 15.4 万条目）
/// 已做到 **与 fs 枚举逐条相等**（C 29204/29204、D 154163/154163、E 5/5、F 2/2）。
/// 曾经约 25% 的 $R 记录缺失，根因不是解析或读取，而是 `parse_record_info` 每条记录只留
/// 一个名字：rustc incremental 的硬链接被逐个改名进回收站，一条记录背上 2~40 个
/// `$FILE_NAME`，单名字扫描只登记了第一个（详见 `read_all_records_all_names`）。
/// 生产路径仍维持 fs 枚举（`collect_drive_entries`）。
///
/// 本函数与对照测试（`real_machine_mft_path_matches_fs_path`）保留，作为
/// mft_core 记录解析可靠性议题（联动 S6 引擎合并）的实验场与门槛。
///
/// Pass 1 (`read_all_records_all_names`) builds the $R-pairing table. It must be the
/// **all-names** entry point, not `read_all_records`: a record hosting a hard-link set is
/// several directory entries at once, and `parse_record_info` keeps only the first name, so a
/// single-name pass silently hides every `$R` link but the first one (measured on D:: 40,106 of
/// 152,551 entries, exactly the multi-name records);
/// Pass 2 (new `read_all_records_with_resident_data`) re-reads sequentially and
/// copies the resident `$DATA` of `$I` records under the SID directory —
/// the SID directory's record number comes from the OS file index
/// (`sid_dir_record_id_via_handle`), NOT from name matching (the record's
/// `$FILE_NAME` may only yield the 8.3 alias "S-1-5-~1").
#[cfg(windows)]
#[allow(dead_code)] // 实验性：未接入生产路径，仅供对照测试使用（原因见函数文档）
fn scan_drive_via_mft(
    drive_letter: char,
    sid_path: &Path,
    user_sid: &str,
    category: &JunkCategory,
) -> Result<(Vec<FileInfo>, DrivePhaseTiming), String> {
    let reader = mft_core::NtfsFileMetadataReader::open(drive_letter)?;

    // ---- 第一遍：全表读取（$R 配对数据源）+ 定位当前用户 SID 目录 ----
    // 定位的权威来源是 OS 文件索引（打开目录句柄拿记录号），而不是名字匹配：
    // 实测 C 盘 SID 目录的记录里 $FILE_NAME 只解析出 8.3 短名 "S-1-5-~1"，
    // 完整 SID 字符串匹配永远落空（本轮 gate 的失败根因）。
    let pass1_started = Instant::now();
    let sid_dir_id = sid_dir_record_id_via_handle(sid_path)?;
    let (records, _stats) = reader.read_all_records_all_names(&|_| true)?;
    let recycle_root_ids: HashSet<u64> = records
        .iter()
        .filter(|record| {
            record.is_dir
                && record.parent_id == 5 // 驱动器根目录的固定 MFT 参考号
                && record.name.eq_ignore_ascii_case("$Recycle.Bin")
        })
        .map(|record| record.mft_id)
        .collect();
    let sid_dir_ids: HashSet<u64> = HashSet::from([sid_dir_id]);
    let enumerate_ms = pass1_started.elapsed().as_millis();
    // 名字匹配结果仅作对照日志：与句柄记录号一致说明名字解析正常，不一致即 8.3 短名情形
    let name_matched_sid_ids: HashSet<u64> = records
        .iter()
        .filter(|record| {
            record.is_dir
                && recycle_root_ids.contains(&record.parent_id)
                && record.name.eq_ignore_ascii_case(user_sid)
        })
        .map(|record| record.mft_id)
        .collect();
    info!(
        "[回收站扫描耗时] mft {}: 第一遍定位：全表名字条目 {} 条，$Recycle.Bin 根 {} 个，名字匹配 SID 目录 {:?}，句柄权威记录号 {}",
        drive_letter,
        records.len(),
        recycle_root_ids.len(),
        name_matched_sid_ids,
        sid_dir_id
    );
    // 位置编号 vs 内容引用编号是否自洽：$Recycle.Bin 与 SID 目录自身的 parent 引用
    // （内容侧真相）连同 run 布局一起输出，用于定位编号错位
    let recycle_root_details: Vec<String> = records
        .iter()
        .filter(|record| recycle_root_ids.contains(&record.mft_id))
        .map(|record| format!("mft_id {} parent_id {}", record.mft_id, record.parent_id))
        .collect();
    let sid_dir_details: Vec<String> = records
        .iter()
        .filter(|record| sid_dir_ids.contains(&record.mft_id))
        .map(|record| format!("mft_id {} parent_id {}", record.mft_id, record.parent_id))
        .collect();
    let mft_file_size = fs::metadata(format!(r"\\.\{}:\$MFT", drive_letter))
        .ok()
        .map(|meta| meta.len());
    info!(
        "[回收站扫描耗时] mft {}: 布局 {}；\\\\.\\{}:\\$MFT 实际大小 {:?}；$Recycle.Bin 根 [{}]；SID 目录 [{}]",
        drive_letter,
        reader.layout_summary(),
        drive_letter,
        mft_file_size,
        recycle_root_details.join("; "),
        sid_dir_details.join("; ")
    );
    info!(
        "[回收站扫描耗时] mft {}: 第一遍定位：全表名字条目 {} 条，$Recycle.Bin 根 {} 个，名字匹配 SID 目录 {:?}，句柄权威记录号 {}",
        drive_letter,
        records.len(),
        recycle_root_ids.len(),
        name_matched_sid_ids,
        sid_dir_id
    );

    // ---- 第二遍：提取 $I 的 resident 内容 ----
    let pass2_started = Instant::now();
    // 诊断计数器：filter 零命中时区分"表里没有 $I 名称记录"和"$I 的 parent 与定位结果不一致"
    let i_named_seen = AtomicUsize::new(0);
    let unmatched_parents = Mutex::new(HashSet::new());
    let filter = |info: &mft_core::MftRecordInfo| {
        if info
            .name
            .get(..2)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("$I"))
        {
            i_named_seen.fetch_add(1, Ordering::Relaxed);
            if !sid_dir_ids.contains(&info.parent_id) {
                let mut parents = unmatched_parents
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if parents.len() < 8 {
                    parents.insert(info.parent_id);
                }
            }
        }
        sid_dir_ids.contains(&info.parent_id)
            && !info.is_dir
            && info
                .name
                .get(..2)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("$I"))
    };
    let (metadata_records, stats) =
        reader.read_all_records_with_resident_data(&filter, &|_| true)?;
    let parse_ms = pass2_started.elapsed().as_millis();
    // 第二遍内部的磁盘/解析分相：判断 $MFT 顺序读与解析哪个是剩余瓶颈
    debug!(
        "[回收站扫描耗时] mft {}: pass2 $MFT io {}ms / parse {}ms / 命中 {} 条",
        drive_letter, stats.io_ms, stats.parse_ms, stats.resident_extracted
    );
    info!(
        "[回收站扫描耗时] mft {}: $I 名称记录 {} 条，SID 目录 id {:?}，未命中 parent 样本 {:?}",
        drive_letter,
        i_named_seen.load(Ordering::Relaxed),
        sid_dir_ids,
        unmatched_parents
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    );
    // 未命中的 parent 到底是什么记录：如果它也是同名 SID 目录，说明盘上存在两个
    // 同名目录（比如曾被整体回收再重建），定位策略需要跟着改。
    let unmatched_detail: Vec<String> = unmatched_parents
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(
            |parent_id| match records.iter().find(|record| record.mft_id == *parent_id) {
                Some(record) => format!(
                    "mft_id {} name={:?} is_dir={} parent_id {}",
                    record.mft_id, record.name, record.is_dir, record.parent_id
                ),
                None => format!("mft_id {parent_id}: <不在已解析记录中（fixup 失败或空闲记录）>"),
            },
        )
        .collect();
    info!(
        "[回收站扫描耗时] mft {}: 未命中 parent 详情 [{}]",
        drive_letter,
        unmatched_detail.join("; ")
    );

    // ---- $R 配对：与 fs 路径同语义——孤儿 $I 丢弃，$R 目录标记 is_dir ----
    let mut data_dirs: HashMap<String, bool> = HashMap::new();
    for record in &records {
        if !sid_dir_ids.contains(&record.parent_id) || record.name.len() <= 2 {
            continue;
        }
        if let Some(suffix) = record.name.strip_prefix("$R") {
            data_dirs.insert(suffix.to_ascii_lowercase(), record.is_dir);
        }
    }

    let mut entries = Vec::new();
    let mut non_resident_fallbacks = 0usize;
    let mut orphan_drops = 0usize;
    let mut parse_drops = 0usize;
    // [P6] 孤儿 $I 清单：延迟到配对循环之后做全量普查（P5 的"前 5 条"样本有采样偏差，
    // 前 5 条恰好全是"$R 早已不在磁盘"的陈旧 $I，真正缺失的那 3.7 万条从未被样本覆盖）。
    #[cfg(test)]
    let mut orphan_suffixes: Vec<String> = Vec::new();
    for metadata in &metadata_records {
        let Some(suffix) = metadata.info.name.strip_prefix("$I") else {
            continue;
        };
        if suffix.is_empty() {
            continue;
        }
        // 孤儿 $I（无 $R 配对）在 Explorer 里也不可见，与 fs 路径同语义丢弃
        let &is_dir = match data_dirs.get(&suffix.to_ascii_lowercase()) {
            Some(is_dir) => is_dir,
            None => {
                orphan_drops += 1;
                #[cfg(test)]
                orphan_suffixes.push(suffix.to_string());
                continue;
            }
        };

        let metadata_path = sid_path.join(&metadata.info.name);
        let (logical_size, original_path, deleted_at) = if metadata.data.is_empty() {
            // 非 resident / 内容缺失：退化为单独读（预期数量≈0，日志可实测）
            non_resident_fallbacks += 1;
            match parse_metadata(&metadata_path) {
                Some(parsed) => parsed,
                None => {
                    parse_drops += 1;
                    continue;
                }
            }
        } else {
            match parse_metadata_bytes(&metadata.data) {
                Some(parsed) => parsed,
                None => {
                    parse_drops += 1;
                    continue;
                }
            }
        };

        let visible_name = Path::new(&original_path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| original_path.clone());

        entries.push(
            FileInfo::new(
                sid_path
                    .join(format!("$R{}", suffix))
                    .to_string_lossy()
                    .into_owned(),
                visible_name,
                logical_size,
                deleted_at,
                is_dir,
                category.clone(),
            )
            .with_original_path(original_path),
        );
    }

    // [P6] 丢弃分相汇总 + 孤儿全量普查/记录级取证（一次运行判别全部剩余假设）
    info!(
        "[P6] mft {}: 丢弃分相 orphan {} / parse {} / 非 resident 兜底 {}",
        drive_letter, orphan_drops, parse_drops, non_resident_fallbacks
    );
    #[cfg(test)]
    probe_orphan_forensics(&reader, &records, sid_path, &sid_dir_ids, &orphan_suffixes);

    let timing = DrivePhaseTiming {
        backend: "mft",
        drive_letter,
        enumerate_ms,
        parse_ms,
        directory_entries: stats.records_seen,
        metadata_files: stats.resident_extracted,
        found_entries: entries.len(),
        non_resident_fallbacks,
    };
    Ok((entries, timing))
}

/// [P6] 孤儿 $I 的全量普查 + 记录级取证（log-only，零行为变化）。
///
/// P5 的"前 5 条 orphan 样本"存在采样偏差：前 5 条恰好全是"$R 早已不在磁盘"的陈旧
/// $I，真正缺失的 3.7 万条从未被覆盖，于是 orphan/parse 两相始终没被区分。本函数改为：
///
/// 1. **全量普查**（无抽样）：每条孤儿 $I 的对应 $R 在磁盘上是否存在、在 pass1 解析结果
///    里是否有一条同名记录（若在，说明问题在配对侧而不是解析侧）；
/// 2. **记录级取证**：对"磁盘有 $R、pass1 无记录"的那批，用 OS 文件句柄拿到权威记录号
///    （文件索引 = MFT 引用），再按记录号**单记录读取**并把属性链 dump 出来——这条路径
///    与 16MB 块流水线完全独立，因此能同时判别"记录本身有问题"和"块读取/记录号映射有问题"；
/// 3. **编号连续性**：打印缺失集合的记录号分布，以及样本记录号在 pass1 中的左右邻居，
///    用于排除"记录号错位"这一类假设。
#[cfg(all(windows, test))]
fn probe_orphan_forensics(
    reader: &mft_core::NtfsFileMetadataReader,
    records: &[mft_core::MftRecordInfo],
    sid_path: &Path,
    sid_dir_ids: &HashSet<u64>,
    orphans: &[String],
) {
    if orphans.is_empty() {
        return;
    }
    let drive = sid_path.display();

    // ---- 1) 全表 $R 名索引（pass1 已解析出的记录）----
    let mut by_name: HashMap<String, (u64, u64)> = HashMap::with_capacity(records.len() / 4);
    for record in records {
        if record
            .name
            .get(..2)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("$R"))
        {
            by_name.insert(
                record.name.to_ascii_lowercase(),
                (record.mft_id, record.parent_id),
            );
        }
    }

    // ---- 2) 全量普查 ----
    let mut disk_present = 0usize;
    let mut disk_absent = 0usize;
    let mut in_pass1_sid = 0usize;
    let mut in_pass1_other_parent = 0usize;
    let mut absent_in_pass1 = 0usize;
    let mut rid_fail = 0usize;
    let mut present_samples: Vec<String> = Vec::new();
    let mut absent_samples: Vec<String> = Vec::new();
    let mut absent_short_name = 0usize;
    let mut missing: Vec<(String, u64)> = Vec::new();
    for suffix in orphans {
        let r_name = format!("$R{suffix}");
        let path = sid_path.join(&r_name);
        if !path.exists() {
            disk_absent += 1;
            // $I 名字本身可能是 8.3 短名（含 '~'），那样构造出的 $R 名必然不存在——
            // 这一条把"陈旧 $I"和"名字解析成短名导致的假孤儿"分开
            if suffix.contains('~') {
                absent_short_name += 1;
            }
            if absent_samples.len() < 5 {
                absent_samples.push(format!("$I{suffix}"));
            }
            continue;
        }
        disk_present += 1;
        if let Some((rid, parent)) = by_name.get(&r_name.to_ascii_lowercase()) {
            if sid_dir_ids.contains(parent) {
                in_pass1_sid += 1;
            } else {
                in_pass1_other_parent += 1;
            }
            if present_samples.len() < 5 {
                present_samples.push(format!(
                    "{r_name} rid {rid} parent {parent} parent_ok={}",
                    sid_dir_ids.contains(parent)
                ));
            }
            continue;
        }
        absent_in_pass1 += 1;
        match file_record_id_via_handle(&path) {
            Ok(rid) => missing.push((suffix.clone(), rid)),
            Err(_) => rid_fail += 1,
        }
    }
    info!(
        "[P6] {drive}: 孤儿 {} 条全量普查 → 磁盘 $R 存在 {} / 不存在 {}（其中 $I 名含 '~' 的短名 {} 条，样本 {:?}）；pass1 有同名 $R 记录 {}（parent∈SID {} / parent 在别处 {}）；pass1 完全没有 {}；句柄取记录号失败 {}",
        orphans.len(),
        disk_present,
        disk_absent,
        absent_short_name,
        absent_samples,
        in_pass1_sid + in_pass1_other_parent,
        in_pass1_sid,
        in_pass1_other_parent,
        absent_in_pass1,
        rid_fail,
    );
    if !present_samples.is_empty() {
        info!("[P6] {drive}: pass1 有同名记录的样本 {present_samples:?}");
    }
    if missing.is_empty() {
        return;
    }

    // ---- 3) 记录号分布：缺失集合是否落在某个连续区间 ----
    let mut rids: Vec<u64> = missing.iter().map(|(_, rid)| *rid).collect();
    rids.sort_unstable();
    let quartile = |fraction: f64| rids[((rids.len() as f64 - 1.0) * fraction) as usize];
    let max_coverable = {
        // run 链覆盖的记录号上界（与 plan_read_chunks 同口径的估算）：二分找出最后一个有偏移的记录号
        let mut low = 0u64;
        let mut high = 4_000_000u64;
        while low < high {
            let middle = (low + high + 1) / 2;
            if reader.probe_record_offset(middle).is_some() {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        low + 1
    };
    info!(
        "[P6] {drive}: 缺失集合记录号分布 n={} min={} p25={} p50={} p75={} max={}（run 链可覆盖上限 ≈{max_coverable}）直方图 {hist:?}",
        rids.len(),
        rids[0],
        quartile(0.25),
        quartile(0.5),
        quartile(0.75),
        rids[rids.len() - 1],
        hist = {
            let mut buckets = [0usize; 10];
            for rid in &rids {
                let index = ((*rid as f64 / max_coverable as f64) * 10.0) as usize;
                buckets[index.min(9)] += 1;
            }
            buckets
        }
    );

    // ---- 4) 逐条分类：按权威记录号单记录读取，判定丢在哪一级 ----
    // 全名字枚举下多条条目共享同一个 mft_id，因此只保证非递减（同一记录的名字相邻）
    let sorted_records = records.windows(2).all(|pair| pair[0].mft_id <= pair[1].mft_id);
    info!("[P6] {drive}: pass1 记录按 mft_id 是否单调递增 = {sorted_records}");
    let mut magic_bad = 0usize;
    let mut fixup_bad = 0usize;
    let mut walk_bad = 0usize;
    let mut parse_none = 0usize;
    let mut parse_none_no_name = 0usize;
    let mut parse_none_with_name = 0usize;
    let mut parse_none_name_samples: Vec<String> = Vec::new();
    let mut parsed_but_absent = 0usize;
    let mut name_mismatch = 0usize;
    let mut parent_mismatch = 0usize;
    let mut with_attr_list = 0usize;
    let mut read_failed = 0usize;
    let mut samples: Vec<String> = Vec::new();
    let mut parsed_absent_samples: Vec<String> = Vec::new();
    let mut walk_samples: Vec<String> = Vec::new();
    let mut name_samples: Vec<String> = Vec::new();
    for (suffix, rid) in &missing {
        let r_name = format!("$R{suffix}");
        let view = match reader.probe_record(*rid) {
            Ok(view) => view,
            Err(error) => {
                read_failed += 1;
                if samples.len() < 4 {
                    samples.push(format!("{r_name} rid={rid} 读取失败: {error}"));
                }
                continue;
            }
        };
        if !view.magic_ok {
            magic_bad += 1;
            if samples.len() < 8 {
                samples.push(format!("{r_name} {}", view.describe()));
            }
            continue;
        }
        if !view.fixup_ok {
            fixup_bad += 1;
            if samples.len() < 8 {
                samples.push(format!("{r_name} {}", view.describe()));
            }
            continue;
        }
        if !view.attribute_list.is_empty() {
            with_attr_list += 1;
        }
        if view.walk_error.is_some() {
            walk_bad += 1;
            if walk_samples.len() < 4 {
                walk_samples.push(format!("{r_name} {}", view.describe()));
            }
            continue;
        }
        if !view.parse_ok {
            parse_none += 1;
            if view.file_names.is_empty() {
                parse_none_no_name += 1;
            } else {
                // 有 $FILE_NAME 却仍被 parse_record_info 拒绝：只剩"reparse 位"等旁路条件
                parse_none_with_name += 1;
                if parse_none_name_samples.len() < 3 {
                    parse_none_name_samples.push(format!("{r_name} {}", view.describe()));
                }
            }
            if samples.len() < 8 {
                samples.push(format!("{r_name} {}", view.describe()));
            }
            continue;
        }
        let name_ok = view
            .file_names
            .iter()
            .any(|(_, _, name)| name.eq_ignore_ascii_case(&r_name));
        let parent_ok = view
            .file_names
            .iter()
            .any(|(parent, _, _)| sid_dir_ids.contains(parent));
        if !name_ok {
            name_mismatch += 1;
            if name_samples.len() < 4 {
                name_samples.push(format!("{r_name} {}", view.describe()));
            }
            continue;
        }
        if !parent_ok {
            parent_mismatch += 1;
        } else {
            parsed_but_absent += 1;
            if parsed_absent_samples.len() < 4 {
                parsed_absent_samples.push(format!("{r_name} {}", view.describe()));
            }
        }
    }
    info!(
        "[P6] {drive}: 逐条分类 n={} magic 非 FILE {} / fixup 失败 {} / 属性链越界 {} / parse_record_info=None {}（其中无 $FILE_NAME {} / 有名字仍被拒 {}）/ 名字不符 {} / parent 不符 {} / **正常解析但 pass1 没有** {} / 读取失败 {}（含 $ATTRIBUTE_LIST 的 {} 条）",
        missing.len(),
        magic_bad,
        fixup_bad,
        walk_bad,
        parse_none,
        parse_none_no_name,
        parse_none_with_name,
        name_mismatch,
        parent_mismatch,
        parsed_but_absent,
        read_failed,
        with_attr_list,
    );
    for (label, items) in [
        ("解析正常但 pass1 没有", &parsed_absent_samples),
        ("有名字仍被拒", &parse_none_name_samples),
        ("属性链越界", &walk_samples),
        ("名字不符", &name_samples),
        ("其他", &samples),
    ] {
        for item in items {
            info!("[P6] {drive}: {label} 样本 {item}");
        }
    }

    // ---- 5) 深挖：等距取 24 条，dump 全貌 + 属性列表追踪 + pass1 邻居 ----
    let stride = (missing.len() / 24).max(1);
    let mut dumped = 0usize;
    for index in (0..missing.len()).step_by(stride) {
        if dumped >= 24 {
            break;
        }
        dumped += 1;
        let (suffix, rid) = &missing[index];
        let r_name = format!("$R{suffix}");
        let view = match reader.probe_record(*rid) {
            Ok(view) => view,
            Err(error) => {
                info!("[P6] {drive}: 深挖 {r_name} rid={rid} 读取失败 {error}");
                continue;
            }
        };
        // pass1 里这个记录号是什么（若存在），以及左右邻居的名字——排除记录号错位
        let mut neighbors: Vec<String> = Vec::new();
        for delta in -2i64..=2 {
            let probe_id = (*rid as i64 + delta).max(0) as u64;
            match records.binary_search_by_key(&probe_id, |record| record.mft_id) {
                Ok(found) => neighbors.push(format!(
                    "{}:{:?}→{}",
                    probe_id, records[found].name, records[found].parent_id
                )),
                Err(_) => neighbors.push(format!("{probe_id}:<无>")),
            }
        }
        info!("[P6] {drive}: 深挖 {r_name} {}", view.describe());
        info!(
            "[P6] {drive}: 深挖 {r_name} rid={rid} pass1邻居 {:?} 记录号字节偏移 {:?}",
            neighbors,
            reader.probe_record_offset(*rid)
        );
        // $ATTRIBUTE_LIST 追踪：若名字确实在扩展记录里，这里能直接证死
        for (attribute_type, extension_rid) in view.attribute_list.iter().take(6) {
            if *extension_rid == *rid {
                continue;
            }
            match reader.probe_record(*extension_rid) {
                Ok(extension) => info!(
                    "[P6] {drive}: 深挖 {r_name} 扩展记录(0x{attribute_type:X}) {}",
                    extension.describe()
                ),
                Err(error) => info!(
                    "[P6] {drive}: 深挖 {r_name} 扩展记录 {extension_rid} 读取失败 {error}"
                ),
            }
        }
    }
}

/// One info line per drive plus a wall-clock total. `backend` 告诉读者该盘走的是哪条路径，
/// 两相口径随 backend 不同（fs=枚举/并行读；mft=第一遍定位/第二遍提取）——对比性能只应
/// 在同 backend 之间进行。
#[cfg(windows)]
fn log_scan_phase_timings(timings: &[DrivePhaseTiming], wall: std::time::Duration, entries: usize) {
    for timing in timings {
        let fallback_note = if timing.non_resident_fallbacks > 0 {
            format!("（非 resident 兜底 {} 次）", timing.non_resident_fallbacks)
        } else {
            String::new()
        };
        info!(
            "[回收站扫描耗时] {} {}: 枚举 {}ms ({} 项, $I {} 个) + 解析 {}ms → 条目 {}{}",
            timing.backend,
            timing.drive_letter,
            timing.enumerate_ms,
            timing.directory_entries,
            timing.metadata_files,
            timing.parse_ms,
            timing.found_entries,
            fallback_note,
        );
    }
    info!(
        "[回收站扫描耗时] 总计 {}ms（{} 盘串行, {} 条目）",
        wall.as_millis(),
        timings.len(),
        entries,
    );
}

#[cfg(windows)]
fn parse_metadata(metadata_path: &Path) -> Option<(u64, String, i64)> {
    let bytes = fs::read(metadata_path).ok()?;
    parse_metadata_bytes(&bytes)
}

#[cfg(windows)]
fn parse_metadata_bytes(bytes: &[u8]) -> Option<(u64, String, i64)> {
    if bytes.len() < 28 {
        return None;
    }

    // $I files from Windows Vista and later: version, original size, deletion time, path length, UTF-16 path.
    let version = u64::from_le_bytes(bytes[0..8].try_into().ok()?);
    if version != 2 {
        return None;
    }

    let logical_size = u64::from_le_bytes(bytes[8..16].try_into().ok()?);
    let filetime = u64::from_le_bytes(bytes[16..24].try_into().ok()?);
    let path_length = u32::from_le_bytes(bytes[24..28].try_into().ok()?) as usize;
    let path_end = 28usize.checked_add(path_length.checked_mul(2)?)?;
    if path_length == 0 || path_end > bytes.len() {
        return None;
    }

    // Offset 24 is the path-length field; the real UTF-16 path starts at offset 28, otherwise the first character becomes a control character.
    let path_bytes = &bytes[28..path_end];
    let utf16_path: Vec<u16> = path_bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .take_while(|character| *character != 0)
        .collect();
    if utf16_path.is_empty() {
        return None;
    }

    let original_path = String::from_utf16_lossy(&utf16_path);
    if original_path.trim().is_empty() {
        return None;
    }

    // FILETIME starts from year 1601; convert it to the Unix timestamp used by the project's existing structures.
    const WINDOWS_TO_UNIX_SECONDS: u64 = 11_644_473_600;
    let deleted_at = filetime
        .checked_div(10_000_000)
        .and_then(|seconds| seconds.checked_sub(WINDOWS_TO_UNIX_SECONDS))
        .map(|seconds| seconds as i64)
        .unwrap_or(0);

    Some((logical_size, original_path, deleted_at))
}

#[cfg(all(test, windows))]
mod tests {
    use super::parse_metadata_bytes;
    use super::{collect_drive_entries, current_user_sid, scan_drive_via_mft};
    use crate::scanner::{CategoryScanResult, FileInfo, JunkCategory};
    use std::path::PathBuf;

    #[test]
    fn parses_v2_metadata_path_without_length_prefix() {
        let original_path = r"D:\C_Map\download\Viap发布封面.png";
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2u64.to_le_bytes());
        bytes.extend_from_slice(&1_648_597u64.to_le_bytes());
        bytes.extend_from_slice(&132_000_000_000_000_000u64.to_le_bytes());
        bytes.extend_from_slice(&(original_path.encode_utf16().count() as u32).to_le_bytes());
        for character in original_path.encode_utf16() {
            bytes.extend_from_slice(&character.to_le_bytes());
        }

        let (_, parsed_path, _) = parse_metadata_bytes(&bytes).expect("应解析有效的 $I 元数据");
        assert_eq!(parsed_path, original_path);
    }

    #[test]
    #[ignore = "真机分相计时（回收站专用扫描器墙钟诊断）：手动运行 cargo test --lib -- --ignored --nocapture"]
    fn real_machine_scan_current_user_phase_timings() {
        // Surface the per-drive phase timings with --nocapture (the app's
        // env_logger::init lives in run(), which tests never call).
        let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
            .try_init();

        let mut result = CategoryScanResult::new(JunkCategory::RecycleBin);
        super::scan_current_user(&JunkCategory::RecycleBin, &mut result);
        // No hard assertion on the entry count: the machine's recycle bin may
        // legitimately be empty. The timing lines in the log are the deliverable.
        assert_eq!(result.category, JunkCategory::RecycleBin);
    }

    /// 真机对照（实验性 MFT 路径，需管理员）：**正确性与完整性双门槛**——MFT 路径产出的条目
    /// 必须与 fs 枚举逐条相等。多出条目 = 路径语义错误；缺失条目 = 解析路径又漏了名字（曾经
    /// 25% 的 $R 因单名字扫描被丢掉，见 `read_all_records_all_names`），两者都直接失败。
    ///
    /// 三明治对照：fs₁ → MFT → fs₂；fs₁ ≠ fs₂ 说明对照期间回收站发生变化（并发会话的
    /// 构建产物写入/移除，实测 D 盘 4 分钟窗口进出约 3.4 万条目），本次比较无效——自动
    /// 重试（最多 3 次），仍不稳定则明确报告"需安静窗口"。稳定窗口内只比较测试开始前就
    /// 已删除的条目，剔除对照期间新增的残余噪声。
    #[test]
    #[ignore = "实验性 MFT 路径对照（需管理员）：手动运行 cargo test --lib -- --ignored --nocapture"]
    fn real_machine_mft_path_matches_fs_path() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
            .try_init();

        // 卷设备访问（\\.\C:）需要管理员；非管理员时在第一盘就会 os error 5，
        // 与其用一条含糊的 IO 错误失败，不如开门见山给出运行指引。
        if !crate::scanner::big_files_engine::mft_core::is_elevated() {
            panic!("此对照需要管理员权限。请在管理员终端运行：cargo test --lib real_machine_mft_path_matches_fs_path -- --ignored --nocapture");
        }

        let Some(user_sid) = current_user_sid() else {
            panic!("真机对照需要当前用户 SID");
        };

        // 三明治重试：fs 快照前后一致（回收站安静）才做集合比较。并发会话（构建、清理）
        // 会在对照期间写入和移除条目——实测 D 盘 4 分钟窗口进出约 3.4 万条目——两次快照
        // 之间的出入是并发噪声而非路径语义差异，不稳定就自动重试。
        const MAX_STABILITY_ATTEMPTS: usize = 3;
        'attempts: for attempt in 1..=MAX_STABILITY_ATTEMPTS {
            let test_started_unix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("系统时钟早于 Unix 纪元")
                .as_secs() as i64;
            let mut compared_drives = 0usize;
            let mut stable = true;
            type EntryKey = (String, u64, i64, bool, Option<String>);
            let mut per_drive_sets: Vec<(
                char,
                std::collections::BTreeSet<EntryKey>,
                std::collections::BTreeSet<EntryKey>,
            )> = Vec::new();
            for drive_letter in crate::sys_drive::existing_drive_letters() {
                let sid_path =
                    PathBuf::from(format!("{}:\\$Recycle.Bin\\{}", drive_letter, user_sid));
                if !sid_path.is_dir() {
                    continue;
                }

                let (fs_entries, _) =
                    collect_drive_entries(&sid_path, drive_letter, &JunkCategory::RecycleBin);
                let (mft_entries, mft_timing) = match scan_drive_via_mft(
                    drive_letter,
                    &sid_path,
                    &user_sid,
                    &JunkCategory::RecycleBin,
                ) {
                    Ok(result) => result,
                    Err(error) => panic!("{drive_letter}: 盘 MFT 直读不可用: {error}"),
                };
                let (fs_entries_after, _) =
                    collect_drive_entries(&sid_path, drive_letter, &JunkCategory::RecycleBin);
                println!(
                "{drive_letter}: 盘 mft 计时：第一遍 {}ms / 第二遍 {}ms / 全表记录 {} 条 / resident 提取 {} 条 / 非 resident 兜底 {} 次",
                mft_timing.enumerate_ms,
                mft_timing.parse_ms,
                mft_timing.directory_entries,
                mft_timing.metadata_files,
                mft_timing.non_resident_fallbacks,
            );

                // 稳定性判定：fs 前后两次快照必须一致，否则对照期间回收站发生了增删
                // （并发噪声，不是路径语义差异），本次比较无效。
                let key = |entry: &FileInfo| {
                    (
                        entry.path.clone(),
                        entry.size,
                        entry.modified_time,
                        entry.is_dir,
                        entry.original_path.clone(),
                    )
                };
                let before_set: std::collections::BTreeSet<EntryKey> =
                    fs_entries.iter().map(&key).collect();
                let after_set: std::collections::BTreeSet<EntryKey> =
                    fs_entries_after.iter().map(&key).collect();
                if before_set != after_set {
                    println!(
                    "{drive_letter}: 盘对照期间回收站发生变化（fs 前 {} 条 → 后 {} 条），本次比较无效",
                    before_set.len(),
                    after_set.len()
                );
                    stable = false;
                    continue;
                }

                // 只比较测试开始前就已删除的条目；对照期间新增的条目（其他会话的构建产物）
                // 在两侧的口径天然不同（fs 先扫看不到，MFT 后扫看得到），剔除而非比较。
                let fs_compared: Vec<&FileInfo> = fs_entries
                    .iter()
                    .filter(|entry| entry.modified_time < test_started_unix)
                    .collect();
                let mft_compared: Vec<&FileInfo> = mft_entries
                    .iter()
                    .filter(|entry| entry.modified_time < test_started_unix)
                    .collect();
                println!(
                    "{drive_letter}: 盘 fs {} 条 / mft {} 条；剔除对照期间新增后比较 {} vs {} 条",
                    fs_entries.len(),
                    mft_entries.len(),
                    fs_compared.len(),
                    mft_compared.len()
                );

                let fs_set: std::collections::BTreeSet<EntryKey> =
                    fs_compared.iter().map(|entry| key(entry)).collect();
                let mft_set: std::collections::BTreeSet<EntryKey> =
                    mft_compared.iter().map(|entry| key(entry)).collect();
                per_drive_sets.push((drive_letter, fs_set, mft_set));
                compared_drives += 1;
            }

            if !stable {
                if attempt < MAX_STABILITY_ATTEMPTS {
                    println!("第 {attempt} 次尝试遇到回收站并发变化，自动重试（{attempt}/{MAX_STABILITY_ATTEMPTS}）");
                    continue 'attempts;
                }
                panic!("连续 {MAX_STABILITY_ATTEMPTS} 次对照期间回收站都在变化（并发会话活跃）。请关闭其他会话的构建/清理后再重跑。");
            }

            assert!(
                compared_drives > 0,
                "真机上应至少有一个含 SID 目录的回收站分区"
            );
            for (drive_letter, fs_set, mft_set) in &per_drive_sets {
                // 正确性门槛（硬断言）：MFT 产出的条目必须是 fs 条目的子集——MFT 凭空
                // 多出条目 = 路径语义错误，立即失败。
                let extra: Vec<_> = mft_set.difference(fs_set).collect();
                assert!(
                    extra.is_empty(),
                    "{drive_letter}: 盘 MFT 产出了 fs 没有的条目（正确性违规，比较 {} 条）。多出 {} 条（前 10 条）: {:?}",
                    fs_set.len(),
                    extra.len(),
                    extra.iter().take(10).collect::<Vec<_>>(),
                );
                // 完整性（fs 有而 MFT 缺失）：曾经是"已知限制"（单名字扫描丢掉硬链接名字），
                // 现在必须为 0，否则说明解析路径又漏了名字，直接失败。
                let missing: Vec<_> = fs_set.difference(mft_set).collect();
                assert!(
                    missing.is_empty(),
                    "{drive_letter}: 盘 MFT 缺失 fs 有的条目 {} 条（完整性违规，比较 {} 条）。前 10 条: {:?}",
                    missing.len(),
                    fs_set.len(),
                    missing.iter().take(10).collect::<Vec<_>>(),
                );
            }
            println!(
                "对照通过（完整性门槛）：{compared_drives} 个盘的 MFT 产出条目与 fs 枚举逐条相等"
            );
            return;
        }
    }
}

#[cfg(windows)]
fn current_user_sid() -> Option<String> {
    static CURRENT_USER_SID: OnceLock<Option<String>> = OnceLock::new();
    CURRENT_USER_SID
        .get_or_init(current_user_sid_uncached)
        .clone()
}

#[cfg(windows)]
fn current_user_sid_uncached() -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{LocalFree, HANDLE, HLOCAL};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token_handle = HANDLE::default();
        // SAFETY: token_handle is the out-param; the pseudo process handle is
        // owned by the OS and never closed here
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token_handle).is_err() {
            return None;
        }

        // token_handle closes itself via HANDLE::drop on every early return
        // (H4 contract §4.2: never call CloseHandle manually on a windows-crate HANDLE)

        let mut required_size: u32 = 0;
        // SAFETY: passing None only queries the required buffer size
        let _ = GetTokenInformation(token_handle, TokenUser, None, 0, &mut required_size);
        if required_size == 0 {
            return None;
        }

        // Use a u64 buffer to satisfy TOKEN_USER's alignment requirement on Windows targets.
        let mut buffer = vec![0u64; (required_size as usize + 7) / 8];
        // SAFETY: buffer is sized to the length reported by the first call, so
        // the API never writes past its end
        let success = GetTokenInformation(
            token_handle,
            TokenUser,
            Some(buffer.as_mut_ptr() as *mut core::ffi::c_void),
            required_size,
            &mut required_size,
        );
        if success.is_err() {
            return None;
        }

        let token_user = buffer.as_ptr() as *const TOKEN_USER;
        let mut sid_string = PWSTR::null();
        // SAFETY: the SID pointer comes from the token info buffer filled above;
        // sid_string receives an API-allocated buffer freed with LocalFree below
        let converted = ConvertSidToStringSidW((*token_user).User.Sid, &mut sid_string);
        if converted.is_err() || sid_string.is_null() {
            return None;
        }

        let sid_length = (0..)
            .take_while(|offset| *sid_string.as_ptr().add(*offset) != 0)
            .count();
        let sid =
            String::from_utf16_lossy(std::slice::from_raw_parts(sid_string.as_ptr(), sid_length));
        // SAFETY: the buffer was allocated by ConvertSidToStringSidW with
        // LocalAlloc semantics
        LocalFree(Some(HLOCAL(sid_string.as_ptr() as *mut core::ffi::c_void)));
        Some(sid)
    }
}
