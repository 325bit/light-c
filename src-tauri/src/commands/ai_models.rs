use crate::ai_models::{
    is_model_package_directory, is_supported_model_extension,
    scan_ai_model_assets_with_progress as scan_ai_model_assets_impl, AiModelScanResult,
};
use crate::cleaner::{
    DeleteFailureReason, EnhancedDeleteEngine, EnhancedDeleteResult, FileDeleteResult,
};
use std::path::Path;
use tauri::{AppHandle, Emitter};
use walkdir::WalkDir;

#[tauri::command]
pub async fn scan_ai_model_assets(
    app_handle: AppHandle,
    enable_deep_discovery: Option<bool>,
) -> Result<AiModelScanResult, String> {
    let deep_discovery = enable_deep_discovery.unwrap_or(false);

    tokio::task::spawn_blocking(move || {
        scan_ai_model_assets_impl(deep_discovery, &|progress| {
            // AI model deep discovery may trigger the MFT fallback; phase events let the frontend keep giving explainable feedback during long IO.
            let _ = app_handle.emit("ai-models:progress", &progress);
        })
    })
    .await
    .map_err(|error| format!("AI 资产扫描任务异常：{}", error))
}

/// Delete a single AI model (a model file or a .mlpackage model package directory); the path is restricted to supported model formats.
#[tauri::command]
pub async fn delete_ai_model(path: String) -> Result<EnhancedDeleteResult, String> {
    tokio::task::spawn_blocking(move || {
        let model_path = Path::new(path.trim());

        // Core ML's .mlpackage is a directory package; scanning counts it as a whole package, so deletion must support whole packages too,
        // otherwise users could see the size but have no way to clean it.
        if model_path.is_dir() {
            if !is_model_package_directory(model_path) {
                return Err("当前目录不是可删除的 AI 模型包".to_string());
            }
            return delete_model_package(model_path);
        }

        if !model_path.is_file() {
            return Err("模型文件不存在，或当前路径不是普通文件".to_string());
        }
        if !is_supported_model_extension(model_path) {
            return Err("当前文件格式不在 AI 模型删除范围内".to_string());
        }

        // If the model file is locked, fail directly instead of scheduling deletion on reboot without explicit consent.
        let engine = EnhancedDeleteEngine::new().with_reboot_delete(false);
        let mut result = engine.delete_files(&[model_path.to_string_lossy().into_owned()]);
        result.generate_summary();
        Ok(result)
    })
    .await
    .map_err(|error| format!("AI 模型删除任务异常：{}", error))?
}

/// Delete a .mlpackage model package directory.
///
/// Measure the size before deletion (the path no longer exists afterwards); on failure give a locatable reason,
/// and keep the return structure identical to single-file deletion so the frontend need not distinguish the two model types.
fn delete_model_package(model_path: &Path) -> Result<EnhancedDeleteResult, String> {
    let logical_size = directory_logical_size(model_path);

    let mut result = EnhancedDeleteResult::new();
    match std::fs::remove_dir_all(model_path) {
        Ok(()) => {
            result.success_count = 1;
            result.freed_logical_size = logical_size;
            // Physical usage cannot be precisely recovered due to cluster alignment; use logical size as the freed amount to avoid overstating to users
            result.freed_physical_size = logical_size;
            result.file_results.push(FileDeleteResult {
                path: model_path.to_string_lossy().into_owned(),
                success: true,
                logical_size,
                physical_size: logical_size,
                failure_reason: None,
                marked_for_reboot: false,
            });
        }
        Err(error) => {
            let reason = error.to_string();
            result.failed_count = 1;
            result.skipped_size = logical_size;
            result.file_results.push(FileDeleteResult {
                path: model_path.to_string_lossy().into_owned(),
                success: false,
                logical_size,
                physical_size: logical_size,
                failure_reason: Some(DeleteFailureReason::Other(reason)),
                marked_for_reboot: false,
            });
        }
    }

    result.generate_summary();
    Ok(result)
}

/// Compute the logical size of a model package directory for display in the delete result.
fn directory_logical_size(path: &Path) -> u64 {
    WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum()
}
