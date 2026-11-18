// ============================================================================
// System tool commands (formatting, opening system settings, etc.)
// ============================================================================

use log::info;

/// Format a file size
#[tauri::command]
pub fn format_size(bytes: u64) -> String {
    crate::scanner::format_size(bytes)
}

/// Open the Windows Disk Cleanup tool
#[tauri::command]
pub fn open_disk_cleanup() -> Result<(), String> {
    info!("打开Windows磁盘清理工具");

    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        // Do not hardcode drive C: Windows can be installed on another drive. Use the same source as the junk cleanup category scan (SYSTEMDRIVE)
        // here, otherwise on non-C systems opening Disk Cleanup would point at C: and the user could not find the target items.
        // cleanmgr /d accepts only the drive letter without a colon, which is exactly what system_drive_letter() returns.
        let drive_letter = crate::system_info::system_drive_letter().to_string();
        Command::new("cleanmgr")
            .arg("/d")
            .arg(drive_letter)
            .spawn()
            .map_err(|e| format!("无法启动磁盘清理工具: {}", e))?;
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err("此功能仅支持Windows系统".to_string())
    }
}

/// Open a path in File Explorer
/// - If it is a directory, drill into it directly
/// - If it is a file, open its containing directory and select the file
#[tauri::command]
pub fn open_in_folder(path: String) -> Result<(), String> {
    info!("打开路径: {}", path);

    #[cfg(target_os = "windows")]
    {
        use std::path::Path;
        use std::process::Command;
        // Windows explorer needs a backslash path; forward slashes would open the desktop instead of the target directory
        let windows_path = path.replace('/', "\\");
        let p = Path::new(&windows_path);
        if p.is_dir() {
            // Directory: open and drill in directly
            Command::new("explorer")
                .arg(&windows_path)
                .spawn()
                .map_err(|e| format!("无法打开文件夹: {}", e))?;
        } else {
            // File: open the containing directory and select it
            Command::new("explorer")
                .arg("/select,")
                .arg(&windows_path)
                .spawn()
                .map_err(|e| format!("无法打开文件夹: {}", e))?;
        }
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err("此功能仅支持Windows系统".to_string())
    }
}

/// Open a file directly (using the system default program)
#[tauri::command]
pub fn open_file(app: tauri::AppHandle, path: String) -> Result<(), String> {
    info!("打开文件: {}", path);

    #[cfg(target_os = "windows")]
    {
        use tauri_plugin_opener::OpenerExt;
        // C3 fix: this used to build `format!("\"{}\"", path)` and hand it to `cmd /C start`,
        // where cmd.exe re-parses the whole command line — a path containing `"` / `&` / `^`
        // (all legal NTFS filename characters) allowed arbitrary command injection.
        // The opener plugin passes the path straight to ShellExecuteW; no shell is involved.
        // Forward slashes are still normalized first: ShellExecuteW needs backslash paths,
        // same as open_in_folder above.
        let windows_path = path.replace('/', "\\");
        app.opener()
            .open_path(windows_path, None::<&str>)
            .map_err(|e| format!("无法打开文件: {}", e))
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
        Err("此功能仅支持Windows系统".to_string())
    }
}

/// Open the Windows Recycle Bin view, not the physical `$Recycle.Bin` internal directory.
#[tauri::command]
pub fn open_recycle_bin() -> Result<(), String> {
    info!("打开系统回收站");

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg("shell:RecycleBinFolder")
            .spawn()
            .map_err(|error| format!("无法打开系统回收站: {}", error))?;
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err("此功能仅支持Windows系统".to_string())
    }
}

/// Open Task Manager's Startup apps page
#[tauri::command]
pub fn open_startup_manager() -> Result<(), String> {
    info!("打开启动项管理器");

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        use std::process::Command;
        Command::new("cmd")
            .args(["/c", "start", "taskmgr", "/0", "/startup"])
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|e| format!("无法打开启动项管理器: {}", e))?;
    }

    Ok(())
}

/// Open the Windows Storage Sense settings page
#[tauri::command]
pub fn open_storage_settings() -> Result<(), String> {
    info!("打开存储感知设置");

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        use std::process::Command;
        Command::new("cmd")
            .args(["/c", "start", "ms-settings:storagesense"])
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|e| format!("无法打开存储感知设置: {}", e))?;
    }

    Ok(())
}
