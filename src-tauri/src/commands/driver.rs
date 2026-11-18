// ============================================================================
// Old driver cleanup commands
// ============================================================================

use crate::driver_cleanup::{DriverDeleteResult, DriverRestoreResult, DriverScanResult};
use crate::error::AppError;

/// Enumerate and analyze third-party driver packages in the Driver Store.
#[tauri::command]
pub async fn scan_old_drivers() -> Result<DriverScanResult, AppError> {
    tokio::task::spawn_blocking(crate::driver_cleanup::scan)
        .await
        .map_err(|error| format!("驱动扫描任务失败: {}", error))?
        .map_err(AppError::from)
}

/// Back up and delete the safe candidate driver packages selected by the user.
#[tauri::command]
pub async fn delete_old_drivers(
    published_names: Vec<String>,
) -> Result<DriverDeleteResult, AppError> {
    tokio::task::spawn_blocking(move || crate::driver_cleanup::delete(published_names))
        .await
        .map_err(|error| format!("驱动清理任务失败: {}", error))?
        .map_err(AppError::from)
}

/// Recursively restore all driver packages from driver_backups in the current data directory.
#[tauri::command]
pub async fn restore_all_driver_backups() -> Result<DriverRestoreResult, AppError> {
    tokio::task::spawn_blocking(crate::driver_cleanup::restore_all_backups)
        .await
        .map_err(|error| format!("驱动恢复任务失败: {}", error))?
        .map_err(AppError::from)
}

/// Open the driver backup directory so the user can inspect the exported INF packages.
#[tauri::command]
pub fn open_driver_backup_dir() -> Result<(), AppError> {
    let directory = crate::driver_cleanup::backup_directory()?;
    crate::commands::tools::open_in_folder(directory)
}
