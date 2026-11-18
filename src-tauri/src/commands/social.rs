// ============================================================================
// Social software dedicated-cleanup command
// ============================================================================

use crate::error::AppError;
use crate::scanner::{SocialScanResult, SocialScanner};
use log::info;

/// Scans social software caches (with risk grading)
#[tauri::command]
pub async fn scan_social_cache() -> Result<SocialScanResult, AppError> {
    info!("开始扫描社交软件缓存（带风险分级）");

    let result = tokio::task::spawn_blocking(|| {
        let scanner = SocialScanner::new();
        scanner.scan()
    })
    .await
    .map_err(|e| format!("扫描任务异常: {}", e))?;

    info!(
        "社交软件扫描完成: {} 个文件, {} 字节, 可删除 {} 个文件 ({} 字节)",
        result.total_files, result.total_size, result.deletable_files, result.deletable_size
    );

    Ok(result)
}
