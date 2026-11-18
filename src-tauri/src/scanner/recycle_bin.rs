// ============================================================================
// Recycle bin scanner
// Only returns valid entries the current user can see in the Windows Recycle Bin UI
// ============================================================================

use super::{CategoryScanResult, FileInfo, JunkCategory};
use log::{debug, warn};
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::sync::OnceLock;

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
        for drive_letter in crate::sys_drive::existing_drive_letters() {
            let sid_path = PathBuf::from(format!("{}:\\$Recycle.Bin\\{}", drive_letter, user_sid));
            collect_drive_entries(&sid_path, category, &mut entries);
        }

        entries.sort_by(|left, right| left.path.cmp(&right.path));
        for entry in entries {
            result.add_file(entry);
        }
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
fn collect_drive_entries(sid_path: &Path, category: &JunkCategory, entries: &mut Vec<FileInfo>) {
    let directory_entries = match fs::read_dir(sid_path) {
        Ok(entries) => entries,
        Err(error) => {
            debug!("无法读取回收站目录 {:?}: {}", sid_path, error);
            return;
        }
    };

    // One read_dir collects both $I and $R: the old implementation also did exists() + is_dir() for each entry
    // two stats; on a cold read, thousands of entries across four partitions meant tens of thousands of metadata accesses — measured first scan
    // of the recycle bin took 7.3 s, while a warm scan took only 2 s; the slowness was all here.
    // On Windows, DirEntry::file_type() comes directly from the directory enumeration result, with no extra system call.
    let mut metadata_files: Vec<(PathBuf, String)> = Vec::new();
    // Lowercase extensions before comparing: NTFS is case-insensitive, so the $I/$R base parts may not share the same casing
    let mut data_dirs: HashMap<String, bool> = HashMap::new();

    for directory_entry in directory_entries.filter_map(Result::ok) {
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

    // Each $I must be opened and read separately; they are fully independent, so they must run in parallel.
    // Reading tens of thousands of small files serially on a cold disk means tens of thousands of random I/Os:
    // measured with 33052 entries in the E drive recycle bin: first scan 16.3 s, warm scan only 1.3 s.
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

    entries.extend(found);
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
    use std::ptr::null_mut;
    use winapi::shared::minwindef::{DWORD, HLOCAL, LPVOID};
    use winapi::shared::ntdef::HANDLE;
    use winapi::shared::sddl::ConvertSidToStringSidW;
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::processthreadsapi::{GetCurrentProcess, OpenProcessToken};
    use winapi::um::securitybaseapi::GetTokenInformation;
    use winapi::um::winbase::LocalFree;
    use winapi::um::winnt::{TokenUser, TOKEN_QUERY, TOKEN_USER};

    unsafe {
        let mut token_handle: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token_handle) == 0 {
            return None;
        }

        let mut required_size: DWORD = 0;
        GetTokenInformation(token_handle, TokenUser, null_mut(), 0, &mut required_size);
        if required_size == 0 {
            CloseHandle(token_handle);
            return None;
        }

        // Use a u64 buffer to satisfy TOKEN_USER's alignment requirement on Windows targets.
        let mut buffer = vec![0u64; (required_size as usize + 7) / 8];
        let success = GetTokenInformation(
            token_handle,
            TokenUser,
            buffer.as_mut_ptr() as LPVOID,
            required_size,
            &mut required_size,
        );
        if success == 0 {
            CloseHandle(token_handle);
            return None;
        }

        let token_user = buffer.as_ptr() as *const TOKEN_USER;
        let mut sid_string: *mut u16 = null_mut();
        let converted = ConvertSidToStringSidW((*token_user).User.Sid, &mut sid_string);
        CloseHandle(token_handle);
        if converted == 0 || sid_string.is_null() {
            return None;
        }

        let sid_length = (0..)
            .take_while(|offset| *sid_string.add(*offset) != 0)
            .count();
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(sid_string, sid_length));
        LocalFree(sid_string as HLOCAL);
        Some(sid)
    }
}
