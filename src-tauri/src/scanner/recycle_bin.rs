// ============================================================================
// Recycle bin scanner
// Only returns valid entries the current user can see in the Windows Recycle Bin UI
// ============================================================================

use super::{CategoryScanResult, FileInfo, JunkCategory};
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
/// 23s wall-clock diagnosis: enumerate (read_dir + $I/$R pairing) vs parse
/// (parallel $I file reads) have different cost structures and different fixes.
struct DrivePhaseTiming {
    drive_letter: char,
    /// read_dir enumeration + $I/$R pairing (serial, one thread).
    enumerate_ms: u128,
    /// Parallel $I metadata reads and parsing.
    parse_ms: u128,
    /// Entries seen by read_dir ($I + $R + anything else in the SID dir).
    directory_entries: usize,
    /// $I metadata files matched by the pairing.
    metadata_files: usize,
    /// Recycle-bin entries emitted for this drive.
    found_entries: usize,
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
                    drive_letter,
                    enumerate_ms: drive_started.elapsed().as_millis(),
                    parse_ms: 0,
                    directory_entries: 0,
                    metadata_files: 0,
                    found_entries: 0,
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
        drive_letter,
        enumerate_ms,
        parse_ms,
        directory_entries: seen_entries,
        metadata_files: metadata_count,
        found_entries: found.len(),
    };
    (found, timing)
}

/// One info line per drive plus a wall-clock total: this is the data that
/// decides how to parallelize the dedicated scanner (drives are scanned
/// serially today, while $I reads already run on the rayon pool).
#[cfg(windows)]
fn log_scan_phase_timings(timings: &[DrivePhaseTiming], wall: std::time::Duration, entries: usize) {
    for timing in timings {
        info!(
            "[回收站扫描耗时] {}: 枚举 {}ms ({} 项, $I {} 个) + 解析 {}ms → 条目 {}",
            timing.drive_letter,
            timing.enumerate_ms,
            timing.directory_entries,
            timing.metadata_files,
            timing.parse_ms,
            timing.found_entries,
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
    use crate::scanner::{CategoryScanResult, JunkCategory};

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
