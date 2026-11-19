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

        // MFT 直读按盘路由（前置条件：管理员 + NTFS）：把 10 万次 $I 随机小文件读换成一次
        // 顺序 $MFT 读（分相计时实测 fs 路径解析相 25.5s，全为随机 IO）。任一盘 MFT 失败都
        // 逐盘降级到 fs 路径，绝不让整次扫描因新路径失败而变空。
        let mft_preconditions = mft_core::is_elevated();
        for drive_letter in crate::sys_drive::existing_drive_letters() {
            let sid_path = PathBuf::from(format!("{}:\\$Recycle.Bin\\{}", drive_letter, user_sid));
            let (drive_entries, timing);
            if mft_preconditions && mft_core::is_ntfs(drive_letter) {
                match scan_drive_via_mft(drive_letter, &sid_path, &user_sid, category) {
                    Ok((mft_entries, mft_timing)) => {
                        drive_entries = mft_entries;
                        timing = mft_timing;
                    }
                    Err(error) => {
                        warn!(
                            "{} 盘回收站 MFT 直读失败，降级为目录枚举: {}",
                            drive_letter, error
                        );
                        let (fs_entries, fs_timing) =
                            collect_drive_entries(&sid_path, drive_letter, category);
                        drive_entries = fs_entries;
                        timing = fs_timing;
                    }
                }
            } else {
                let (fs_entries, fs_timing) =
                    collect_drive_entries(&sid_path, drive_letter, category);
                drive_entries = fs_entries;
                timing = fs_timing;
            }
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

/// One drive's recycle-bin entries via two sequential `$MFT` passes instead of
/// ~100k random small-file reads (measured: fs-path parse phase 25.5s on D:,
/// all of it random IO; a sequential `$MFT` read of the same drive is ~2s).
///
/// Pass 1 (existing `read_all_records`, untouched) locates the current user's
/// SID directory on this drive — the range semantics must match the fs path,
/// which only ever enumerates the current user's own SID folder.
/// Pass 2 (new `read_all_records_with_resident_data`) re-reads sequentially and
/// copies the resident `$DATA` of `$I` records whose parent is that SID folder.
/// `$I` headers are always ≤ ~550 bytes, so their content is resident in practice;
/// entries that come back with empty content fall back to one `fs::read` each.
#[cfg(windows)]
fn scan_drive_via_mft(
    drive_letter: char,
    sid_path: &Path,
    user_sid: &str,
    category: &JunkCategory,
) -> Result<(Vec<FileInfo>, DrivePhaseTiming), String> {
    let reader = mft_core::NtfsFileMetadataReader::open(drive_letter)?;

    // ---- 第一遍：定位当前用户 SID 目录 ----
    let pass1_started = Instant::now();
    let (records, _stats) = reader.read_all_records(&|_| true)?;
    let recycle_root_ids: HashSet<u64> = records
        .iter()
        .filter(|record| {
            record.is_dir
                && record.parent_id == 5 // 驱动器根目录的固定 MFT 参考号
                && record.name.eq_ignore_ascii_case("$Recycle.Bin")
        })
        .map(|record| record.mft_id)
        .collect();
    let sid_dir_ids: HashSet<u64> = records
        .iter()
        .filter(|record| {
            record.is_dir
                && recycle_root_ids.contains(&record.parent_id)
                && record.name.eq_ignore_ascii_case(user_sid)
        })
        .map(|record| record.mft_id)
        .collect();
    let enumerate_ms = pass1_started.elapsed().as_millis();
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
        "[回收站扫描耗时] mft {}: 第一遍定位：全表记录 {} 条，$Recycle.Bin 根 {} 个，当前用户 SID 目录 {} 个",
        drive_letter,
        records.len(),
        recycle_root_ids.len(),
        sid_dir_ids.len()
    );
    // SID 目录定位失败意味着 pass2 必然空手而归——与其静默 Ok(0)（对照测试会误报
    // "全部缺失"），不如显式报错走逐盘降级，把定位现场的数字带进警告。
    if sid_dir_ids.is_empty() {
        return Err(format!(
            "未在 {drive_letter}: 盘 MFT 中定位到当前用户 SID 目录（全表记录 {} 条，$Recycle.Bin 根 {} 个）",
            records.len(),
            recycle_root_ids.len()
        ));
    }

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
            None => continue,
        };

        let metadata_path = sid_path.join(&metadata.info.name);
        let (logical_size, original_path, deleted_at) = if metadata.data.is_empty() {
            // 非 resident / 内容缺失：退化为单独读（预期数量≈0，日志可实测）
            non_resident_fallbacks += 1;
            match parse_metadata(&metadata_path) {
                Some(parsed) => parsed,
                None => continue,
            }
        } else {
            match parse_metadata_bytes(&metadata.data) {
                Some(parsed) => parsed,
                None => continue,
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

    /// 真机对照门槛（MFT 上线条件）：同一台机器上，MFT 直读路径与 fs 路径产出的回收站
    /// 条目集合必须逐条一致。
    ///
    /// 回收站不是静态目标：并发会话（构建、清理）会持续往里写入新条目，而 fs 先扫、
    /// MFT 后扫，间隔内的新增会单向表现为"MFT 多出"——实测被 s6 会话的构建命中过。
    /// 因此只比较**测试开始前就已删除**的条目（回收站条目的 modified_time 即删除时间），
    /// 对照期间的新增剔除出双方，属预期噪声而非差异。
    #[test]
    #[ignore = "真机对照（MFT 直读 vs fs 路径，需管理员）：手动运行 cargo test --lib -- --ignored --nocapture"]
    fn real_machine_mft_path_matches_fs_path() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
            .try_init();

        // 卷设备访问（\\.\C:）需要管理员；非管理员时在第一盘就会 os error 5，
        // 与其用一条含糊的 IO 错误失败，不如开门见山给出运行指引。
        if !crate::scanner::big_files_engine::mft_core::is_elevated() {
            panic!("此对照需要管理员权限。请在管理员终端运行：cargo test --lib real_machine_mft_path_matches_fs_path -- --ignored --nocapture");
        }

        let test_started_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时钟早于 Unix 纪元")
            .as_secs() as i64;

        let Some(user_sid) = current_user_sid() else {
            panic!("真机对照需要当前用户 SID");
        };
        let mut compared_drives = 0usize;
        for drive_letter in crate::sys_drive::existing_drive_letters() {
            let sid_path = PathBuf::from(format!("{}:\\$Recycle.Bin\\{}", drive_letter, user_sid));
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
            println!(
                "{drive_letter}: 盘 mft 计时：第一遍 {}ms / 第二遍 {}ms / 全表记录 {} 条 / resident 提取 {} 条 / 非 resident 兜底 {} 次",
                mft_timing.enumerate_ms,
                mft_timing.parse_ms,
                mft_timing.directory_entries,
                mft_timing.metadata_files,
                mft_timing.non_resident_fallbacks,
            );

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

            let key = |entry: &FileInfo| {
                (
                    entry.path.clone(),
                    entry.size,
                    entry.modified_time,
                    entry.is_dir,
                    entry.original_path.clone(),
                )
            };
            let fs_set: std::collections::BTreeSet<_> =
                fs_compared.iter().map(|entry| key(entry)).collect();
            let mft_set: std::collections::BTreeSet<_> =
                mft_compared.iter().map(|entry| key(entry)).collect();

            let missing: Vec<_> = fs_set.difference(&mft_set).collect();
            let extra: Vec<_> = mft_set.difference(&fs_set).collect();
            assert!(
                missing.is_empty() && extra.is_empty(),
                "{drive_letter}: 盘两路径条目不一致（比较 {} 条）。\nfs 有而 MFT 缺失 {} 条（前 10 条）: {:?}\nMFT 多出 {} 条（前 10 条）: {:?}",
                fs_compared.len(),
                missing.len(),
                missing.iter().take(10).collect::<Vec<_>>(),
                extra.len(),
                extra.iter().take(10).collect::<Vec<_>>(),
            );
            compared_drives += 1;
        }
        assert!(
            compared_drives > 0,
            "真机上应至少有一个含 SID 目录的回收站分区"
        );
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
