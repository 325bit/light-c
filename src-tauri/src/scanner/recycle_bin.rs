// ============================================================================
// Recycle bin scanner
// Only returns valid entries the current user can see in the Windows Recycle Bin UI
// ============================================================================

use super::{CategoryScanResult, FileInfo, JunkCategory};
use crate::scanner::big_files_engine::mft_core;
use log::{debug, info, warn};
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::sync::OnceLock;
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

/// 生产入口：按运行时环境选引擎——**管理员 + NTFS 一律走 MFT 直读**，其余回退 fs 枚举。
///
/// 回退是**整盘**的：一个盘要么全走 MFT、要么全走 fs，不允许同盘两种口径混用（混用会让
/// 条目数与大小的来源变成"两套"的平均值，排查时无从归因）。任何 MFT 侧失败——卷设备打不开、
/// `$MFT` 布局异常、SID 目录句柄取不到记录号——都退化为 fs，功能不会因此不可用。
///
/// 不按条目数分档是本轮的决定：MFT 一遍读的成本近乎常量（$MFT 大小 / 顺序读带宽），而 fs 的
/// 成本是"每条目一次随机小读"。两遍合一后 MFT 侧约 2.3s，小卷（C 盘 2.9 万条目、fs 1.9s）只
/// 慢 0.4s，用这点时间换掉整套阈值逻辑与两套口径，是划算的。
#[cfg(windows)]
fn collect_drive_entries(
    sid_path: &Path,
    drive_letter: char,
    category: &JunkCategory,
) -> (Vec<FileInfo>, DrivePhaseTiming) {
    let elevated = mft_core::is_elevated();
    let ntfs = mft_core::is_ntfs(drive_letter);
    if elevated && ntfs {
        match scan_drive_via_mft(drive_letter, sid_path, category) {
            Ok(result) => return result,
            Err(error) => warn!(
                "[回收站扫描] {}: MFT 直读失败，整盘回退 fs 枚举：{}",
                drive_letter, error
            ),
        }
    } else {
        debug!(
            "[回收站扫描] {}: 管理员 {} / NTFS {} → fs 枚举",
            drive_letter, elevated, ntfs
        );
    }
    collect_drive_entries_via_fs(sid_path, drive_letter, category)
}

#[cfg(windows)]
fn collect_drive_entries_via_fs(
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

/// One drive's recycle-bin entries via a **single** sequential `$MFT` read instead of
/// ~150k random small-file reads.
///
/// 正确性门槛：真机对照（`real_machine_mft_path_matches_fs_path`）要求与 fs 枚举**双向逐条
/// 相等**。曾经约 25% 的 $R 条目缺失，根因不是解析或读取，而是 `parse_record_info` 每条记录
/// 只留一个名字：rustc incremental 的硬链接被逐个改名进回收站，一条记录背上 2~40 个
/// `$FILE_NAME`，单名字扫描只登记了第一个（详见 `collect_file_names`）。
///
/// 定位的权威来源是 OS 文件索引（打开目录句柄拿记录号），**不是名字匹配**：实测 C 盘该目录的
/// `$FILE_NAME` 只解析出 8.3 短名 "S-1-5-~1"，完整 SID 字符串匹配永远落空（上一轮 gate 的失败根因）。
///
/// 拿到记录号之后，本次扫描要的东西**全落在"父目录 = 它"的记录上**——$R 的配对表（名字 +
/// is_dir）与 $I 的 resident 内容。于是两者能在同一次全表读里产出：以前为了一半信息把 1.8GB
/// 的 $MFT 再读一遍（D 盘实测 2212 + 2051 = 4263ms），合并后只需一遍（约 2.3s）。
#[cfg(windows)]
fn scan_drive_via_mft(
    drive_letter: char,
    sid_path: &Path,
    category: &JunkCategory,
) -> Result<(Vec<FileInfo>, DrivePhaseTiming), String> {
    let started = Instant::now();
    let reader = mft_core::NtfsFileMetadataReader::open(drive_letter)?;
    let sid_dir_id = sid_dir_record_id_via_handle(sid_path)?;
    let locate_ms = started.elapsed().as_millis();

    let read_started = Instant::now();
    let read = reader.read_children_with_resident_data(
        sid_dir_id,
        // 内容侧谓词：这条记录只要在目标目录下有任一 `$I…` 名字，就取它的 resident `$DATA`
        &|name: &str| {
            name.get(..2)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("$I"))
        },
        &|_| true,
    )?;
    let read_ms = read_started.elapsed().as_millis();
    info!(
        "[回收站扫描耗时] mft {}: 定位 {}ms（SID 目录记录号 {}）；单遍读表 {}ms（io {}ms / parse {}ms）→ 目录项 {} 条，$I 内容命中 {} 条；{}",
        drive_letter,
        locate_ms,
        sid_dir_id,
        read_ms,
        read.stats.io_ms,
        read.stats.parse_ms,
        read.entries.len(),
        read.resident_hits,
        reader.layout_summary(),
    );

    // ---- $R 配对表：与 fs 路径同语义（孤儿 $I 丢弃，$R 目录标记 is_dir）----
    let mut data_dirs: HashMap<String, bool> = HashMap::with_capacity(read.entries.len() / 2 + 16);
    for entry in &read.entries {
        if entry.name.len() <= 2 {
            continue;
        }
        if let Some(suffix) = entry.name.strip_prefix("$R") {
            data_dirs.insert(suffix.to_ascii_lowercase(), entry.is_dir);
        }
    }
    // $I → resident 内容：同一次读产出的另一半，按记录号关联（$I 文件不会被硬链接，名字唯一）
    let mut resident_by_record: HashMap<u64, &[u8]> =
        HashMap::with_capacity(read.resident.len() * 2);
    for (mft_id, data) in &read.resident {
        resident_by_record.insert(*mft_id, data.as_slice());
    }

    let pair_started = Instant::now();
    let mut entries: Vec<FileInfo> = Vec::with_capacity(data_dirs.len());
    let mut non_resident_fallbacks = 0usize;
    let mut orphan_drops = 0usize;
    let mut parse_drops = 0usize;
    for entry in &read.entries {
        let Some(suffix) = entry.name.strip_prefix("$I") else {
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
                continue;
            }
        };

        let metadata_path = sid_path.join(&entry.name);
        let resident = resident_by_record
            .get(&entry.mft_id)
            .copied()
            .unwrap_or(&[]);
        let (logical_size, original_path, deleted_at) = if resident.is_empty() {
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
            match parse_metadata_bytes(resident) {
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
                    .join(format!("$R{suffix}"))
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
    let pair_ms = pair_started.elapsed().as_millis();

    info!(
        "[回收站扫描耗时] mft {}: 配对 {}ms → 条目 {}（孤儿 $I 丢弃 {} / 解析失败 {} / 非 resident 兜底 {}）",
        drive_letter,
        pair_ms,
        entries.len(),
        orphan_drops,
        parse_drops,
        non_resident_fallbacks
    );

    let timing = DrivePhaseTiming {
        backend: "mft",
        drive_letter,
        // 枚举相 = 定位 + 单遍读表；解析相 = 纯内存配对（不再读盘）
        enumerate_ms: locate_ms + read_ms,
        parse_ms: pair_ms,
        directory_entries: read.entries.len(),
        metadata_files: read.resident_hits,
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
    use super::{collect_drive_entries_via_fs, current_user_sid, scan_drive_via_mft};
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

                // 对照的 fs 侧必须**显式**走 fs 引擎：若这里调生产入口 `collect_drive_entries`，
                // 管理员态下它会自动选 MFT，对照就退化成"MFT vs MFT"，永远相等、永远通过。
                let (fs_entries, fs_timing) =
                    collect_drive_entries_via_fs(&sid_path, drive_letter, &JunkCategory::RecycleBin);
                let (mft_entries, mft_timing) = match scan_drive_via_mft(
                    drive_letter,
                    &sid_path,
                    &JunkCategory::RecycleBin,
                ) {
                    Ok(result) => result,
                    Err(error) => panic!("{drive_letter}: 盘 MFT 直读不可用: {error}"),
                };
                let (fs_entries_after, _) = collect_drive_entries_via_fs(
                    &sid_path,
                    drive_letter,
                    &JunkCategory::RecycleBin,
                );
                println!(
                "{drive_letter}: 盘 mft 计时：定位+读表 {}ms / 内存配对 {}ms / 目录项 {} 条 / $I 内容 {} 条 / 非 resident 兜底 {} 次",
                mft_timing.enumerate_ms,
                mft_timing.parse_ms,
                mft_timing.directory_entries,
                mft_timing.metadata_files,
                mft_timing.non_resident_fallbacks,
            );
                // 切生产的收益分母就在这一行：fs 枚举（read_dir + 并行 $I 读）的耗时
                println!(
                    "{drive_letter}: 盘 fs 计时：{} {}ms（read_dir {} + 并行 $I 读解析 {}）",
                    fs_timing.backend,
                    fs_timing.enumerate_ms + fs_timing.parse_ms,
                    fs_timing.enumerate_ms,
                    fs_timing.parse_ms,
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
