// ============================================================================
// Cleanup log module - records every file deletion operation
// ============================================================================
//
// Features:
// 1. Serialize log data to JSON files with serde_json
// 2. Count files via std::fs::read_dir to implement log rotation (default: keep 10 files)
// 3. Write logs asynchronously without blocking the main thread
// 4. Cleanup logic continues even if log writing fails
//
// Log storage location: AppData/Roaming/LightC/logs/
// File naming format: cleanup_YYYYMMDD_HHMMSS.json
// ============================================================================

use chrono::Local;
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Default maximum number of log files to keep
const DEFAULT_MAX_LOG_FILES: usize = 10;
const MIN_LOG_FILES: usize = 1;
const MAX_LOG_FILES_LIMIT: usize = 100;

fn normalize_log_retention(max_log_files: Option<usize>) -> usize {
    // The log retention count comes from frontend local settings; the backend clamps the bounds again to prevent manual localStorage tampering from causing unlimited retention or clearing too many logs.
    max_log_files
        .unwrap_or(DEFAULT_MAX_LOG_FILES)
        .clamp(MIN_LOG_FILES, MAX_LOG_FILES_LIMIT)
}

// ============================================================================
// Log data structures
// ============================================================================

/// A single cleanup record
///
/// Uses serde's Serialize/Deserialize derives for JSON serialization:
/// - #[derive(Serialize, Deserialize)] auto-generates serialization code
/// - serde_json::to_string_pretty() converts the struct into a formatted JSON string
/// - serde_json::from_str() parses a JSON string into the struct
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupLogEntry {
    /// Operation timestamp (ISO 8601 format)
    pub timestamp: String,
    /// Cleanup module category (e.g. "WeChat cleanup", "registry leftovers", "big file cleanup")
    pub category: String,
    /// Absolute path of the file / registry key
    pub path: String,
    /// Space freed (bytes)
    pub size: u64,
    /// Operation result: "Success", "Failed", or "Locked_Pending_Reboot"
    pub result: String,
    /// Error message (if any)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

/// Complete log for a single cleanup session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupSession {
    /// Session start time
    pub session_start: String,
    /// Session end time
    pub session_end: String,
    /// Total number of files cleaned
    pub total_files: usize,
    /// Number of successful deletions
    pub success_count: usize,
    /// Number of failures
    pub failed_count: usize,
    /// Total space freed (bytes)
    pub total_freed_bytes: u64,
    /// Detailed cleanup records
    pub entries: Vec<CleanupLogEntry>,
}

impl CleanupSession {
    /// Create a new cleanup session
    pub fn new() -> Self {
        Self {
            session_start: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            session_end: String::new(),
            total_files: 0,
            success_count: 0,
            failed_count: 0,
            total_freed_bytes: 0,
            entries: Vec::new(),
        }
    }

    /// Add a cleanup record
    pub fn add_entry(&mut self, entry: CleanupLogEntry) {
        if entry.result == "Success" {
            self.success_count += 1;
            self.total_freed_bytes += entry.size;
        } else {
            self.failed_count += 1;
        }
        self.total_files += 1;
        self.entries.push(entry);
    }

    /// Finish the session
    pub fn finish(&mut self) {
        self.session_end = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    }
}

impl Default for CleanupSession {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Log manager
// ============================================================================

/// Cleanup log manager
///
/// Responsibilities:
/// 1. Write cleanup records to JSON files
/// 2. Manage the number of log files (rotation)
/// 3. Provide an async write interface
pub struct CleanupLogger {
    /// Log storage directory
    log_dir: PathBuf,
    /// Current session (uses Arc<Mutex> for async access)
    current_session: Arc<Mutex<Option<CleanupSession>>>,
}

impl CleanupLogger {
    /// Create the log manager
    ///
    /// # Arguments
    /// * `app_data_dir` - application data directory (AppData/Roaming/LightC)
    pub fn new(app_data_dir: &Path) -> Self {
        let log_dir = app_data_dir.join("logs");

        // Ensure the log directory exists
        if let Err(e) = fs::create_dir_all(&log_dir) {
            error!("创建日志目录失败: {:?}, 错误: {}", log_dir, e);
        }

        Self {
            log_dir,
            current_session: Arc::new(Mutex::new(None)),
        }
    }

    /// Get the log directory path
    pub fn get_log_dir(&self) -> &Path {
        &self.log_dir
    }

    /// Start a new cleanup session
    pub async fn start_session(&self) {
        let mut session = self.current_session.lock().await;
        *session = Some(CleanupSession::new());
        info!("开始新的清理会话");
    }

    /// Record a single cleanup operation
    ///
    /// # Arguments
    /// * `category` - cleanup module category
    /// * `path` - file path
    /// * `size` - file size
    /// * `success` - whether it succeeded
    /// * `error_msg` - error message (optional)
    pub async fn log_entry(
        &self,
        category: &str,
        path: &str,
        size: u64,
        success: bool,
        error_msg: Option<String>,
    ) {
        let entry = CleanupLogEntry {
            timestamp: Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
            category: category.to_string(),
            path: path.to_string(),
            size,
            result: if success {
                "Success".to_string()
            } else {
                "Failed".to_string()
            },
            error_message: error_msg,
        };

        let mut session = self.current_session.lock().await;
        if let Some(ref mut s) = *session {
            s.add_entry(entry);
        } else {
            // If there is no active session, create a temporary one
            let mut new_session = CleanupSession::new();
            new_session.add_entry(entry);
            *session = Some(new_session);
        }
    }

    /// Finish the current session and save the log
    ///
    /// Uses serde_json to serialize log data:
    /// 1. Call serde_json::to_string_pretty() to generate formatted JSON
    /// 2. Write to the file with std::fs::write()
    /// 3. Even if writing fails, the program is unaffected
    pub async fn finish_session(&self) -> Result<PathBuf, String> {
        let mut session_guard = self.current_session.lock().await;

        if let Some(ref mut session) = *session_guard {
            session.finish();

            // Generate the log file name
            let filename = format!("cleanup_{}.json", Local::now().format("%Y%m%d_%H%M%S"));
            let log_path = self.log_dir.join(&filename);

            // Serialize to a formatted JSON string with serde_json
            // to_string_pretty() produces readable, indented JSON
            match serde_json::to_string_pretty(session) {
                Ok(json_content) => {
                    // Write the file, handling errors with Result
                    match fs::write(&log_path, json_content) {
                        Ok(_) => {
                            info!("清理日志已保存: {:?}", log_path);

                            // Perform log rotation (in a background thread, non-blocking)
                            let log_dir = self.log_dir.clone();
                            tokio::spawn(async move {
                                if let Err(e) = rotate_logs(&log_dir, DEFAULT_MAX_LOG_FILES).await {
                                    warn!("日志轮转失败: {}", e);
                                }
                            });

                            *session_guard = None;
                            return Ok(log_path);
                        }
                        Err(e) => {
                            // Write failed: record the error but do not crash
                            error!("写入日志文件失败: {}", e);
                            *session_guard = None;
                            return Err(format!("写入日志文件失败: {}", e));
                        }
                    }
                }
                Err(e) => {
                    // Serialization failed: record the error but do not crash
                    error!("序列化日志失败: {}", e);
                    *session_guard = None;
                    return Err(format!("序列化日志失败: {}", e));
                }
            }
        }

        Err("没有活动的清理会话".to_string())
    }

    /// Save a batch of cleanup records directly (without session mode)
    ///
    /// Used to record multiple cleanup results at once
    pub async fn save_cleanup_results(
        &self,
        entries: Vec<CleanupLogEntry>,
        max_log_files: usize,
    ) -> Result<PathBuf, String> {
        if entries.is_empty() {
            return Err("没有清理记录".to_string());
        }

        let mut session = CleanupSession::new();
        for entry in entries {
            session.add_entry(entry);
        }
        session.finish();

        // Generate the log file name
        let filename = format!("cleanup_{}.json", Local::now().format("%Y%m%d_%H%M%S"));
        let log_path = self.log_dir.join(&filename);

        // Serialize and write
        let json_content =
            serde_json::to_string_pretty(&session).map_err(|e| format!("序列化失败: {}", e))?;

        fs::write(&log_path, json_content).map_err(|e| format!("写入失败: {}", e))?;

        info!("清理日志已保存: {:?}", log_path);

        // Perform log rotation
        let log_dir = self.log_dir.clone();
        tokio::spawn(async move {
            if let Err(e) = rotate_logs(&log_dir, max_log_files).await {
                warn!("日志轮转失败: {}", e);
            }
        });

        Ok(log_path)
    }
}

// ============================================================================
// Log rotation logic
// ============================================================================

/// Log rotation - keep only the most recent max_log_files logs
///
/// Implementation:
/// 1. Use std::fs::read_dir() to walk the log directory
/// 2. Filter .json files and collect file info
/// 3. Sort by creation time (oldest first)
/// 4. If the file count exceeds max_log_files, delete the oldest files
async fn rotate_logs(log_dir: &Path, max_log_files: usize) -> Result<(), String> {
    debug!("开始日志轮转检查，目录: {:?}", log_dir);
    let max_log_files = max_log_files.clamp(MIN_LOG_FILES, MAX_LOG_FILES_LIMIT);

    // Use read_dir to walk the directory and collect all JSON log files
    let entries: Vec<_> = match fs::read_dir(log_dir) {
        Ok(dir) => dir
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .map(|ext| ext == "json")
                    .unwrap_or(false)
            })
            .filter_map(|entry| {
                // Get the file's creation time (or modification time as fallback)
                let metadata = entry.metadata().ok()?;
                let created = metadata.created().or_else(|_| metadata.modified()).ok()?;
                Some((entry.path(), created))
            })
            .collect(),
        Err(e) => {
            return Err(format!("读取日志目录失败: {}", e));
        }
    };

    let file_count = entries.len();
    debug!("当前日志文件数量: {}", file_count);

    // If the file count does not exceed the limit, no rotation is needed
    if file_count <= max_log_files {
        return Ok(());
    }

    // Sort by creation time (oldest first)
    let mut sorted_entries = entries;
    sorted_entries.sort_by(|a, b| a.1.cmp(&b.1));

    // Compute how many files need to be deleted
    let files_to_delete = file_count - max_log_files;
    info!("日志轮转: 需要删除 {} 个旧文件", files_to_delete);

    // Delete the oldest files
    for (path, _) in sorted_entries.into_iter().take(files_to_delete) {
        match fs::remove_file(&path) {
            Ok(_) => {
                info!("已删除旧日志: {:?}", path);
            }
            Err(e) => {
                warn!("删除旧日志失败: {:?}, 错误: {}", path, e);
            }
        }
    }

    Ok(())
}

/// Run the log rotation check at application startup
pub async fn cleanup_old_logs(app_data_dir: &Path) {
    let log_dir = app_data_dir.join("logs");
    if log_dir.exists() {
        if let Err(e) = rotate_logs(&log_dir, DEFAULT_MAX_LOG_FILES).await {
            warn!("启动时日志轮转失败: {}", e);
        }
    }
}

// ============================================================================
// Log command functions (called by thin wrappers in commands.rs)
// ============================================================================

/// Cleanup log entry (format passed in from the frontend)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupLogEntryInput {
    pub category: String,
    pub path: String,
    pub size: u64,
    pub success: bool,
    pub error_message: Option<String>,
}

/// Cleanup history summary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupHistorySummary {
    pub filename: String,
    pub session_start: String,
    pub session_end: String,
    pub total_files: usize,
    pub success_count: usize,
    pub failed_count: usize,
    pub total_freed_bytes: u64,
}

/// Record a cleanup operation to the log file
pub async fn record_cleanup_action(
    app_data_dir: &Path,
    entries: Vec<CleanupLogEntryInput>,
    max_log_files: Option<usize>,
) -> Result<String, String> {
    use log::info;

    info!("记录清理操作，共 {} 条记录", entries.len());

    if entries.is_empty() {
        return Ok("没有需要记录的清理操作".to_string());
    }

    let logger = CleanupLogger::new(app_data_dir);
    let max_log_files = normalize_log_retention(max_log_files);

    let log_entries: Vec<CleanupLogEntry> = entries
        .into_iter()
        .map(|e| CleanupLogEntry {
            timestamp: Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
            category: e.category,
            path: e.path,
            size: e.size,
            result: if e.success {
                "Success".to_string()
            } else {
                "Failed".to_string()
            },
            error_message: e.error_message,
        })
        .collect();

    match logger
        .save_cleanup_results(log_entries, max_log_files)
        .await
    {
        Ok(path) => {
            info!("清理日志已保存: {:?}", path);
            Ok(format!("日志已保存: {}", path.display()))
        }
        Err(e) => {
            log::warn!("保存清理日志失败: {}", e);
            Err(e)
        }
    }
}

/// Open the log folder (explorer.exe)
pub fn open_logs_folder(app_data_dir: &Path) -> Result<(), String> {
    use log::info;

    info!("打开日志文件夹");

    let log_path = app_data_dir.join("logs");

    if !log_path.exists() {
        std::fs::create_dir_all(&log_path).map_err(|e| format!("创建日志目录失败: {}", e))?;
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(&log_path)
            .spawn()
            .map_err(|e| format!("打开文件夹失败: {}", e))?;
    }

    #[cfg(not(target_os = "windows"))]
    {
        return Err("此功能仅支持 Windows 系统".to_string());
    }

    Ok(())
}

/// Get the cleanup history list
pub fn get_cleanup_history(app_data_dir: &Path) -> Result<Vec<CleanupHistorySummary>, String> {
    use log::info;

    info!("获取清理历史记录");

    let log_path = app_data_dir.join("logs");

    if !log_path.exists() {
        return Ok(Vec::new());
    }

    let mut history: Vec<CleanupHistorySummary> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&log_path) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().map(|ext| ext == "json").unwrap_or(false) {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(session) = serde_json::from_str::<CleanupSession>(&content) {
                        history.push(CleanupHistorySummary {
                            filename: path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default(),
                            session_start: session.session_start,
                            session_end: session.session_end,
                            total_files: session.total_files,
                            success_count: session.success_count,
                            failed_count: session.failed_count,
                            total_freed_bytes: session.total_freed_bytes,
                        });
                    }
                }
            }
        }
    }

    history.sort_by(|a, b| b.session_start.cmp(&a.session_start));

    Ok(history)
}

// ============================================================================
// Global log manager instance
// ============================================================================

use once_cell::sync::OnceCell;

static CLEANUP_LOGGER: OnceCell<CleanupLogger> = OnceCell::new();

/// Initialize the global log manager
pub fn init_logger(app_data_dir: &Path) {
    let _ = CLEANUP_LOGGER.set(CleanupLogger::new(app_data_dir));
    info!("清理日志系统已初始化");
}

/// Get the global log manager
pub fn get_logger() -> Option<&'static CleanupLogger> {
    CLEANUP_LOGGER.get()
}
