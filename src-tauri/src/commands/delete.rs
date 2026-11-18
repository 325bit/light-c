// ============================================================================
// File deletion command
// ============================================================================

use crate::cleaner::{
    DeleteEngine, EnhancedDeleteEngine, EnhancedDeleteProgress, EnhancedDeleteResult,
    PermanentDeleteEngine, PermanentDeleteResult, SafetyCheckResult,
};
use crate::scanner::{deep_junk, DeleteResult};
use log::info;
use serde::Deserialize;
use tauri::{AppHandle, Emitter};

/// Sends deletion progress to the frontend; a failure to emit shouldn't interrupt the actual deletion task.
fn emit_delete_progress(app: &AppHandle, progress: EnhancedDeleteProgress) {
    if let Err(error) = app.emit("junk-clean:delete-progress", progress) {
        log::warn!("发送垃圾清理删除进度失败: {}", error);
    }
}

/// Sends the deletion-preparation phase, helping the frontend give immediate feedback even while the backend expands deep categories.
fn emit_delete_preparing(app: &AppHandle, total_count: usize) {
    emit_delete_progress(
        app,
        EnhancedDeleteProgress {
            phase: "preparing".to_string(),
            processed_count: 0,
            total_count,
            success_count: 0,
            failed_count: 0,
            reboot_pending_count: 0,
            freed_physical_size: 0,
            elapsed_ms: 0,
        },
    );
}

/// Deletion request parameters
#[derive(Debug, Deserialize)]
pub struct DeleteRequest {
    pub paths: Vec<String>,
}

/// Deletes the specified files
#[tauri::command]
pub async fn delete_files(request: DeleteRequest) -> Result<DeleteResult, String> {
    info!("开始删除 {} 个文件", request.paths.len());

    let result = tokio::task::spawn_blocking(move || {
        let engine = DeleteEngine::new();
        engine.delete_paths(&request.paths)
    })
    .await
    .map_err(|e| format!("删除任务异常: {}", e))?;

    info!(
        "删除完成: 成功 {}, 失败 {}, 释放 {} 字节",
        result.success_count, result.failed_count, result.freed_size
    );

    Ok(result)
}

/// Enhanced file deletion
#[tauri::command]
pub async fn enhanced_delete_files(
    app: AppHandle,
    paths: Vec<String>,
) -> Result<EnhancedDeleteResult, String> {
    info!("增强删除: 开始删除 {} 个文件", paths.len());
    emit_delete_preparing(&app, paths.len());

    let progress_app = app.clone();
    let result = tokio::task::spawn_blocking(move || {
        let engine = EnhancedDeleteEngine::new();
        engine.delete_files_with_progress(&paths, |progress| {
            emit_delete_progress(&progress_app, progress);
        })
    })
    .await
    .map_err(|e| format!("删除任务失败: {}", e))?;

    info!(
        "增强删除完成: 成功 {}, 失败 {}, 待重启 {}, 释放 {} 字节",
        result.success_count,
        result.failed_count,
        result.reboot_pending_count,
        result.freed_physical_size
    );

    Ok(result)
}

/// Deletes deep-scan results; the backend re-validates path rules to prevent deletion of arbitrary files if the frontend is tampered with.
#[tauri::command]
pub async fn delete_deep_junk_files(
    app: AppHandle,
    mut paths: Vec<String>,
    scan_id: Option<String>,
    category_names: Option<Vec<String>>,
    excluded_paths: Option<Vec<String>>,
) -> Result<EnhancedDeleteResult, String> {
    let category_names = category_names.unwrap_or_default();
    let excluded_paths = excluded_paths.unwrap_or_default();
    let mut degraded_warning = None;
    if !category_names.is_empty() {
        // Deep-scan results are returned by page; on deletion the full category is restored from the backend session, avoiding the frontend sending only the first page of files.
        match deep_junk::get_paths_for_categories(
            scan_id.as_deref().unwrap_or_default(),
            &category_names,
            &excluded_paths,
        ) {
            Ok(extra_paths) => paths.extend(extra_paths),
            Err(error) => {
                // When the session has expired or is missing, downgrade to cleaning only the current page of files selected in the frontend,
                // and notify the user in the result, so a whole-category cleanup isn't completely blocked just because results expired.
                log::warn!("深度扫描会话失效，降级为清理当前页文件: {}", error);
                degraded_warning = Some(error);
            }
        }
    }

    let mut unique_paths = std::collections::HashSet::new();
    paths.retain(|path| unique_paths.insert(path.to_lowercase()));
    if paths.is_empty() {
        return Ok(EnhancedDeleteResult::new());
    }

    if let Some(invalid_path) = paths
        .iter()
        .find(|path| !deep_junk::is_deep_junk_path(path))
    {
        return Err(format!(
            "深度清理安全校验失败，拒绝删除路径: {}",
            invalid_path
        ));
    }

    info!("深度垃圾清理: 开始删除 {} 个文件", paths.len());
    emit_delete_preparing(&app, paths.len());
    let progress_app = app.clone();
    let mut result = tokio::task::spawn_blocking(move || {
        // Deep-scan paths have already passed is_deep_junk_path whitelist validation and are safe cache directories;
        // enable elevated deletion (icacls) to clean stubborn cache files that normal permissions can't delete,
        // applying only to directories covered by SAFE_OWNERSHIP_PATHS, not affecting other deletion entry points.
        let engine = EnhancedDeleteEngine::new().with_take_ownership(true);
        engine.delete_files_with_progress(&paths, |progress| {
            emit_delete_progress(&progress_app, progress);
        })
    })
    .await
    .map_err(|error| format!("深度垃圾删除任务失败: {}", error))?;

    result.warning = degraded_warning;

    info!(
        "深度垃圾清理完成: 成功 {}, 失败 {}, 待重启 {}, 释放 {} 字节",
        result.success_count,
        result.failed_count,
        result.reboot_pending_count,
        result.freed_physical_size
    );
    Ok(result)
}

/// Gets a file's physical size (aligned to clusters)
#[tauri::command]
pub async fn get_physical_size(logical_size: u64) -> Result<u64, String> {
    let engine = EnhancedDeleteEngine::new();
    Ok(engine.calculate_physical_size(logical_size))
}

/// Checks whether administrator privileges are needed
#[tauri::command]
pub async fn check_admin_for_path(path: String) -> Result<bool, String> {
    let path_lower = path.to_lowercase();

    let admin_required_paths = [
        "c:\\windows\\",
        "c:\\program files",
        "c:\\programdata\\microsoft\\windows",
    ];

    for admin_path in &admin_required_paths {
        if path_lower.starts_with(admin_path) {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Permanently deletes uninstall leftovers (deep cleanup)
#[tauri::command]
pub async fn delete_leftovers_permanent(
    paths: Vec<String>,
    // Whether to allow "directories containing executable files". Default false;
    // only true when the user explicitly clicked "Force Delete" in the result popup (whitelist and system-directory protection still apply).
    allow_executables: Option<bool>,
) -> Result<PermanentDeleteResult, String> {
    let allow_executables = allow_executables.unwrap_or(false);
    info!(
        "永久删除: 开始深度清理 {} 个卸载残留文件夹（放行可执行文件: {}）",
        paths.len(),
        allow_executables
    );

    let result = tokio::task::spawn_blocking(move || {
        let engine = PermanentDeleteEngine::new();
        engine.delete_leftovers(paths, allow_executables)
    })
    .await
    .map_err(|e| format!("永久删除任务失败: {}", e))?;

    info!(
        "永久删除完成: 成功 {}, 失败 {}, 待审核 {}, 待重启 {}, 释放 {} 字节",
        result.success_count,
        result.failed_count,
        result.manual_review_count,
        result.reboot_pending_count,
        result.freed_size
    );

    Ok(result)
}

/// Performs a safety check on a single path
#[tauri::command]
pub async fn check_leftover_safety(path: String) -> Result<SafetyCheckResult, String> {
    let result = tokio::task::spawn_blocking(move || {
        let engine = PermanentDeleteEngine::new();
        let path = std::path::Path::new(&path);
        engine.perform_safety_checks(path)
    })
    .await
    .map_err(|e| format!("安全检查失败: {}", e))?;

    Ok(result)
}
