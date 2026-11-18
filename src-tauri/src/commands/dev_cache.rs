// ============================================================================
// Dev space cleanup commands
// ============================================================================

use crate::error::AppError;
use crate::scanner::{
    delete_dev_cache_paths as run_delete, scan_dev_cache as run_scan,
    scan_other_app_caches as run_other_scan, scan_version_leftovers as run_version_scan,
    DevCacheDeleteResult, DevCacheScanResult, VersionLeftoverScanResult,
};
use log::info;

/// Scans dev toolchain caches (the "dev cache cleanup" page).
///
/// Traverses a dozen or so directories (NuGet package caches can have tens of thousands of directories), so run in the blocking thread pool to avoid freezing the UI.
///
/// The other two halves of the dev-space module are **not** included any more: multi-version leftovers (`scan_version_leftovers`)
/// and other software caches (`scan_other_app_caches`) moved to the "leftover & cache" page, so each page only pays for
/// the directories it displays.
#[tauri::command]
pub async fn scan_dev_cache() -> Result<DevCacheScanResult, AppError> {
    info!("开始扫描开发缓存");

    let result = tokio::task::spawn_blocking(run_scan)
        .await
        .map_err(|error| format!("扫描任务异常: {}", error))?;

    info!(
        "开发缓存扫描完成：{} 个分组，跳过 {} 处",
        result.groups.len(),
        result.skipped
    );
    Ok(result)
}

/// Scans the "other software caches" group only (rendered on the "leftover & cache" page).
///
/// These are the downloaded driver installers / launcher web caches that garbage cleanup and driver cleanup both
/// ignore. Same result shape as `scan_dev_cache` so the frontend reuses one type and one renderer.
#[tauri::command]
pub async fn scan_other_app_caches() -> Result<DevCacheScanResult, AppError> {
    info!("开始扫描其它软件缓存");

    let result = tokio::task::spawn_blocking(run_other_scan)
        .await
        .map_err(|error| format!("扫描任务异常: {}", error))?;

    info!(
        "其它软件缓存扫描完成：{} 个分组，共 {} 字节，跳过 {} 处",
        result.groups.len(),
        result.total_size,
        result.skipped
    );
    Ok(result)
}

/// Scans multi-version leftovers only (several version directories of the same software side by side).
///
/// Same blocking-pool treatment as the cache scan: each version directory is measured recursively and Edge alone can hold several GB.
#[tauri::command]
pub async fn scan_version_leftovers() -> Result<VersionLeftoverScanResult, AppError> {
    info!("开始扫描多版本残留");

    let result = tokio::task::spawn_blocking(run_version_scan)
        .await
        .map_err(|error| format!("扫描任务异常: {}", error))?;

    info!(
        "多版本残留扫描完成：{} 组，可回收 {} 字节，跳过 {} 处",
        result.groups.len(),
        result.reclaimable_size,
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
pub async fn delete_dev_cache_paths(paths: Vec<String>) -> Result<DevCacheDeleteResult, AppError> {
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
