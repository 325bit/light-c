// ============================================================================
// C drive full-disk change analysis command
//
// MFT enumeration and file size aggregation are long blocking tasks, so they run in spawn_blocking,
// avoiding occupying Tauri's async runtime threads and slowing down frontend IPC responses.
// ============================================================================

use log::info;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter};

#[tauri::command]
pub fn cancel_disk_growth_scan() {
    crate::scan_cancel::cancel(crate::scan_cancel::ScanKind::DiskGrowth);
}

#[tauri::command]
pub async fn get_disk_growth_file_details(
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
    // `diff` (default, by change magnitude) | `size` (current size) | `name`
    sort: Option<String>,
    // Omitted = the natural direction of `sort` (names A→Z, sizes largest first)
    desc: Option<bool>,
    drive_letter: Option<String>,
) -> Result<crate::disk_growth::DiskGrowthFileDetailsResponse, String> {
    tokio::task::spawn_blocking(move || {
        crate::disk_growth::get_file_change_details(path, offset, limit, sort, desc, drive_letter)
    })
    .await
    .map_err(|error| format!("读取文件级变化明细失败: {}", error))?
}

#[tauri::command]
pub async fn get_disk_growth_directory_details(
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
    drive_letter: Option<String>,
) -> Result<crate::disk_growth::DiskGrowthDirectoryDetailsResponse, String> {
    tokio::task::spawn_blocking(move || {
        crate::disk_growth::get_directory_change_details(path, offset, limit, drive_letter)
    })
    .await
    .map_err(|error| format!("读取目录级变化明细失败: {}", error))?
}

#[tauri::command]
pub async fn get_disk_growth_export_tree(
    paths: Vec<String>,
    max_depth: Option<usize>,
    drive_letter: Option<String>,
) -> Result<crate::disk_growth::DiskGrowthExportTreeResponse, String> {
    tokio::task::spawn_blocking(move || {
        crate::disk_growth::get_export_directory_tree(paths, max_depth, drive_letter)
    })
    .await
    .map_err(|error| format!("读取 HTML 导出目录明细失败: {}", error))?
}

/// Write the disk space change analysis HTML via the system save dialog.
/// The save operation runs on the Rust side, reusing the registered dialog plugin and handling Windows paths and extensions uniformly.
#[tauri::command]
pub async fn save_disk_growth_html(
    app: AppHandle,
    content: String,
    default_file_name: String,
    dialog_title: String,
) -> Result<Option<String>, String> {
    use std::fs;
    use tauri_plugin_dialog::DialogExt;

    if content.is_empty() {
        return Err("导出的 HTML 内容为空".to_string());
    }

    let selected_path = app
        .dialog()
        .file()
        .set_title(&dialog_title)
        .set_file_name(&default_file_name)
        .add_filter("HTML 文件", &["html"])
        .blocking_save_file();
    let Some(selected_path) = selected_path else {
        // A user cancelling the save is a normal flow and should not be treated as an export failure.
        return Ok(None);
    };

    let mut output_path = PathBuf::from(selected_path.to_string());
    let has_html_extension = output_path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("html"));
    if !has_html_extension {
        // The user may edit the file name, so the HTML extension is forced here.
        output_path.set_extension("html");
    }

    fs::write(&output_path, content).map_err(|error| {
        format!(
            "写入磁盘空间变化 HTML 失败（{}）：{}",
            output_path.display(),
            error
        )
    })?;

    Ok(Some(output_path.to_string_lossy().into_owned()))
}

#[tauri::command]
pub async fn scan_disk_growth(
    app_handle: AppHandle,
    max_change_entries: Option<usize>,
    drive_letter: Option<String>,
) -> Result<crate::disk_growth::DiskScanAndAnalyzeResponse, String> {
    let log_drive = drive_letter.as_deref().unwrap_or("系统盘").to_string();
    info!("开始执行 {} 全盘空间变化分析", log_drive);
    crate::scan_cancel::reset(crate::scan_cancel::ScanKind::DiskGrowth);

    let result = tokio::task::spawn_blocking(move || {
        crate::disk_growth::scan_and_analyze_drive_with_progress(
            &|progress| {
                // Scanning happens on a blocking thread; stage progress is sent back to the frontend via events to avoid a long "silent" IPC wait.
                let _ = app_handle.emit("disk-growth:progress", &progress);
            },
            max_change_entries,
            drive_letter,
        )
    })
    .await
    .map_err(|error| format!("全盘分析任务执行失败: {}", error))??;

    info!(
        "{} 全盘分析完成: {} 个目录变化，扫描 {} 个文件，耗时 {}ms",
        result.drive_letter,
        result.growth.entries.len(),
        result.total_files_scanned,
        result.scan_duration_ms
    );

    Ok(result)
}
