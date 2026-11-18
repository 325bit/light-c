// ============================================================================
// Shell icon cleanup commands
// The command layer only handles async scheduling and error boundaries; registry safety validation is centralized in scanner::shell_icons.
// ============================================================================

use crate::scanner::{self, ShellIconOperationResult, ShellIconTarget};
use log::info;

#[tauri::command]
pub async fn scan_shell_icons() -> Result<Vec<scanner::ShellIconInfo>, String> {
    tokio::task::spawn_blocking(scanner::scan_shell_icons)
        .await
        .map_err(|error| format!("虚拟磁盘扫描任务失败: {}", error))?
}

#[tauri::command]
pub async fn remove_shell_icon(
    target: ShellIconTarget,
    mode: u8,
) -> Result<ShellIconOperationResult, String> {
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
    result
}

#[tauri::command]
pub async fn unlock_shell_icon(
    target: ShellIconTarget,
) -> Result<ShellIconOperationResult, String> {
    // The unlock operation also runs on a blocking thread; copy the target so later operation records are unaffected.
    let target_for_task = target.clone();
    let result = tokio::task::spawn_blocking(move || scanner::unlock_shell_icon(&target_for_task))
        .await
        .map_err(|error| format!("解锁虚拟磁盘任务失败: {}", error))?;
    scanner::record_shell_icon_operation(&target, "解除防复活", &result);
    result
}

#[tauri::command]
pub async fn restore_shell_icon(
    target: ShellIconTarget,
) -> Result<ShellIconOperationResult, String> {
    // The restore operation may involve registry imports; copy the target before handing it to the blocking thread.
    let target_for_task = target.clone();
    let result = tokio::task::spawn_blocking(move || scanner::restore_shell_icon(&target_for_task))
        .await
        .map_err(|error| format!("恢复虚拟磁盘任务失败: {}", error))?;
    scanner::record_shell_icon_operation(&target, "恢复节点", &result);
    result
}

#[tauri::command]
pub fn restart_explorer() -> Result<(), String> {
    scanner::restart_explorer()
}

#[tauri::command]
pub fn open_shell_icon_backup_dir() -> Result<(), String> {
    scanner::open_shell_icon_backup_dir()
}

#[tauri::command]
pub fn open_shell_icon_log() -> Result<(), String> {
    scanner::open_shell_icon_log()
}

#[tauri::command]
pub fn open_shell_icon_registry(target: ShellIconTarget) -> Result<(), String> {
    scanner::open_shell_icon_registry(&target)
}
