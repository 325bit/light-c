//! Directory junction / symlink detection.
//!
//! Why needed: after moving a directory to another drive with a tool like FolderMove, the original location is left with a
//! **reparse point** (junction or symlink). The scanner follows it and reads the real files, but the UI still shows
//! the original drive letter — users think "my C drive is not that big" and cannot tell the directory is not actually on C anymore.
//! So here we resolve both "where this link points from and to" and "the final landing location" and hand them to the UI.
//!
//! Uses only four kernel32 functions via the `windows` crate:
//! - `GetFileAttributesW` determines whether it is a reparse point
//! - `CreateFileW(..., FILE_FLAG_OPEN_REPARSE_POINT)` + `DeviceIoControl(FSCTL_GET_REPARSE_POINT)`
//!   reads the tag, used to distinguish junctions from symlinks
//! - pass the target string to `std::fs::read_link` (std already handles the junction `\??\` prefix)
//!
//! ⚠️ Only "detect and display"; **traversal semantics are unchanged**: size counting still does not follow links (see `dev_cache::measure`
//! for details); otherwise a junction pointing back to a parent would make the scan loop in place.

use serde::Serialize;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetFileAttributesW, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_SHARE_MODE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, INVALID_FILE_ATTRIBUTES, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;

// Numeric-only constants kept local: pulling them in would require the
// Win32_System_Ioctl / Win32_System_SystemServices features for three u32s.
const FSCTL_GET_REPARSE_POINT: u32 = 0x0009_00A8;
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

/// Link type. The UI shows them separately — deleting a junction removes only the link; a symlink is similar in effect but different in origin,
/// so seeing the concrete type tells users which tool to use to change it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LinkKind {
    /// Directory symlink created by `mklink /D`
    Symlink,
    /// Directory junction created by `mklink /J`, most common with FolderMove / manual moves
    Junction,
}

/// Link relationships along a path.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkInfo {
    /// Link type (the frontend displays "symlink" / "directory junction" accordingly)
    pub kind: LinkKind,
    /// **Which level** of the path is the link.
    ///
    /// Usually not the scanned item itself: the user moved `.workbuddy` while what is scanned is `.workbuddy\logs`。
    pub link_path: String,
    /// The target recorded in the link (absolute path, `\??\` prefix already stripped)
    pub target: String,
    /// Where the whole path finally lands after following all links — i.e. where the space is really occupied
    pub resolved: String,
}

fn wide(value: &OsStr) -> Vec<u16> {
    let mut units: Vec<u16> = value.encode_wide().collect();
    units.push(0);
    units
}

fn is_reparse_point(path: &Path) -> bool {
    // SAFETY: the path is a NUL-terminated wide string living for the whole call
    let attrs = unsafe { GetFileAttributesW(PCWSTR::from_raw(wide(path.as_os_str()).as_ptr())) };
    attrs != INVALID_FILE_ATTRIBUTES && attrs & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
}

/// Reads the tag of a reparse point. Returns `None` if unreadable (permissions / not a reparse point).
///
/// ⚠️ The output buffer must be a full 16 KB (MAXIMUM_REPARSE_DATA_BUFFER_SIZE):
/// with only 4-8 bytes, `DeviceIoControl` fails outright with ERROR_MORE_DATA,
/// so all links are treated as "not links" — and it fails silently, hard to notice.
fn reparse_tag(path: &Path) -> Option<u32> {
    const MAXIMUM_REPARSE_DATA_BUFFER_SIZE: usize = 16 * 1024;

    // SAFETY: the path is a NUL-terminated wide string; the handle returned
    // here closes itself via HANDLE::drop (H4 contract §4.2, never CloseHandle
    // manually on a windows-crate HANDLE)
    let handle = match unsafe {
        CreateFileW(
            PCWSTR::from_raw(wide(path.as_os_str()).as_ptr()),
            FILE_GENERIC_READ.0,
            FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0),
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    } {
        Ok(handle) => handle,
        Err(_) => return None,
    };

    let mut buffer = vec![0u8; MAXIMUM_REPARSE_DATA_BUFFER_SIZE];
    let mut returned: u32 = 0;
    // SAFETY: buffer is the full 16 KB the FSCTL may write; handle is valid
    // from the CreateFileW check above
    let ok = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_GET_REPARSE_POINT,
            None,
            0,
            Some(buffer.as_mut_ptr() as *mut core::ffi::c_void),
            buffer.len() as u32,
            Some(&mut returned),
            None,
        )
    };
    if ok.is_err() || returned < 4 {
        return None;
    }
    Some(u32::from_le_bytes([
        buffer[0], buffer[1], buffer[2], buffer[3],
    ]))
}

/// Whether this path **itself** is a link; returns the concrete type.
pub fn kind_of(path: &Path) -> Option<LinkKind> {
    if !is_reparse_point(path) {
        return None;
    }
    match reparse_tag(path) {
        Some(IO_REPARSE_TAG_SYMLINK) => Some(LinkKind::Symlink),
        Some(IO_REPARSE_TAG_MOUNT_POINT) => Some(LinkKind::Junction),
        // Cloud placeholders (OneDrive on-demand files) are also reparse points, but that is not "moved away"; do not report them
        _ => None,
    }
}

/// Strip `\\?\` / `\\?\UNC\` prefixes into a user-friendly form.
fn friendly(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\");
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{}", rest);
    }
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
}

/// Final landing location of a path (following all links). Falls back to the original path if unresolvable.
fn final_path(path: &Path) -> String {
    match std::fs::canonicalize(path) {
        Ok(real) => friendly(&real),
        Err(_) => friendly(path),
    }
}

/// Analyzes link relationships along a whole path.
///
/// Check level by level down from the drive root, reporting only the **topmost** link — a moved directory with another link nested below it
/// is rare; reporting the first level is enough and avoids cluttering the UI. Returns `None` when there is no link.
pub fn analyze(path: &Path) -> Option<LinkInfo> {
    // ancestors() goes "from self upward"; reversing it gives root-down order, so we can take the outermost level
    let mut layers: Vec<PathBuf> = path.ancestors().map(Path::to_path_buf).collect();
    layers.reverse();

    for layer in layers {
        // Skip "C:" and "C:\" — meaningless levels
        if layer.to_string_lossy().len() <= 3 {
            continue;
        }
        let Some(kind) = kind_of(&layer) else {
            continue;
        };
        // std::fs::read_link works for both junctions and symlinks
        let target = std::fs::read_link(&layer)
            .map(|target| friendly(&target))
            .unwrap_or_default();
        return Some(LinkInfo {
            kind,
            link_path: friendly(&layer),
            target,
            resolved: final_path(path),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A normal directory should not be treated as a link.
    #[test]
    fn plain_directory_is_not_a_link() {
        let temp = std::env::temp_dir();
        assert!(kind_of(&temp).is_none());
        assert!(analyze(&temp.join("whatever")).is_none());
    }

    /// A genuinely relocated directory on this machine (`.workbuddy`, etc.) should yield its type and landing location.
    ///
    /// Assert only when "this machine really has moved `.workbuddy`";
    /// if it has not been moved, skip — do not count environment differences as failures.
    #[test]
    fn detects_real_moved_directory() {
        let Some(home) = std::env::var_os("USERPROFILE") else {
            return;
        };
        let link = PathBuf::from(home).join(".workbuddy");
        // Not moved (not a reparse point) → this test case has nothing to verify
        if !is_reparse_point(&link) {
            return;
        }
        let info =
            analyze(&link.join("logs")).expect("`.workbuddy` 是重解析点，但 analyze() 没识别出来");
        assert!(!info.link_path.is_empty());
        assert!(!info.target.is_empty());
        assert!(!info.resolved.is_empty());
        // The landing location should not still carry the `\\?\` prefix, or it looks ugly in the UI
        assert!(!info.resolved.starts_with(r"\\?\"));
        assert!(matches!(info.kind, LinkKind::Symlink | LinkKind::Junction));
    }
}
