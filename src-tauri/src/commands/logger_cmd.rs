// ============================================================================
// Cleanup log commands
// ============================================================================

use crate::error::AppError;

pub use crate::logger::{CleanupHistorySummary, CleanupLogEntryInput};

/// Records cleanup operations to the log file
#[tauri::command]
pub async fn record_cleanup_action(
    entries: Vec<CleanupLogEntryInput>,
    max_log_files: Option<usize>,
) -> Result<String, AppError> {
    let app_data_dir = crate::data_dir::get_data_dir();
    crate::logger::record_cleanup_action(&app_data_dir, entries, max_log_files)
        .await
        .map_err(AppError::from)
}

/// Opens the log folder
#[tauri::command]
pub async fn open_logs_folder() -> Result<(), AppError> {
    let app_data_dir = crate::data_dir::get_data_dir();
    crate::logger::open_logs_folder(&app_data_dir).map_err(AppError::from)
}

/// Gets the cleanup history list
#[tauri::command]
pub async fn get_cleanup_history() -> Result<Vec<CleanupHistorySummary>, AppError> {
    let app_data_dir = crate::data_dir::get_data_dir();
    crate::logger::get_cleanup_history(&app_data_dir).map_err(AppError::from)
}
