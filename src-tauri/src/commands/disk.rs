// ============================================================================
// Disk info commands
// ============================================================================

use crate::error::{AppError, ErrorCode};
use log::info;
use serde::Serialize;

/// Disk info
#[derive(Debug, Serialize)]
pub struct DiskInfo {
    pub total_space: u64,
    pub used_space: u64,
    pub free_space: u64,
    pub usage_percent: f32,
    pub drive_letter: String,
}

/// Fixed partitions of this machine; reused by multi-disk modules such as big files and big directories.
#[derive(Debug, Serialize)]
pub struct LocalDriveInfo {
    pub drive_letter: String,
    pub root_path: String,
    pub volume_name: String,
    pub file_system: String,
    pub total_space: u64,
    pub used_space: u64,
    pub free_space: u64,
    pub usage_percent: f32,
    pub is_system: bool,
    pub is_ntfs: bool,
}

/// Get disk info for drive C
#[tauri::command]
pub fn get_disk_info() -> Result<DiskInfo, AppError> {
    info!("获取磁盘信息");

    #[cfg(target_os = "windows")]
    {
        // Default to the system drive (%SYSTEMDRIVE%), compatible with machines where the system drive is not named C
        let drive = query_drive_info(crate::system_info::system_drive_letter())?;

        Ok(DiskInfo {
            total_space: drive.total_space,
            used_space: drive.used_space,
            free_space: drive.free_space,
            usage_percent: drive.usage_percent,
            drive_letter: drive.drive_letter,
        })
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err(AppError::new(ErrorCode::Unsupported))
    }
}

/// Get the list of fixed disk partitions on this machine.
#[tauri::command]
pub fn get_local_drives() -> Result<Vec<LocalDriveInfo>, AppError> {
    info!("获取本机固定磁盘分区列表");

    #[cfg(target_os = "windows")]
    {
        // M13 fix: single drive enumeration source (crate::sys_drive)
        let mut drives = Vec::new();
        for letter in crate::sys_drive::fixed_drive_letters()? {
            let root = format!("{}:\\", letter);
            match query_drive_info(letter) {
                Ok(drive) => drives.push(drive),
                Err(error) => {
                    // A single partition error should not block the whole list, so a bad disk or permission issue does not drag down the whole settings entry.
                    log::warn!("读取磁盘 {} 信息失败: {}", root, error);
                }
            }
        }

        drives.sort_by(|left, right| left.drive_letter.cmp(&right.drive_letter));
        Ok(drives)
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err(AppError::new(ErrorCode::Unsupported))
    }
}

#[cfg(target_os = "windows")]
fn query_drive_info(letter: char) -> Result<LocalDriveInfo, AppError> {
    use winapi::um::fileapi::GetDiskFreeSpaceExW;
    use winapi::um::winnt::ULARGE_INTEGER;

    let normalized_letter = letter.to_ascii_uppercase();
    if !normalized_letter.is_ascii_alphabetic() {
        return Err(AppError::new(ErrorCode::InvalidArgument).arg("letter", letter.to_string()));
    }

    let root = format!("{}:\\", normalized_letter);
    let wide_root = wide_null(&root);
    let mut free_bytes_available: ULARGE_INTEGER = unsafe { std::mem::zeroed() };
    let mut total_bytes: ULARGE_INTEGER = unsafe { std::mem::zeroed() };
    let mut total_free_bytes: ULARGE_INTEGER = unsafe { std::mem::zeroed() };

    let result = unsafe {
        GetDiskFreeSpaceExW(
            wide_root.as_ptr(),
            &mut free_bytes_available,
            &mut total_bytes,
            &mut total_free_bytes,
        )
    };
    if result == 0 {
        return Err(AppError::new(ErrorCode::ExternalFailed)
            .detail("GetDiskFreeSpaceExW returned 0")
            .arg("drive", root));
    }

    let (volume_name, file_system) = query_volume_labels(&wide_root);
    let total = unsafe { *total_bytes.QuadPart() };
    let free = unsafe { *total_free_bytes.QuadPart() };
    let used = total.saturating_sub(free);
    let usage_percent = if total > 0 {
        (used as f64 / total as f64 * 100.0) as f32
    } else {
        0.0
    };
    let drive_letter = format!("{}:", normalized_letter);
    let system_drive = std::env::var("SystemDrive")
        .unwrap_or_else(|_| "C:".to_string())
        .trim_end_matches('\\')
        .to_ascii_uppercase();

    Ok(LocalDriveInfo {
        drive_letter: drive_letter.clone(),
        root_path: root,
        volume_name,
        is_ntfs: file_system.eq_ignore_ascii_case("NTFS"),
        file_system,
        total_space: total,
        used_space: used,
        free_space: free,
        usage_percent,
        is_system: drive_letter == system_drive,
    })
}

#[cfg(target_os = "windows")]
fn query_volume_labels(root: &[u16]) -> (String, String) {
    use winapi::um::fileapi::GetVolumeInformationW;

    let mut volume_name = vec![0u16; 260];
    let mut file_system = vec![0u16; 64];
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            volume_name.as_mut_ptr(),
            volume_name.len() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            file_system.as_mut_ptr(),
            file_system.len() as u32,
        )
    };

    if ok == 0 {
        return (String::new(), String::new());
    }

    (
        utf16_buffer_to_string(&volume_name),
        utf16_buffer_to_string(&file_system),
    )
}

#[cfg(target_os = "windows")]
fn utf16_buffer_to_string(buffer: &[u16]) -> String {
    let len = buffer
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

#[cfg(target_os = "windows")]
fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
