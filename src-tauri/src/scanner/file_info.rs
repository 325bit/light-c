// ============================================================================
// File information structure definition
// Used to store detailed information about scanned files
// ============================================================================

use super::JunkCategory;
use crate::error::{AppError, ErrorCode};
use serde::{Deserialize, Serialize};

/// Detailed information of a single file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    /// Full path of the file
    pub path: String,
    /// File name
    pub name: String,
    /// File size (bytes)
    pub size: u64,
    /// Original path of the file before deletion (only recycle bin entries have a value)
    pub original_path: Option<String>,
    /// Last modified time (Unix timestamp)
    pub modified_time: i64,
    /// Whether it is a directory
    pub is_dir: bool,
    /// Category it belongs to
    pub category: JunkCategory,
}

impl FileInfo {
    /// Create new file information
    pub fn new(
        path: String,
        name: String,
        size: u64,
        modified_time: i64,
        is_dir: bool,
        category: JunkCategory,
    ) -> Self {
        FileInfo {
            path,
            name,
            size,
            original_path: None,
            modified_time,
            is_dir,
            category,
        }
    }

    /// Record the original path from recycle bin metadata, so the UI can hint to the user where the file came from.
    pub fn with_original_path(mut self, original_path: String) -> Self {
        self.original_path = Some(original_path);
        self
    }
}

/// Category scan result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryScanResult {
    /// Category
    pub category: JunkCategory,
    /// Category display name
    pub display_name: String,
    /// Category description
    pub description: String,
    /// Risk level
    pub risk_level: u8,
    /// Whether it is managed by the system. When true, the frontend does not offer direct deletion and only guides the user to use system disk cleanup —
    /// such directories (e.g. Windows.old) cannot be deleted inside the app, and are the system's fallback data.
    pub system_managed: bool,
    /// All files under this category
    pub files: Vec<FileInfo>,
    /// Total size (bytes)
    pub total_size: u64,
    /// File count
    pub file_count: usize,
    /// Deep scan pagination flag; always false for fast scans.
    pub has_more: bool,
}

impl CategoryScanResult {
    /// Create a new category scan result
    pub fn new(category: JunkCategory) -> Self {
        CategoryScanResult {
            display_name: category.display_name().to_string(),
            description: category.description().to_string(),
            risk_level: category.risk_level(),
            system_managed: category.is_system_managed(),
            category,
            files: Vec::new(),
            total_size: 0,
            file_count: 0,
            has_more: false,
        }
    }

    /// Add a file to the result
    pub fn add_file(&mut self, file: FileInfo) {
        self.total_size += file.size;
        self.file_count += 1;
        self.files.push(file);
    }

    /// Get a human-readable total size
    pub fn human_readable_total_size(&self) -> String {
        format_size(self.total_size)
    }

    /// Sort by file size descending: the largest space hogs go first, so users can prioritize them.
    ///
    /// Explanation:
    /// Files produced by cross-cutting sources (fast scan's parallel threads, deep scan's multi-partition merge, recycle bin scan) are ordered
    /// by traversal order and thread completion order, which is meaningless to users. Here we normalize to "size descending + path ascending":
    /// equal sizes are sorted by path, ensuring the same scan result is displayed in a consistent order every time, and that during deep scan pagination
    /// the first screen already shows the largest files, without an extra pass over all categories just for sorting.
    pub fn sort_files_by_size_desc(&mut self) {
        self.files.sort_by(|left, right| {
            right
                .size
                .cmp(&left.size)
                .then_with(|| left.path.cmp(&right.path))
        });
    }
}

/// Full scan result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanResult {
    /// Scan results for each category
    pub categories: Vec<CategoryScanResult>,
    /// Total size (bytes)
    pub total_size: u64,
    /// Total file count
    pub total_file_count: usize,
    /// Scan duration (milliseconds)
    pub scan_duration_ms: u64,
    /// Scan timestamp
    pub scan_timestamp: i64,
    /// Names of categories whose scan threads panicked and produced no result.
    /// Empty on a healthy scan; non-empty entries mean the visible result is partial.
    #[serde(default)]
    pub failed_categories: Vec<String>,
}

impl ScanResult {
    /// Create a new scan result
    pub fn new() -> Self {
        ScanResult {
            categories: Vec::new(),
            total_size: 0,
            total_file_count: 0,
            scan_duration_ms: 0,
            scan_timestamp: chrono::Utc::now().timestamp(),
            failed_categories: Vec::new(),
        }
    }

    /// Add a category result
    pub fn add_category_result(&mut self, result: CategoryScanResult) {
        self.total_size += result.total_size;
        self.total_file_count += result.file_count;
        self.categories.push(result);
    }

    /// Set scan duration
    pub fn set_duration(&mut self, duration_ms: u64) {
        self.scan_duration_ms = duration_ms;
    }

    /// Get a human-readable total size
    pub fn human_readable_total_size(&self) -> String {
        format_size(self.total_size)
    }
}

impl Default for ScanResult {
    fn default() -> Self {
        Self::new()
    }
}

/// Format file size into a human-readable format
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Deletion operation result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteResult {
    /// Number of files successfully deleted
    pub success_count: usize,
    /// Number of files that failed to delete
    pub failed_count: usize,
    /// Number of files marked for deletion on reboot
    pub reboot_pending_count: usize,
    /// Amount of space freed (bytes) — **only what was really freed is counted**, excluding the pending reboot portion
    pub freed_size: u64,
    /// Total size of files marked "delete on reboot" — these files have not freed disk space at this moment
    pub reboot_pending_size: u64,
    /// Whether a reboot is required to finish the cleanup
    pub needs_reboot: bool,
    /// List of failed files and reasons
    pub failed_files: Vec<DeleteError>,
}

impl DeleteResult {
    /// Create a new deletion result
    pub fn new() -> Self {
        DeleteResult {
            success_count: 0,
            failed_count: 0,
            reboot_pending_count: 0,
            freed_size: 0,
            reboot_pending_size: 0,
            needs_reboot: false,
            failed_files: Vec::new(),
        }
    }

    /// Record a successful deletion
    pub fn add_success(&mut self, size: u64) {
        self.success_count += 1;
        self.freed_size += size;
    }

    /// Record a deletion on reboot
    pub fn add_reboot_pending(&mut self, size: u64) {
        self.reboot_pending_count += 1;
        self.needs_reboot = true;
        // These files are only really deleted after a reboot, **the disk space is not freed at this moment**.
        // Previously their full sizes were also added to freed_size, causing "claimed several GB freed but actually only a bit over 1 GB".
        // Now they are accumulated separately; whether a reboot is needed is already expressed by needs_reboot / reboot_pending_count,
        // and the UI's "space will be freed after reboot" hint still works, only the "already freed" number is no longer inflated.
        self.reboot_pending_size += size;
    }

    /// Record a deletion failure.
    ///
    /// The typed error is split into a code (which the UI localizes) and an
    /// English diagnostic (which only reaches the log and the tooltip). Before
    /// S3a this took a bare string, so a refused path and an OS failure were
    /// indistinguishable once they reached the frontend.
    pub fn add_failure(&mut self, path: String, error: AppError) {
        self.failed_count += 1;
        self.failed_files.push(DeleteError {
            path,
            reason: error.message.clone(),
            failure_code: Some(error.code),
        });
    }
}

impl Default for DeleteResult {
    fn default() -> Self {
        Self::new()
    }
}

/// Deletion error information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteError {
    /// File path
    pub path: String,
    /// English diagnostic. The UI localizes from `failure_code` and only falls
    /// back to this text, so it must never be the primary user-facing copy.
    pub reason: String,
    /// Machine-readable category, so the UI never has to sniff `reason`.
    /// Absent from the wire when unknown, keeping old payloads compatible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<ErrorCode>,
}
