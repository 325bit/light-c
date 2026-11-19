// ============================================================================
// System health score module
// Calculate the health score from three dimensions: disk space, hibernation file, and junk files.
// The disk-space dimension follows the partition selected in the top bar; the hibernation file and system junk are on the system drive, so they always count against the system drive.
// ============================================================================

use log::info;
use serde::Serialize;

/// System health score result
#[derive(Debug, Clone, Serialize)]
pub struct HealthScoreResult {
    /// Total score (0-100)
    pub score: u32,
    /// Score for free space on the currently selected partition (0-40)
    pub disk_score: u32,
    /// Hibernation file score (0-30)
    pub hibernation_score: u32,
    /// Junk file score (0-30)
    pub junk_score: u32,
    /// Free percentage of the currently selected partition
    pub disk_free_percent: f64,
    /// Whether a hibernation file exists
    pub has_hibernation: bool,
    /// Hibernation file size
    pub hibernation_size: u64,
    /// Estimated junk file size
    pub junk_size: u64,
}

/// Calculate the system health score
///
/// `drive_letter` is the partition selected in the top bar; passing None uses the system drive (preserving old behavior).
/// Only the "disk space" dimension follows the partition: the hibernation file and system junk are on the system drive,
/// switching to D: will not move hiberfil.sys or %TEMP%, so those two always count against the system drive.
///
/// Scoring algorithm:
/// - Free percentage of the selected partition (40% weight): more free space means a higher score
/// - Hibernation file (30% weight): no hibernation file gets full marks; otherwise points are deducted by size
/// - Junk files (30% weight): less junk means a higher score
pub fn calculate(drive_letter: Option<&str>) -> HealthScoreResult {
    info!("计算系统健康评分...");

    let (disk_free_percent, disk_score) = calculate_disk_score(drive_letter);
    let (has_hibernation, hibernation_size, hibernation_score) = calculate_hibernation_score();
    let (junk_size, junk_score) = calculate_junk_score();

    let score = disk_score + hibernation_score + junk_score;

    info!(
        "健康评分: {} (磁盘:{}, 休眠:{}, 垃圾:{})",
        score, disk_score, hibernation_score, junk_score
    );

    HealthScoreResult {
        score,
        disk_score,
        hibernation_score,
        junk_score,
        disk_free_percent,
        has_hibernation,
        hibernation_size,
        junk_size,
    }
}

/// Normalize the drive letter from the frontend into that drive's root directory (e.g., the C: root).
///
/// Only take the first ASCII letter and reassemble; do not splice the user string directly into a Win32 API path -
/// the command layer is externally callable, so a fake path with subdirectories must not be treated as a valid drive letter.
/// If absent or invalid, fall back to the system drive (%SYSTEMDRIVE%, compatible with machines where the system drive is not C:).
#[cfg(target_os = "windows")]
fn disk_score_root(drive_letter: Option<&str>) -> String {
    let letter = drive_letter
        .and_then(|value| value.chars().find(|ch| ch.is_ascii_alphabetic()))
        .map(|ch| ch.to_ascii_uppercase())
        .unwrap_or_else(crate::system_info::system_drive_letter);
    let mut root = String::with_capacity(3);
    root.push(letter);
    root.push(':');
    root.push(std::path::MAIN_SEPARATOR);
    root
}

/// Calculate the disk space score (max 40)
fn calculate_disk_score(drive_letter: Option<&str>) -> (f64, u32) {
    #[cfg(target_os = "windows")]
    {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

        // Default to the system drive; after a top-bar switch, use the selected partition; the drive letter is already normalized in disk_score_root
        let drive_root = disk_score_root(drive_letter);
        let path: Vec<u16> = OsStr::new(&drive_root)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let mut free_bytes: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut _total_free: u64 = 0;

        // SAFETY: path is NUL-terminated and the three u64 out-params live for
        // the whole duration of the call
        let success = unsafe {
            GetDiskFreeSpaceExW(
                PCWSTR::from_raw(path.as_ptr()),
                Some(&mut free_bytes),
                Some(&mut total_bytes),
                Some(&mut _total_free),
            )
        };

        if success.is_ok() && total_bytes > 0 {
            let free_percent = (free_bytes as f64 / total_bytes as f64) * 100.0;
            let score = if free_percent >= 30.0 {
                40
            } else if free_percent >= 20.0 {
                30 + ((free_percent - 20.0) / 10.0 * 10.0) as u32
            } else if free_percent >= 10.0 {
                20 + ((free_percent - 10.0) / 10.0 * 10.0) as u32
            } else if free_percent >= 5.0 {
                10 + ((free_percent - 5.0) / 5.0 * 10.0) as u32
            } else {
                (free_percent / 5.0 * 10.0) as u32
            };
            return (free_percent, score.min(40));
        }
    }

    (50.0, 20) // default value on non-Windows or when the call fails
}

/// Calculate the hibernation file score (max 30)
fn calculate_hibernation_score() -> (bool, u64, u32) {
    let hiberfil_path = std::path::Path::new("C:\\hiberfil.sys");

    if hiberfil_path.exists() {
        let size = std::fs::metadata(hiberfil_path)
            .map(|m| m.len())
            .unwrap_or(0);

        let score = if size < 4 * 1024 * 1024 * 1024 {
            20
        } else if size < 8 * 1024 * 1024 * 1024 {
            15
        } else if size < 16 * 1024 * 1024 * 1024 {
            10
        } else {
            5
        };

        (true, size, score)
    } else {
        (false, 0, 30)
    }
}

/// Calculate the junk file score (max 30)
fn calculate_junk_score() -> (u64, u32) {
    let mut total_junk_size: u64 = 0;

    let junk_paths = [
        std::env::var("TEMP").unwrap_or_default(),
        std::env::var("TMP").unwrap_or_default(),
        format!(
            "{}\\AppData\\Local\\Temp",
            std::env::var("USERPROFILE").unwrap_or_default()
        ),
        // The system directory follows %SystemRoot%, compatible with machines where the system drive is not C:
        crate::system_info::system_root_dir()
            .join("Temp")
            .to_string_lossy()
            .into_owned(),
        crate::system_info::system_root_dir()
            .join("Prefetch")
            .to_string_lossy()
            .into_owned(),
    ];

    for path_str in &junk_paths {
        if path_str.is_empty() {
            continue;
        }
        let path = std::path::Path::new(path_str);
        if path.exists() && path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    if let Ok(metadata) = entry.metadata() {
                        if metadata.is_file() {
                            total_junk_size += metadata.len();
                        }
                    }
                }
            }
        }
    }

    // Simplified estimate for the recycle bin
    if std::path::Path::new("C:\\$Recycle.Bin").exists() {
        total_junk_size += 100 * 1024 * 1024;
    }

    let score = if total_junk_size < 500 * 1024 * 1024 {
        30
    } else if total_junk_size < 1024 * 1024 * 1024 {
        25
    } else if total_junk_size < 2 * 1024 * 1024 * 1024 {
        20
    } else if total_junk_size < 5 * 1024 * 1024 * 1024 {
        15
    } else if total_junk_size < 10 * 1024 * 1024 * 1024 {
        10
    } else {
        5
    };

    (total_junk_size, score)
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    /// The value from a top-bar drive switch must first be normalized into a single drive-letter root:
    /// this ensures "d" / "D:" are equivalent and that fake paths with subdirectories never reach the Win32 API.
    #[test]
    fn disk_score_root_normalizes_drive_letter() {
        let separator = std::path::MAIN_SEPARATOR;
        let system_root = format!(
            "{}:{}",
            crate::system_info::system_drive_letter(),
            separator
        );

        assert_eq!(disk_score_root(None), system_root);
        assert_eq!(disk_score_root(Some("")), system_root);
        assert_eq!(disk_score_root(Some("123")), system_root);
        assert_eq!(disk_score_root(Some("D:")), format!("D:{}", separator));
        assert_eq!(disk_score_root(Some("d")), format!("D:{}", separator));
        assert_eq!(
            disk_score_root(Some("E:/Users")),
            format!("E:{}", separator)
        );
    }
}
