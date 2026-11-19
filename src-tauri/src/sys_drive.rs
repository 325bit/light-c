// ============================================================================
// System drives - the single owner of "which drives exist and what are they"
//
// M13 audit fix: drive enumeration was spelled out four times — the fixed-drive
// loop in deep_junk, the A-Z × 26 `is_dir` loop in recycle_bin, the C-Z
// `exists()` loop in social_scanner and the mask loop in commands/disk. All of
// them now come from here. The legacy `is_dir`/`exists` re-check is kept as a
// belt-and-braces filter so unmounted volumes never reach the Shell APIs, but
// the bitmask lookup replaces the blind 26-iteration probing.
// ============================================================================

use std::path::Path;

/// Extract the drive root (`X:\`) from a path's drive-letter prefix.
/// Returns `None` for UNC paths, relative paths and prefix-less strings.
pub fn drive_root_of(path: &Path) -> Option<String> {
    let path = path.to_string_lossy();
    let bytes = path.as_bytes();
    if bytes.len() < 2 || bytes[1] != b':' || !bytes[0].is_ascii_alphabetic() {
        return None;
    }
    Some(format!("{}:\\", (bytes[0] as char).to_ascii_uppercase()))
}

/// Drive letters that currently exist (mounted), from a single
/// `GetLogicalDrives` call instead of probing all 26 letters.
#[cfg(windows)]
pub fn existing_drive_letters() -> Vec<char> {
    // SAFETY: no parameters; the API only reads the drive bitmask
    let mask = unsafe { windows::Win32::Storage::FileSystem::GetLogicalDrives() };
    if mask == 0 {
        return Vec::new();
    }

    (0..26)
        .filter(|&index| mask & (1 << index) != 0)
        .map(|index| (b'A' + index as u8) as char)
        // `GetLogicalDrives` can report volumes without media (empty card
        // readers); keep the existence re-check so callers never hand an
        // unmounted root to the Shell APIs.
        .filter(|&letter| Path::new(&format!("{}:\\", letter)).is_dir())
        .collect()
}

/// Same as `existing_drive_letters`, as `X:\` roots.
#[cfg(windows)]
pub fn existing_drive_roots() -> Vec<String> {
    existing_drive_letters()
        .into_iter()
        .map(|letter| format!("{}:\\", letter))
        .collect()
}

/// Fixed (internal) disk drive letters, for scans that must skip removable and
/// network drives.
#[cfg(windows)]
pub fn fixed_drive_letters() -> Result<Vec<char>, String> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    use windows::Win32::System::WindowsProgramming::DRIVE_FIXED;

    // SAFETY: no parameters; the API only reads the drive bitmask
    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 {
        return Err("无法读取本机磁盘分区列表".to_string());
    }

    Ok((0..26)
        .filter_map(|index| {
            if mask & (1 << index) == 0 {
                return None;
            }
            let letter = (b'A' + index as u8) as char;
            let root = format!("{}:\\", letter);
            let wide_root = root
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect::<Vec<_>>();
            // SAFETY: wide_root is a NUL-terminated wide string living for the call
            let drive_type = unsafe { GetDriveTypeW(PCWSTR::from_raw(wide_root.as_ptr())) };
            (drive_type == DRIVE_FIXED && Path::new(&root).is_dir()).then_some(letter)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_root_of_extracts_the_letter_prefix() {
        assert_eq!(
            drive_root_of(Path::new(r"C:\Temp\x.tmp")),
            Some("C:\\".to_string())
        );
        assert_eq!(
            drive_root_of(Path::new("d:/cache/x")),
            Some("D:\\".to_string())
        );
        assert_eq!(drive_root_of(Path::new("C:\\")), Some("C:\\".to_string()));

        // No drive-letter prefix: extended-length paths (callers must strip the
        // `\\?\` prefix first, e.g. via path_guard::normalize), UNC paths,
        // relative paths, bare names
        assert_eq!(drive_root_of(Path::new(r"\\?\C:\Windows")), None);
        assert_eq!(drive_root_of(Path::new(r"\\server\share\dir")), None);
        assert_eq!(drive_root_of(Path::new(r"Temp\..\x")), None);
        assert_eq!(drive_root_of(Path::new("")), None);
    }
}
