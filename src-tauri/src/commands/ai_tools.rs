// ============================================================================
// AI tool cleanup commands
// ============================================================================

use crate::error::AppError;
use crate::scanner::{AiCacheDeleteResult, AiCacheFileList, AiToolScanResult, AiToolScanner};
use log::info;

/// Scans caches of various AI tools. Scanning traverses directories, so run it in the blocking thread pool to avoid freezing the UI.
#[tauri::command]
pub async fn scan_ai_tool_caches() -> Result<AiToolScanResult, AppError> {
    info!("开始扫描 AI 工具缓存");

    let result = tokio::task::spawn_blocking(|| {
        let scanner = AiToolScanner::new();
        scanner.scan()
    })
    .await
    .map_err(|e| format!("扫描任务异常: {}", e))?;

    info!(
        "AI 工具缓存扫描完成：{} 个工具，{} 个文件",
        result.groups.len(),
        result.total_files
    );
    Ok(result)
}

/// Deletes the selected cache directories, returning the success count and failure details.
///
/// Every path is re-validated for ownership in `AiToolScanner::delete_dirs` — paths from the frontend are not trusted,
/// and directory deletion has no recycle bin to recover from, so an extra check is worthwhile.
///
/// The return value must include a failure list: cache files are often in use while tools run, so returning only a success count would make the UI
/// show "deleted N items" while the actual usage has not dropped at all.
#[tauri::command]
pub async fn delete_ai_tool_caches(paths: Vec<String>) -> Result<AiCacheDeleteResult, AppError> {
    if paths.is_empty() {
        return Ok(AiCacheDeleteResult {
            deleted: 0,
            failed: Vec::new(),
        });
    }
    info!("准备删除 {} 个 AI 工具缓存目录", paths.len());

    let result = tokio::task::spawn_blocking(move || {
        let scanner = AiToolScanner::new();
        scanner.delete_dirs(&paths)
    })
    .await
    .map_err(|e| format!("删除任务异常: {}", e))?;

    info!(
        "AI 工具缓存删除完成：成功 {} 个，失败 {} 个",
        result.deleted,
        result.failed.len()
    );
    Ok(result)
}

/// Lists all files under a cache directory (for the detail modal).
///
/// Path ownership must also be validated here: although this endpoint is read-only, the frontend should not use it to traverse arbitrary directories.
#[tauri::command]
pub async fn list_ai_cache_files(path: String) -> Result<AiCacheFileList, AppError> {
    let target = std::path::PathBuf::from(&path);
    let listing = tokio::task::spawn_blocking(move || {
        let scanner = AiToolScanner::new();
        if !scanner.is_managed_cache_dir(&target) {
            return Err(format!("不是 AI 工具缓存目录: {}", target.display()));
        }
        Ok(scanner.list_files(&target))
    })
    .await
    .map_err(|e| format!("读取任务异常: {}", e))??;
    Ok(listing)
}
