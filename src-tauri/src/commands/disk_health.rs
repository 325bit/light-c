// ============================================================================
// Disk info commands
// ============================================================================

use crate::error::AppError;

/// Read basic physical disk info and Windows Storage health status.
#[tauri::command]
pub async fn get_disk_health() -> Result<Vec<crate::disk_health::DiskHealthInfo>, AppError> {
    // CIM queries are blocking IO, so run them on a dedicated thread to avoid stalling Tauri's async runtime.
    tokio::task::spawn_blocking(crate::disk_health::query_disk_health)
        .await
        .map_err(|error| format!("磁盘信息任务执行失败: {}", error))?
        .map_err(AppError::from)
}
