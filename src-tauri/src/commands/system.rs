// ============================================================================
// System-related commands (system slim + health score + system info)
// ============================================================================

use log::info;
use tauri::Window;

// Re-export for use by the frontend
pub use crate::health_score::HealthScoreResult;
pub use crate::system_info::SystemInfo;
pub use crate::system_slim::SystemSlimStatus;

// ============================================================================
// System slim
// ============================================================================

/// Check whether running with administrator privileges
#[tauri::command]
pub fn check_admin_privilege() -> bool {
    crate::system_slim::check_admin()
}

/// Restart the current program with administrator privileges.
///
/// Go through standard Windows UAC (the `runas` verb of `ShellExecuteW`), no bypass and no silent elevation —
/// the user still has to confirm in the system prompt.
///
/// Returns `true` meaning the admin instance has been launched and the current process will then exit automatically;
/// returns `false` meaning the user cancelled UAC (or it was already admin, no restart needed).
/// Cancellation is a normal choice, not treated as failure, and callers should not show an error prompt.
#[tauri::command]
pub fn relaunch_as_admin(app: tauri::AppHandle) -> Result<bool, String> {
    if crate::system_slim::check_admin() {
        // Already admin: no need for the caller to restart
        return Ok(false);
    }

    let exe_path = std::env::current_exe()
        .map_err(|error| format!("无法定位当前程序路径: {}", error))?;

    #[cfg(target_os = "windows")]
    {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use winapi::um::shellapi::ShellExecuteW;
        use winapi::um::winuser::SW_SHOWNORMAL;

        /// Convert to a NUL-terminated wide string; ShellExecuteW only accepts this form
        fn to_wide(value: &OsStr) -> Vec<u16> {
            value.encode_wide().chain(std::iter::once(0)).collect()
        }

        let operation = to_wide(OsStr::new("runas"));
        let file = to_wide(exe_path.as_os_str());
        let parameters = to_wide(OsStr::new(""));
        // The working directory follows the exe, so portable builds' relative-path detection of the data directory stays correct
        let working_directory = exe_path
            .parent()
            .map(|parent| to_wide(parent.as_os_str()))
            .unwrap_or_else(|| to_wide(OsStr::new(".")));

        // SAFETY: all four strings are NUL-terminated wide chars and live for the whole duration of the call
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                operation.as_ptr(),
                file.as_ptr(),
                parameters.as_ptr(),
                working_directory.as_ptr(),
                SW_SHOWNORMAL,
            )
        };

        // Convention: return value <= 32 means failure; 1223 = user clicked "No" in UAC
        let code = result as isize;
        if code <= 32 {
            if code == 1223 || code == 5 {
                info!("用户取消了管理员权限请求（错误码 {}）", code);
                return Ok(false);
            }
            return Err(format!("请求管理员权限失败（ShellExecute 返回 {}）", code));
        }
        info!("已启动管理员实例，准备退出当前进程");
    }

    #[cfg(not(target_os = "windows"))]
    return Err("只有 Windows 需要提权重启".to_string());

    // Two instances running at once would compete for the same log and config files, so the old process must step aside.
    // The 300ms delay leaves an initialization window for the new instance — it reads config immediately on startup and would read a half-written file if it hit the old process mid-write.
    let app_handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        app_handle.exit(0);
    });

    Ok(true)
}

/// Get the system slim status.
///
/// WinSxS DISM analysis runs as a background task; this command only returns the currently known result plus an `analyzing` flag,
/// and pushes the two items via the `system-slim:winsxs-ready` event when analysis completes, so a window is needed to emit events.
///
/// `force_refresh`: pass true when the user manually clicks "recheck" to skip the 10-minute cache and really rerun DISM once.
/// When not passed (or false), use the cache, for scenarios like automatic refresh.
#[tauri::command]
pub async fn get_system_slim_status(
    window: Window,
    force_refresh: Option<bool>,
) -> SystemSlimStatus {
    crate::system_slim::get_status(window, force_refresh.unwrap_or(false)).await
}

/// Disable hibernation (async + progress push)
#[tauri::command]
pub async fn disable_hibernation(window: Window) -> Result<String, String> {
    crate::system_slim::disable_hibernation_with_progress(&window).await
}

/// Enable hibernation (async + progress push)
#[tauri::command]
pub async fn enable_hibernation(window: Window) -> Result<String, String> {
    crate::system_slim::enable_hibernation_with_progress(&window).await
}

/// Clean the WinSxS component store
#[tauri::command]
pub async fn cleanup_winsxs(window: Window) -> Result<String, String> {
    crate::system_slim::cleanup_winsxs(&window).await
}

/// Deep-clean the WinSxS component baseline
#[tauri::command]
pub async fn cleanup_winsxs_resetbase(window: Window) -> Result<String, String> {
    crate::system_slim::cleanup_winsxs_resetbase(&window).await
}

/// Open system virtual memory settings
#[tauri::command]
pub fn open_virtual_memory_settings() -> Result<(), String> {
    crate::system_slim::open_virtual_memory_settings()
}

/// Rebuild the Windows Search index
#[tauri::command]
pub async fn rebuild_search_index() -> Result<String, String> {
    // Put the COM call on a blocking thread: if the search service is abnormal, CoCreateInstance may hang,
    // and a synchronous command would tie up this IPC thread as well.
    tokio::task::spawn_blocking(crate::system_slim::rebuild_search_index)
        .await
        .map_err(|e| format!("任务执行失败: {}", e))?
}

// ============================================================================
// Health score
// ============================================================================

/// Calculate the system health score.
/// `drive_letter` is the partition selected in the top bar; when omitted, calculate for the system drive, preserving old caller behavior.
#[tauri::command]
pub fn get_health_score(drive_letter: Option<String>) -> HealthScoreResult {
    crate::health_score::calculate(drive_letter.as_deref())
}

// ============================================================================
// System info
// ============================================================================

/// Get system information
#[tauri::command]
pub async fn get_system_info() -> Result<SystemInfo, String> {
    info!("获取系统信息");
    crate::system_info::gather()
}
