// ============================================================================
// Junk scan and large-file scan commands
// ============================================================================

use crate::error::{AppError, ErrorCode};
use crate::scanner::{
    big_files, deep_junk, CategoryScanResult, JunkCategory, ScanEngine, ScanResult,
};
use log::info;
use serde::{Deserialize, Serialize};
use tauri::Window;

/// Scan request parameters
#[derive(Debug, Deserialize)]
pub struct ScanRequest {
    pub categories: Option<Vec<String>>,
}

/// Category information (for frontend display)
#[derive(Debug, Serialize)]
pub struct CategoryInfo {
    pub name: String,
    pub description: String,
    pub risk_level: u8,
}

/// Performs a junk file scan
#[tauri::command]
pub async fn scan_junk_files(request: Option<ScanRequest>) -> Result<ScanResult, AppError> {
    info!("开始扫描垃圾文件");

    let result = tokio::task::spawn_blocking(move || {
        let engine = if let Some(req) = request {
            if let Some(category_names) = req.categories {
                // Compare against borrowed `&str` display names. Building a
                // `String` per category per comparison allocated for nothing:
                // `display_name` already hands out a `&'static str`.
                let names: Vec<&str> = category_names.iter().map(String::as_str).collect();
                let categories: Vec<JunkCategory> = JunkCategory::all()
                    .into_iter()
                    .filter(|c| names.contains(&c.display_name()))
                    .collect();

                if categories.is_empty() {
                    ScanEngine::new()
                } else {
                    ScanEngine::new().with_categories(categories)
                }
            } else {
                ScanEngine::new()
            }
        } else {
            ScanEngine::new()
        };

        engine.scan()
    })
    .await
    .map_err(|e| format!("扫描任务异常: {}", e))?;

    info!(
        "扫描完成: {} 个文件, {} 字节",
        result.total_file_count, result.total_size
    );

    Ok(result)
}

/// Performs a deep junk scan across all fixed partitions, preferring MFT for NTFS.
#[tauri::command]
pub async fn scan_deep_junk_files(
    window: Window,
    max_depth: Option<u32>,
) -> Result<deep_junk::DeepJunkScanResult, AppError> {
    info!("开始深度扫描垃圾文件");
    deep_junk::reset_cancelled();
    // The recursion depth is passed from the settings page. It must be written before spawn_blocking —
    // the scan thread reads module-level state, so a moment's delay leaves the previous round's depth.
    deep_junk::set_scan_depth(max_depth);

    let result = tokio::task::spawn_blocking(move || deep_junk::scan_all(&window))
        .await
        .map_err(|error| format!("深度扫描任务异常: {}", error))??;

    deep_junk::create_session(result).map_err(AppError::from)
}

/// Gets the paginated files of a deep-scan category.
#[tauri::command]
pub fn get_deep_junk_category_page(
    scan_id: String,
    category_name: String,
    offset: usize,
    limit: Option<usize>,
) -> Result<CategoryScanResult, AppError> {
    deep_junk::get_category_page(&scan_id, &category_name, offset, limit.unwrap_or(500))
        .map_err(AppError::from)
}

/// Cancels the running deep junk scan.
#[tauri::command]
pub fn cancel_deep_junk_scan() {
    info!("收到取消深度垃圾扫描请求");
    deep_junk::cancel();
}

/// Scans a single category
#[tauri::command]
pub async fn scan_category(category_name: String) -> Result<CategoryScanResult, AppError> {
    info!("扫描分类: {}", category_name);

    let result = tokio::task::spawn_blocking(move || -> Result<CategoryScanResult, String> {
        let category = JunkCategory::all()
            .into_iter()
            .find(|c| c.display_name() == category_name)
            .ok_or_else(|| format!("未知分类: {}", category_name))?;

        let engine = ScanEngine::new();
        Ok(engine.scan_category(&category))
    })
    .await
    .map_err(|e| format!("扫描任务异常: {}", e))??;

    Ok(result)
}

/// Gets all available cleanup categories
#[tauri::command]
pub fn get_categories() -> Vec<CategoryInfo> {
    JunkCategory::all()
        .into_iter()
        .map(|c| CategoryInfo {
            name: c.display_name().to_string(),
            description: c.description().to_string(),
            risk_level: c.risk_level(),
        })
        .collect()
}

/// Scans the system drive for large files, pushing progress in real time
#[tauri::command]
pub async fn scan_large_files(
    window: Window,
    top_n: Option<usize>,
    drive_letter: Option<String>,
) -> Result<Vec<big_files::LargeFileEntry>, AppError> {
    big_files::reset_cancelled();
    let window = window.clone();
    // The large-file list is rendered directly in the frontend, so the command layer clamps the count to keep abnormal config from making the UI and scan pressure spiral.
    let top_n = top_n.unwrap_or(50).clamp(10, 500);
    let drive_letter = normalize_large_file_drive_letter(drive_letter.as_deref())?;
    tokio::task::spawn_blocking(move || big_files::scan(&window, top_n, drive_letter))
        .await
        .map_err(|e| format!("扫描任务异常: {}", e))?
        .map_err(AppError::from)
}

/// Cancels the large-file scan
#[tauri::command]
pub fn cancel_large_file_scan() {
    big_files::cancel();
}

fn normalize_large_file_drive_letter(value: Option<&str>) -> Result<char, AppError> {
    // The frontend only passes a drive letter, but we still do a fallback check here to prevent paths or special characters when the command is called manually.
    let raw = value
        .and_then(|text| text.chars().find(|ch| ch.is_ascii_alphabetic()))
        .unwrap_or_else(|| {
            std::env::var("SYSTEMDRIVE")
                .ok()
                .and_then(|drive| drive.chars().find(|ch| ch.is_ascii_alphabetic()))
                .unwrap_or('C')
        })
        .to_ascii_uppercase();

    let root = format!("{}:\\", raw);
    if !std::path::Path::new(&root).is_dir() {
        return Err(AppError::new(ErrorCode::NotFound).arg("drive", root));
    }

    Ok(raw)
}
