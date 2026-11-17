// ============================================================================
// 开发空间清理命令
// ============================================================================

use crate::scanner::{
    delete_dev_cache_paths as run_delete, scan_dev_cache as run_scan, DevCacheDeleteResult,
    DevCacheScanResult,
};
use log::info;

/// 扫描开发工具链缓存与多版本残留。
///
/// 要遍历十几个目录（NuGet 包缓存动辄上万个目录），放阻塞线程池里跑，避免卡住 UI。
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

/// 删除选中的目录，返回成功数与失败明细。
///
/// 前端传来的路径不可信：后端会重新校验归属（必须精确等于某个目标目录，
/// 或位于版本根目录之下的版本号目录），只读项与当前使用的版本一律拒绝。
///
/// 返回值必须看 `failed`：IDE / 构建进程开着时文件常被占用，删除会部分失败，
/// 只报成功数会让界面显示"已删完"而实际占用没降。
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
