// ============================================================================
// Shell icon cleanup commands
// The command layer only handles async scheduling and error boundaries; registry safety validation is centralized in scanner::shell_icons.
// ============================================================================

use crate::error::AppError;
use crate::scanner::{self, ShellIconOperationResult, ShellIconTarget};
use log::info;

#[tauri::command]
pub async fn scan_shell_icons() -> Result<Vec<scanner::ShellIconInfo>, AppError> {
    tokio::task::spawn_blocking(scanner::scan_shell_icons)
        .await
        .map_err(|error| format!("虚拟磁盘扫描任务失败: {}", error))?
        .map_err(AppError::from)
}

#[tauri::command]
pub async fn remove_shell_icon(
    target: ShellIconTarget,
    mode: u8,
) -> Result<ShellIconOperationResult, AppError> {
    info!("处理虚拟磁盘节点: {} / {}", target.hive, target.clsid);
    // The background task needs ownership of the target, but logs still use the original target, so keep a copy.
    let target_for_task = target.clone();
    let result =
        tokio::task::spawn_blocking(move || scanner::remove_shell_icon(&target_for_task, mode))
            .await
            .map_err(|error| format!("虚拟磁盘处理任务失败: {}", error))?;
    scanner::record_shell_icon_operation(
        &target,
        if mode == 2 { "彻底删除" } else { "删除" },
        &result,
    );
    result.map_err(AppError::from)
}

#[tauri::command]
pub async fn unlock_shell_icon(
    target: ShellIconTarget,
) -> Result<ShellIconOperationResult, AppError> {
    // The unlock operation also runs on a blocking thread; copy the target so later operation records are unaffected.
    let target_for_task = target.clone();
    let result = tokio::task::spawn_blocking(move || scanner::unlock_shell_icon(&target_for_task))
        .await
        .map_err(|error| format!("解锁虚拟磁盘任务失败: {}", error))?;
    scanner::record_shell_icon_operation(&target, "解除防复活", &result);
    result.map_err(AppError::from)
}

#[tauri::command]
pub async fn restore_shell_icon(
    target: ShellIconTarget,
) -> Result<ShellIconOperationResult, AppError> {
    // The restore operation may involve registry imports; copy the target before handing it to the blocking thread.
    let target_for_task = target.clone();
    let result = tokio::task::spawn_blocking(move || scanner::restore_shell_icon(&target_for_task))
        .await
        .map_err(|error| format!("恢复虚拟磁盘任务失败: {}", error))?;
    scanner::record_shell_icon_operation(&target, "恢复节点", &result);
    result.map_err(AppError::from)
}

#[tauri::command]
pub fn restart_explorer() -> Result<(), AppError> {
    scanner::restart_explorer().map_err(AppError::from)
}

#[tauri::command]
pub fn open_shell_icon_backup_dir() -> Result<(), AppError> {
    scanner::open_shell_icon_backup_dir().map_err(AppError::from)
}

#[tauri::command]
pub fn open_shell_icon_log() -> Result<(), AppError> {
    scanner::open_shell_icon_log().map_err(AppError::from)
}

#[tauri::command]
pub fn open_shell_icon_registry(target: ShellIconTarget) -> Result<(), AppError> {
    scanner::open_shell_icon_registry(&target).map_err(AppError::from)
}
