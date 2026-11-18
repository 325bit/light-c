// ============================================================================
// Dev space cleanup commands
// ============================================================================

use crate::scanner::{
    delete_dev_cache_paths as run_delete, scan_dev_cache as run_scan, DevCacheDeleteResult,
    DevCacheScanResult,
};
use log::info;

/// Scans dev toolchain caches and multi-version leftovers.
///
/// Traverses a dozen or so directories (NuGet package caches can have tens of thousands of directories), so run in the blocking thread pool to avoid freezing the UI.
#[tauri::command]
pub async fn scan_dev_cache() -> Result<DevCacheScanResult, String> {
    info!("开始扫描开发空间");

    let result = tokio::task::spawn_blocking(run_scan)
        .await
        .map_err(|error| format!("扫描任务异常: {}", error))?;

    info!(
        "开发空间扫描完成：{} 个分组，{} 个版本组，跳过 {} 处",
        result.groups.len(),
        result.version_groups.len(),
        result.skipped
    );
    Ok(result)
}

/// Deletes the selected directories, returning the success count and failure details.
///
/// Paths from the frontend are not trusted: the backend re-validates ownership (must exactly equal a target directory,
/// or be a version-number directory under a version root); read-only items and the currently used version are always rejected.
///
/// The return value must include `failed`: files are often in use while IDE/build processes run, so deletion may partially fail,
/// reporting only the success count would make the UI show "all deleted" while actual usage has not dropped.
#[tauri::command]
pub async fn delete_dev_cache_paths(paths: Vec<String>) -> Result<DevCacheDeleteResult, String> {
    if paths.is_empty() {
        return Ok(DevCacheDeleteResult {
            deleted: 0,
            freed: 0,
            failed: Vec::new(),
        });
    }
    info!("准备删除 {} 个开发空间目录", paths.len());

    let result = tokio::task::spawn_blocking(move || run_delete(&paths))
        .await
        .map_err(|error| format!("删除任务异常: {}", error))?;

    info!(
        "开发空间删除完成：成功 {} 个，释放 {} 字节，失败 {} 个",
        result.deleted,
        result.freed,
        result.failed.len()
    );
    Ok(result)
}
