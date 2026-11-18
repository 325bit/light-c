// ============================================================================
// Enhanced delete engine - handles locked files and permission issues
//
// Core features:
// 1. Take Ownership - acquire file ownership to delete protected files
// 2. Delete on Reboot - use MOVEFILE_DELAY_UNTIL_REBOOT to handle locked files
// 3. Physical size calculation - return the disk space actually freed
//
// Safety mechanisms:
// - Strict whitelist path checking; Take Ownership is only performed in safe directories
// - System-critical files can never be deleted
// - Detailed deletion result feedback, including skip reasons
// ============================================================================

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use log::{debug, info, warn};
use serde::{Deserialize, Serialize};

use super::safety_constants::is_rebuildable_system_cache_path;

// ============================================================================
// Windows API bindings
// ============================================================================

#[cfg(windows)]
pub(crate) mod windows_api {
    use std::ptr;

    // MoveFileEx flags
    pub const MOVEFILE_DELAY_UNTIL_REBOOT: u32 = 0x00000004;
    pub const MOVEFILE_REPLACE_EXISTING: u32 = 0x00000001;

    // File attribute constants
    pub const FILE_ATTRIBUTE_READONLY: u32 = 0x00000001;
    pub const FILE_ATTRIBUTE_HIDDEN: u32 = 0x00000002;
    pub const FILE_ATTRIBUTE_SYSTEM: u32 = 0x00000004;

    #[link(name = "kernel32")]
    extern "system" {
        /// Mark a file for deletion on reboot
        ///
        /// # Safety note
        /// This function uses the MOVEFILE_DELAY_UNTIL_REBOOT flag to mark a file for deletion on reboot.
        /// This is the standard way Windows Storage Sense handles locked files.
        ///
        /// # Parameters
        /// - lpExistingFileName: path of the file to delete (wide characters)
        /// - lpNewFileName: target path; NULL means delete
        /// - dwFlags: operation flags, using MOVEFILE_DELAY_UNTIL_REBOOT
        pub fn MoveFileExW(
            lpExistingFileName: *const u16,
            lpNewFileName: *const u16,
            dwFlags: u32,
        ) -> i32;

        /// Get file attributes
        pub fn GetFileAttributesW(lpFileName: *const u16) -> u32;

        /// Set file attributes
        pub fn SetFileAttributesW(lpFileName: *const u16, dwFileAttributes: u32) -> i32;

        /// Get the last error code
        pub fn GetLastError() -> u32;

        /// Get disk free space (used to calculate cluster size)
        pub fn GetDiskFreeSpaceW(
            lpRootPathName: *const u16,
            lpSectorsPerCluster: *mut u32,
            lpBytesPerSector: *mut u32,
            lpNumberOfFreeClusters: *mut u32,
            lpTotalNumberOfClusters: *mut u32,
        ) -> i32;
    }

    /// Convert a Rust string to a Windows wide string
    pub fn to_wide_string(s: &str) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Mark a file for deletion on reboot
    ///
    /// # Note
    /// This function uses the Windows API MoveFileExW with the MOVEFILE_DELAY_UNTIL_REBOOT flag
    /// to mark locked files as "delete on reboot". This is the standard way Windows Storage Sense handles stubborn files.
    ///
    /// How it works:
    /// 1. Call MoveFileExW with the target path set to NULL
    /// 2. Windows records the operation in the registry key PendingFileRenameOperations
    /// 3. At the next system startup, the deletion is performed before the user logs in
    ///
    /// Safety considerations:
    /// - Only perform this operation on files that have passed safety checks
    /// - System-critical files must never use this feature
    /// - The user is shown a prompt that "a reboot is needed to complete cleanup"
    pub fn mark_for_delete_on_reboot(path: &str) -> Result<(), String> {
        let wide_path = to_wide_string(path);

        unsafe {
            let result = MoveFileExW(
                wide_path.as_ptr(),
                ptr::null(), // NULL means delete rather than move
                MOVEFILE_DELAY_UNTIL_REBOOT,
            );

            if result != 0 {
                Ok(())
            } else {
                let error_code = GetLastError();
                Err(format!("标记重启删除失败，错误代码: {}", error_code))
            }
        }
    }

    /// Remove a file's read-only, hidden, and system attributes
    pub fn remove_protection_attributes(path: &str) -> Result<(), String> {
        let wide_path = to_wide_string(path);

        unsafe {
            let attrs = GetFileAttributesW(wide_path.as_ptr());
            if attrs == u32::MAX {
                return Err("无法获取文件属性".to_string());
            }

            // Remove read-only, hidden, and system attributes
            let new_attrs =
                attrs & !(FILE_ATTRIBUTE_READONLY | FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM);

            if new_attrs != attrs {
                let result = SetFileAttributesW(wide_path.as_ptr(), new_attrs);
                if result == 0 {
                    return Err("无法修改文件属性".to_string());
                }
            }
        }

        Ok(())
    }

    /// Get the disk cluster size (used to calculate physical usage)
    pub fn get_cluster_size(root_path: &str) -> Option<u32> {
        let wide_path = to_wide_string(root_path);
        let mut sectors_per_cluster: u32 = 0;
        let mut bytes_per_sector: u32 = 0;
        let mut free_clusters: u32 = 0;
        let mut total_clusters: u32 = 0;

        unsafe {
            let result = GetDiskFreeSpaceW(
                wide_path.as_ptr(),
                &mut sectors_per_cluster,
                &mut bytes_per_sector,
                &mut free_clusters,
                &mut total_clusters,
            );

            if result != 0 {
                Some(sectors_per_cluster * bytes_per_sector)
            } else {
                None
            }
        }
    }
}

// ============================================================================
// Deletion result types
// ============================================================================

/// Classification of deletion failure reasons
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DeleteFailureReason {
    /// File does not exist
    NotFound,
    /// Insufficient permissions
    PermissionDenied,
    /// File is locked (in use)
    FileLocked,
    /// System-protected file
    SystemProtected,
    /// Path outside the allowed scope
    OutOfScope,
    /// Already marked for deletion on reboot
    MarkedForReboot,
    /// Other error
    Other(String),
}

impl DeleteFailureReason {
    /// Get a user-friendly Chinese description
    pub fn display_message(&self) -> &str {
        match self {
            Self::NotFound => "文件不存在",
            Self::PermissionDenied => "权限不足",
            Self::FileLocked => "文件被系统占用",
            Self::SystemProtected => "系统保护文件",
            Self::OutOfScope => "不在清理范围内",
            Self::MarkedForReboot => "已标记重启后删除",
            Self::Other(_) => "删除失败",
        }
    }

    /// Get detailed hint text (for tooltips)
    pub fn tooltip(&self) -> &str {
        match self {
            Self::NotFound => "该文件可能已被其他程序删除",
            Self::PermissionDenied => "需要管理员权限才能删除此文件",
            Self::FileLocked => "该文件正被系统或其他程序使用，将在重启后删除",
            Self::SystemProtected => "这是系统关键文件，删除可能导致系统不稳定",
            Self::OutOfScope => "该文件不在安全清理范围内",
            Self::MarkedForReboot => "文件已标记，将在下次重启时自动删除",
            Self::Other(msg) => msg.as_str(),
        }
    }
}

/// Deletion result for a single file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDeleteResult {
    /// File path
    pub path: String,
    /// Whether deletion succeeded
    pub success: bool,
    /// Logical size (the file's actual content size)
    pub logical_size: u64,
    /// Physical size (actual disk space occupied, cluster-aligned)
    pub physical_size: u64,
    /// Failure reason (if failed)
    pub failure_reason: Option<DeleteFailureReason>,
    /// Whether marked for deletion on reboot
    pub marked_for_reboot: bool,
}

/// Enhanced deletion result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhancedDeleteResult {
    /// Number of files successfully deleted
    pub success_count: usize,
    /// Number of files that failed
    pub failed_count: usize,
    /// Number of files marked for deletion on reboot
    pub reboot_pending_count: usize,
    /// Physical space actually freed (bytes)
    pub freed_physical_size: u64,
    /// Total logical size (for comparison)
    pub freed_logical_size: u64,
    /// Size of skipped files (could not be deleted due to errors)
    pub skipped_size: u64,
    /// Detailed per-file deletion results
    pub file_results: Vec<FileDeleteResult>,
    /// Whether a reboot is needed to complete cleanup
    pub needs_reboot: bool,
    /// Summary message (WeChat style)
    pub summary_message: String,
    /// Warning messages during cleanup (e.g. a downgrade caused by an expired deep-scan session); None for a normal deletion.
    pub warning: Option<String>,
}

/// Batch progress of the enhanced deletion process.
///
/// Progress only aggregates the current batch's statistics and carries no file details, to avoid frequently transferring large objects to the WebView during bulk deletion.
#[derive(Debug, Clone, Serialize)]
pub struct EnhancedDeleteProgress {
    /// Current phase; the delete engine currently uses cleaning.
    pub phase: String,
    /// Number of files processed.
    pub processed_count: usize,
    /// Total number of deduplicated files in this deletion.
    pub total_count: usize,
    /// Number of files successfully deleted.
    pub success_count: usize,
    /// Number of files that failed.
    pub failed_count: usize,
    /// Number of files marked for deletion on reboot.
    pub reboot_pending_count: usize,
    /// Physical space confirmed freed so far.
    pub freed_physical_size: u64,
    /// Time elapsed since the delete engine started.
    pub elapsed_ms: u64,
}

/// Maximum interval between progress events, so the UI keeps responding even when a single batch is slow.
const DELETE_PROGRESS_INTERVAL: Duration = Duration::from_millis(500);
/// Normal batch progress interval, avoiding the overhead of sending an IPC event for every file.
const DELETE_PROGRESS_BATCH_SIZE: usize = 500;

impl EnhancedDeleteResult {
    pub fn new() -> Self {
        Self {
            success_count: 0,
            failed_count: 0,
            reboot_pending_count: 0,
            freed_physical_size: 0,
            freed_logical_size: 0,
            skipped_size: 0,
            file_results: Vec::new(),
            needs_reboot: false,
            summary_message: String::new(),
            warning: None,
        }
    }

    /// Generate a WeChat-style summary message
    pub fn generate_summary(&mut self) {
        let freed_mb = self.freed_physical_size as f64 / 1024.0 / 1024.0;
        let skipped_mb = self.skipped_size as f64 / 1024.0 / 1024.0;

        let mut parts = Vec::new();

        // Successfully freed portion
        if self.freed_physical_size > 0 {
            if freed_mb >= 1024.0 {
                parts.push(format!("成功释放 {:.1} GB", freed_mb / 1024.0));
            } else if freed_mb >= 1.0 {
                parts.push(format!("成功释放 {:.1} MB", freed_mb));
            } else {
                parts.push(format!("成功释放 {:.0} KB", freed_mb * 1024.0));
            }
        }

        // The skipped portion cannot be uniformly described as "in use by the system"; Shell API failures in the recycle bin and permission issues also land here.
        if self.skipped_size > 0 {
            if skipped_mb >= 1.0 {
                parts.push(format!("{:.1} MB 跳过", skipped_mb));
            } else {
                parts.push(format!("{:.0} KB 跳过", skipped_mb * 1024.0));
            }
        }

        // Portion pending deletion on reboot
        if self.reboot_pending_count > 0 {
            parts.push(format!(
                "{} 个文件将在重启后删除",
                self.reboot_pending_count
            ));
        }

        self.summary_message = if parts.is_empty() {
            "没有文件被清理".to_string()
        } else {
            parts.join("，")
        };
    }
}

impl Default for EnhancedDeleteResult {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Enhanced delete engine
// ============================================================================

/// Safe directories where Take Ownership is allowed
///
/// # Note
/// The "take ownership" operation is only allowed for files under these directories.
/// This prevents accidental deletion of system files.
const SAFE_OWNERSHIP_PATHS: &[&str] = &[
    "\\temp",
    "\\tmp",
    "\\cache",
    "\\caches",
    "\\appdata\\local\\temp",
    "\\appdata\\local\\microsoft\\windows\\temporary internet files",
    "\\appdata\\local\\microsoft\\windows\\inetcache",
    "\\appdata\\local\\microsoft\\windows\\caches",
    "\\appdata\\local\\microsoft\\windows\\explorer",
    "\\appdata\\local\\microsoft\\windows\\webcache",
    "\\appdata\\local\\microsoft\\windows\\clipboard",
    // Service Worker / media cache directories of Chromium-based browsers, at the same level as Cache and equally rebuildable.
    "\\service worker",
    "\\media cache",
    "\\windows\\temp",
    // Windows 10/11 system-level temp directories and kernel crash report directories, containing rebuildable diagnostic data.
    "\\windows\\systemtemp",
    "\\windows\\livekernelreports",
    "\\windows\\prefetch",
    "\\windows\\softwaredistribution\\download",
    "\\windows\\softwaredistribution\\deliveryoptimization",
    "\\windows\\serviceprofiles\\networkservice\\appdata\\local\\microsoft\\windows\\deliveryoptimization",
    "\\windows\\serviceprofiles\\localservice\\appdata\\local\\fontcache",
    "\\programdata\\microsoft\\windows defender\\localcopy",
    "\\programdata\\microsoft\\windows defender\\support",
    "\\programdata\\microsoft\\windows defender\\scans\\history\\service",
    // Windows error report queues/archives: jointly held by SYSTEM and TrustedInstaller, so elevated deletion must be allowed.
    "\\programdata\\microsoft\\windows\\wer",
    "\\windows\\system32\\d3d_cache",
    "\\appdata\\local\\d3dscache",
    "\\appdata\\local\\amd\\dxcache",
    "\\appdata\\local\\nvidia\\dxcache",
    "\\appdata\\local\\nvidia\\glcache",
    "\\appdata\\local\\nvidia\\dxc",
    "\\appdata\\local\\nvidia\\computecache",
    "\\appdata\\local\\intel\\shadercache",
    "\\$recycle.bin",
];

// Use the built-in Administrators group SID to avoid icacls failing to resolve localized group names on Chinese Windows.
const LOCAL_ADMINISTRATORS_SID: &str = "*S-1-5-32-544";

/// Enhanced delete engine
pub struct EnhancedDeleteEngine {
    /// Disk cluster size cache
    cluster_size: u32,
    /// Cache cluster size per volume when cleaning multiple partitions, avoiding a Windows API call per file.
    cluster_sizes: Mutex<HashMap<String, u32>>,
    /// Whether deletion on reboot is enabled
    enable_reboot_delete: bool,
    /// Whether to attempt taking ownership
    enable_take_ownership: bool,
}

impl EnhancedDeleteEngine {
    /// Create a new enhanced delete engine
    pub fn new() -> Self {
        // Get the C drive cluster size, defaulting to 4096 bytes
        let cluster_size = windows_api::get_cluster_size("C:\\").unwrap_or(4096);

        Self {
            cluster_size,
            cluster_sizes: Mutex::new(HashMap::new()),
            enable_reboot_delete: true, // enabled by default, handles occupied files
            enable_take_ownership: false, // disabled by default, icacls calls are slow
        }
    }

    /// Set whether deletion on reboot is enabled
    pub fn with_reboot_delete(mut self, enabled: bool) -> Self {
        self.enable_reboot_delete = enabled;
        self
    }

    /// Set whether taking ownership is enabled
    pub fn with_take_ownership(mut self, enabled: bool) -> Self {
        self.enable_take_ownership = enabled;
        self
    }

    /// Calculate a file's physical usage (cluster-aligned)
    ///
    /// # Note
    /// A file's actual space on disk is allocated in clusters.
    /// For example, a 1-byte file actually occupies 4KB on a disk with a 4KB cluster size.
    /// This function returns the disk space the file actually frees, not its content size.
    pub fn calculate_physical_size(&self, logical_size: u64) -> u64 {
        if logical_size == 0 {
            return 0;
        }
        let cluster_size = self.cluster_size as u64;
        ((logical_size + cluster_size - 1) / cluster_size) * cluster_size
    }

    /// Calculate physical usage by the file's partition, avoiding applying the C drive cluster size when deeply cleaning D/E drives.
    fn calculate_physical_size_for_path(&self, path: &Path, logical_size: u64) -> u64 {
        let Some(drive_root) = drive_root(path) else {
            return self.calculate_physical_size(logical_size);
        };
        let cluster_size = self.cluster_size_for_drive(&drive_root);
        align_physical_size(logical_size, cluster_size)
    }

    fn cluster_size_for_drive(&self, drive_root: &str) -> u32 {
        if let Ok(cache) = self.cluster_sizes.lock() {
            if let Some(cluster_size) = cache.get(drive_root) {
                return *cluster_size;
            }
        }

        let cluster_size = windows_api::get_cluster_size(drive_root).unwrap_or(self.cluster_size);
        if let Ok(mut cache) = self.cluster_sizes.lock() {
            cache.insert(drive_root.to_string(), cluster_size);
        }
        cluster_size
    }

    /// Delete a list of files
    pub fn delete_files(&self, paths: &[String]) -> EnhancedDeleteResult {
        // Keep the original public interface so other modules can reuse the delete engine without handling progress events.
        self.delete_files_with_progress(paths, |_| {})
    }

    /// Delete a list of files and report progress in batches.
    ///
    /// The callback runs on the delete thread; the caller may only do lightweight notifications and must not perform file IO there.
    pub fn delete_files_with_progress<F>(
        &self,
        paths: &[String],
        mut on_progress: F,
    ) -> EnhancedDeleteResult
    where
        F: FnMut(EnhancedDeleteProgress),
    {
        let mut result = EnhancedDeleteResult::new();
        let total_count = paths.len();
        let started_at = Instant::now();
        let mut processed_count = 0usize;
        let mut last_progress_at = Instant::now();

        // Push the cleaning phase once when the delete task first enters the execution thread, so small tasks do not appear to have no progress.
        on_progress(EnhancedDeleteProgress {
            phase: "cleaning".to_string(),
            processed_count: 0,
            total_count,
            success_count: 0,
            failed_count: 0,
            reboot_pending_count: 0,
            freed_physical_size: 0,
            elapsed_ms: 0,
        });

        // Progress events carry only aggregated data, avoiding slowing down the actual cleanup during bulk deletion.
        let mut emit_progress = |processed: usize, current_result: &EnhancedDeleteResult| {
            let should_emit = processed == total_count
                || processed.saturating_sub(1) % DELETE_PROGRESS_BATCH_SIZE == 0
                || last_progress_at.elapsed() >= DELETE_PROGRESS_INTERVAL;
            if !should_emit {
                return;
            }

            on_progress(EnhancedDeleteProgress {
                phase: "cleaning".to_string(),
                processed_count: processed,
                total_count,
                success_count: current_result.success_count,
                failed_count: current_result.failed_count,
                reboot_pending_count: current_result.reboot_pending_count,
                freed_physical_size: current_result.freed_physical_size,
                elapsed_ms: started_at.elapsed().as_millis() as u64,
            });
            last_progress_at = Instant::now();
        };

        info!("增强删除引擎：开始删除 {} 个文件", paths.len());

        // Separate recycle bin paths: they are deleted entry by entry, not via SHEmptyRecycleBinW.
        // H1 fix: SHEmptyRecycleBinW empties an entire volume — selecting 3 entries wiped every
        // other item on that drive, silently and unrecoverably. The scanner only surfaces entries
        // under the current user's own SID folder, where the owning user holds Full Control, so
        // removing the $R/$I pair directly needs no SYSTEM privileges.
        // C1 fix: normalize first, so `C:/...` or `\\?\...` recycle entries are still classified correctly.
        let (recycle_paths, normal_paths): (Vec<&String>, Vec<&String>) =
            paths.iter().partition(|p| {
                crate::path_guard::normalize(Path::new(p))
                    .to_lowercase()
                    .contains("\\$recycle.bin")
            });

        // Recycle entries are deleted one by one so a bad volume or a foreign-SID entry cannot
        // drag the others down.
        if !recycle_paths.is_empty() {
            info!(
                "检测到 {} 个回收站条目，按条目删除 $R/$I 对",
                recycle_paths.len()
            );

            // Sizes must be read before deletion, because afterwards the paths no longer exist and would report 0 bytes.
            let mut recycle_by_drive: BTreeMap<String, Vec<(String, u64, u64)>> = BTreeMap::new();
            for path in &recycle_paths {
                let logical_size = self.get_file_size(Path::new(path));
                let Some(drive_root) = recycle_drive_root(path) else {
                    let physical_size = self.calculate_physical_size(logical_size);
                    result.failed_count += 1;
                    result.skipped_size += physical_size;
                    result.file_results.push(FileDeleteResult {
                        path: (*path).clone(),
                        success: false,
                        logical_size,
                        physical_size,
                        failure_reason: Some(DeleteFailureReason::Other(
                            "回收站路径缺少有效盘符".to_string(),
                        )),
                        marked_for_reboot: false,
                    });
                    // Invalid recycle bin paths also count as processed, so the progress total still converges to 100% on bad input.
                    processed_count += 1;
                    emit_progress(processed_count, &result);
                    continue;
                };
                // The recycle bin supports multiple drives, so physical size must use the cluster size of the volume holding the entry, not a fixed C drive value.
                let physical_size =
                    align_physical_size(logical_size, self.cluster_size_for_drive(&drive_root));
                recycle_by_drive.entry(drive_root).or_default().push((
                    (*path).clone(),
                    logical_size,
                    physical_size,
                ));
            }

            for (_drive_root, entries) in recycle_by_drive {
                for (path, logical_size, physical_size) in entries {
                    match delete_recycle_entry(&path) {
                        Ok(()) => {
                            result.success_count += 1;
                            result.freed_logical_size += logical_size;
                            result.freed_physical_size += physical_size;
                            result.file_results.push(FileDeleteResult {
                                path,
                                success: true,
                                logical_size,
                                physical_size,
                                failure_reason: None,
                                marked_for_reboot: false,
                            });
                        }
                        Err(error) => {
                            warn!("回收站条目删除失败: {}", error);
                            result.failed_count += 1;
                            result.skipped_size += physical_size;
                            result.file_results.push(FileDeleteResult {
                                path,
                                success: false,
                                logical_size,
                                physical_size,
                                failure_reason: Some(DeleteFailureReason::Other(error)),
                                marked_for_reboot: false,
                            });
                        }
                    }
                    processed_count += 1;
                }
                emit_progress(processed_count, &result);
            }
        }

        // Delete normal files one by one
        for path in normal_paths {
            let file_result = self.delete_single_file(path);

            match &file_result.failure_reason {
                None => {
                    result.success_count += 1;
                    result.freed_logical_size += file_result.logical_size;
                    result.freed_physical_size += file_result.physical_size;
                }
                Some(DeleteFailureReason::MarkedForReboot) => {
                    result.reboot_pending_count += 1;
                    result.needs_reboot = true;
                }
                Some(_) => {
                    result.failed_count += 1;
                    result.skipped_size += file_result.physical_size;
                }
            }

            result.file_results.push(file_result);
            processed_count += 1;
            emit_progress(processed_count, &result);
        }

        result.generate_summary();

        info!(
            "增强删除完成: 成功 {}, 失败 {}, 待重启 {}, 释放 {} 字节",
            result.success_count,
            result.failed_count,
            result.reboot_pending_count,
            result.freed_physical_size
        );

        result
    }

    /// Delete a single file
    fn delete_single_file(&self, path: &str) -> FileDeleteResult {
        let file_path = Path::new(path);

        // Get the file size
        let logical_size = self.get_file_size(file_path);
        let physical_size = self.calculate_physical_size_for_path(file_path, logical_size);

        // C1 fix, gate 0: reject `..` / `.` traversal before any deletion happens.
        // The prefix checks below match the literal string, the OS resolves `..`.
        if crate::path_guard::validate_deletable(file_path).is_err() {
            return FileDeleteResult {
                path: path.to_string(),
                success: false,
                logical_size,
                physical_size,
                failure_reason: Some(DeleteFailureReason::OutOfScope),
                marked_for_reboot: false,
            };
        }

        // Check whether the file exists
        if !file_path.exists() {
            return FileDeleteResult {
                path: path.to_string(),
                success: false,
                logical_size,
                physical_size,
                failure_reason: Some(DeleteFailureReason::NotFound),
                marked_for_reboot: false,
            };
        }

        // Safety check
        if self.is_system_protected(file_path) {
            return FileDeleteResult {
                path: path.to_string(),
                success: false,
                logical_size,
                physical_size,
                failure_reason: Some(DeleteFailureReason::SystemProtected),
                marked_for_reboot: false,
            };
        }

        // Try to delete
        match self.try_delete(file_path) {
            Ok(_) => {
                debug!("成功删除: {}", path);
                FileDeleteResult {
                    path: path.to_string(),
                    success: true,
                    logical_size,
                    physical_size,
                    failure_reason: None,
                    marked_for_reboot: false,
                }
            }
            Err(e) => {
                // Treat only Windows sharing violations as "locked", so permission errors are not wrongly queued for reboot.
                let is_locked = matches!(e.raw_os_error, Some(32 | 33));

                if is_locked && self.enable_reboot_delete {
                    // Try to mark the file for deletion on reboot
                    if self.is_safe_for_ownership(file_path) {
                        match windows_api::mark_for_delete_on_reboot(path) {
                            Ok(_) => {
                                info!("文件已标记为重启删除: {}", path);
                                return FileDeleteResult {
                                    path: path.to_string(),
                                    success: false,
                                    logical_size,
                                    physical_size,
                                    failure_reason: Some(DeleteFailureReason::MarkedForReboot),
                                    marked_for_reboot: true,
                                };
                            }
                            Err(mark_err) => {
                                warn!("标记重启删除失败: {} - {}", path, mark_err);
                            }
                        }
                    }

                    FileDeleteResult {
                        path: path.to_string(),
                        success: false,
                        logical_size,
                        physical_size,
                        failure_reason: Some(DeleteFailureReason::FileLocked),
                        marked_for_reboot: false,
                    }
                } else if e.raw_os_error == Some(5) || e.message.contains("权限") {
                    FileDeleteResult {
                        path: path.to_string(),
                        success: false,
                        logical_size,
                        physical_size,
                        // Keep the specific results of icacls and attribute handling so the UI does not show only a generic insufficient-permissions message.
                        failure_reason: Some(DeleteFailureReason::Other(format!(
                            "权限不足：{}",
                            e.message
                        ))),
                        marked_for_reboot: false,
                    }
                } else {
                    FileDeleteResult {
                        path: path.to_string(),
                        success: false,
                        logical_size,
                        physical_size,
                        failure_reason: Some(DeleteFailureReason::Other(e.message)),
                        marked_for_reboot: false,
                    }
                }
            }
        }
    }

    /// Try to delete a file (multiple strategies)
    fn try_delete(&self, path: &Path) -> Result<(), DeleteAttemptError> {
        // Keep the original error code from the first deletion so later strategy failures can still be correctly identified as sharing violations.
        let first_error = match self.direct_delete(path) {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };

        let mut attempt_messages = vec![format!("直接删除失败: {}", first_error)];

        // Strategy 2: delete after removing protection attributes, keeping the failure reason so the UI can locate the problem.
        if let Err(error) = self.delete_after_remove_attrs(path) {
            attempt_messages.push(error);
        } else {
            return Ok(());
        }

        // Cache directories explicitly marked by the Defender cleanup wizard allow single-file ownership takeover by default; other directories remain under the global toggle.
        let path_string = path.to_string_lossy();
        let is_cache_path = is_rebuildable_system_cache_path(&path_string);
        if self.is_safe_for_ownership(path) && (self.enable_take_ownership || is_cache_path) {
            if let Err(error) = self.delete_with_ownership(path) {
                attempt_messages.push(error);
            } else {
                return Ok(());
            }
        }

        // All strategies failed; aggregate each step's result so the UI does not show only a generic "deletion failed".
        Err(DeleteAttemptError {
            message: format!("删除失败: {}", attempt_messages.join("；")),
            raw_os_error: first_error.raw_os_error(),
        })
    }

    /// Delete directly
    fn direct_delete(&self, path: &Path) -> io::Result<()> {
        if path.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        }
    }

    /// Delete after removing protection attributes
    fn delete_after_remove_attrs(&self, path: &Path) -> Result<(), String> {
        let path_str = path.to_string_lossy();

        // Remove read-only and similar attributes
        windows_api::remove_protection_attributes(&path_str)?;

        // Try deleting again
        self.direct_delete(path)
            .map_err(|e| format!("移除属性后仍无法删除: {}", e))
    }

    /// Delete after taking ownership
    ///
    /// # Note
    /// Use the icacls command to take ownership of the file, then delete it.
    /// Note: for performance, the /T recursive flag is not used; only a single file is handled.
    ///
    /// Safety considerations:
    /// - Only run in directories listed in SAFE_OWNERSHIP_PATHS
    /// - Never perform this operation on system-critical files
    fn delete_with_ownership(&self, path: &Path) -> Result<(), String> {
        let path_str = path.to_string_lossy();

        debug!("尝试获取所有权: {}", path_str);

        // Take ownership using icacls (no /T recursion, for better performance)
        let output = Command::new("icacls")
            .arg(&*path_str)
            .arg("/setowner")
            .arg(LOCAL_ADMINISTRATORS_SID)
            .arg("/C") // continue on errors
            .arg("/Q") // quiet mode
            .creation_flags(0x08000000) // CREATE_NO_WINDOW - do not show a console window
            .output()
            .map_err(|e| format!("执行 icacls 失败: {}", e))?;

        if !output.status.success() {
            return Err(format!(
                "获取所有权失败（退出码 {:?}）",
                output.status.code()
            ));
        }

        // Grant full control
        let output = Command::new("icacls")
            .arg(&*path_str)
            .arg("/grant")
            .arg(format!("{}:F", LOCAL_ADMINISTRATORS_SID))
            .arg("/C")
            .arg("/Q")
            .creation_flags(0x08000000)
            .output()
            .map_err(|e| format!("执行 icacls 授权失败: {}", e))?;

        if !output.status.success() {
            return Err(format!("授权失败（退出码 {:?}）", output.status.code()));
        }

        // Try deleting again
        self.direct_delete(path)
            .map_err(|e| format!("获取所有权后仍无法删除: {}", e))
    }

    /// Check whether a path is safe for Take Ownership
    fn is_safe_for_ownership(&self, path: &Path) -> bool {
        let path_string = path.to_string_lossy();
        let normalized = path_string.replace('/', "\\").to_ascii_lowercase();

        SAFE_OWNERSHIP_PATHS.iter().any(|safe_path| {
            let Some(start) = normalized.find(safe_path) else {
                return false;
            };
            let suffix = &normalized[start + safe_path.len()..];
            // Directory whitelist must be matched by path segments, to avoid accidentally allowing similarly named directories.
            suffix.is_empty() || suffix.starts_with('\\')
        })
    }

    /// Check whether a file is system-protected (uses shared safety constants, consistent with delete_engine)
    fn is_system_protected(&self, path: &Path) -> bool {
        use super::safety_constants::{
            is_rebuildable_system_cache_path, PROTECTED_FILES, PROTECTED_PATH_PREFIXES,
        };

        // C1 fix: normalize separators and strip the `\\?\` prefix before matching,
        // consistent with delete_engine::is_protected_path.
        let path_str = crate::path_guard::normalize(path).to_lowercase();

        for prefix in PROTECTED_PATH_PREFIXES {
            if path_str.starts_with(prefix) && !is_rebuildable_system_cache_path(&path_str) {
                return true;
            }
        }

        if let Some(file_name) = path.file_name() {
            let name = file_name.to_string_lossy().to_lowercase();
            for protected in PROTECTED_FILES {
                if name == *protected {
                    return true;
                }
            }
        }

        false
    }

    /// Get the file size
    fn get_file_size(&self, path: &Path) -> u64 {
        // A recycle bin entry's displayed size comes from $I metadata; a directory must not be counted by only its direct $R children.
        if let Some(logical_size) = get_recycle_metadata_size(path) {
            return logical_size;
        }

        if path.is_file() {
            fs::metadata(path).map(|m| m.len()).unwrap_or(0)
        } else if path.is_dir() {
            self.calculate_dir_size(path)
        } else {
            0
        }
    }

    /// Calculate directory size (simplified, returns an estimate for performance)
    fn calculate_dir_size(&self, path: &Path) -> u64 {
        // For performance, only direct children are counted, without recursive traversal
        // The actually freed space is reported by the system after deletion
        fs::read_dir(path)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter_map(|e| e.metadata().ok())
                    .map(|m| m.len())
                    .sum()
            })
            .unwrap_or(0)
    }
}

/// Read the original logical size of a recycle bin entry, so cleanup and scanning use the same measurement basis.
fn get_recycle_metadata_size(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_str()?;
    if !name.starts_with("$R") || name.len() <= 2 {
        return None;
    }

    let metadata_path = path.parent()?.join(format!("$I{}", &name[2..]));
    let bytes = fs::read(metadata_path).ok()?;
    if bytes.len() < 16 {
        return None;
    }

    Some(u64::from_le_bytes(bytes[8..16].try_into().ok()?))
}

/// Convert to actual occupied space using the specified volume's cluster size, avoiding wrong cluster sizes for cross-drive recycle bin statistics.
fn align_physical_size(logical_size: u64, cluster_size: u32) -> u64 {
    if logical_size == 0 || cluster_size == 0 {
        return 0;
    }

    let cluster_size = cluster_size as u64;
    ((logical_size + cluster_size - 1) / cluster_size) * cluster_size
}

/// Deletion attempt error, keeping the original system error code to distinguish "locked" from permission problems.
#[derive(Debug)]
struct DeleteAttemptError {
    message: String,
    raw_os_error: Option<i32>,
}

/// Extract the drive root path needed by the Shell API from a recycle bin data path.
fn recycle_drive_root(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    if bytes.len() < 2 || bytes[1] != b':' || !bytes[0].is_ascii_alphabetic() {
        return None;
    }

    Some(format!("{}:\\", (bytes[0] as char).to_ascii_uppercase()))
}

/// Case-insensitive `strip_prefix` that never panics on a non-char boundary.
fn strip_ascii_prefix<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        Some(&text[prefix.len()..])
    } else {
        None
    }
}

/// Derive the $I metadata twin of a recycle bin entry from its $R data path.
///
/// A recycle bin entry is a pair sharing one suffix: `$R<suffix>` holds the data,
/// `$I<suffix>` holds the size / original-path / deletion-time metadata the Recycle Bin
/// UI displays. Deleting only one half leaves either invisible data or a phantom entry.
fn recycle_entry_twin_path(path: &str) -> Option<String> {
    let file_path = Path::new(path);
    let parent = file_path.parent()?;
    let file_name = file_path.file_name()?.to_string_lossy();
    let suffix = strip_ascii_prefix(&file_name, "$R")?;
    if suffix.is_empty() {
        return None;
    }
    Some(
        parent
            .join(format!("$I{}", suffix))
            .to_string_lossy()
            .into_owned(),
    )
}

/// Delete one recycle bin entry: the $R data blob plus its $I metadata twin.
///
/// H1 audit fix. The previous flow grouped the selected paths per drive and called
/// `SHEmptyRecycleBinW`, whose semantics are "empty the whole volume" — selecting 3
/// entries wiped every other item on that drive, silently and unrecoverably. The
/// scanner only surfaces entries under the current user's own SID folder, where the
/// owning user holds Full Control, so removing the pair directly needs no SYSTEM
/// privileges and leaves nothing behind for Explorer to display.
///
/// Guards, in order: `path_guard` rejects traversal and empty input;
/// `is_current_user_entry_path` pins the target to the current user's own recycle
/// data; the $I twin must live in the same directory. A tampered frontend therefore
/// cannot aim this at arbitrary files or at another account's entries.
fn delete_recycle_entry(path: &str) -> Result<(), String> {
    let file_path = Path::new(path);
    crate::path_guard::validate_deletable(file_path)?;
    if !crate::scanner::recycle_bin::is_current_user_entry_path(path) {
        return Err(format!(
            "路径不是当前用户的回收站数据文件，已拒绝: {}",
            path
        ));
    }
    let metadata_path = recycle_entry_twin_path(path)
        .ok_or_else(|| format!("路径不是 $R 回收站条目，已拒绝删除: {}", path))?;

    // Clear hidden/system/read-only attributes first: $Recycle.Bin contents carry the
    // hidden bit, and std::fs removal refuses read-only files.
    let _ = windows_api::remove_protection_attributes(path);
    if file_path.is_dir() {
        fs::remove_dir_all(file_path)
            .map_err(|error| format!("删除回收站数据目录失败: {}", error))?;
    } else {
        fs::remove_file(file_path).map_err(|error| format!("删除回收站数据文件失败: {}", error))?;
    }

    // The data blob is gone; remove the metadata so Explorer stops showing the entry.
    // Deleting the data first means a metadata failure can never hide live data — worst
    // case is a phantom entry, reported honestly as a failure here.
    let _ = windows_api::remove_protection_attributes(&metadata_path);
    fs::remove_file(&metadata_path).map_err(|error| {
        format!(
            "回收站数据已删除但元数据清理失败 ({}): {}",
            metadata_path, error
        )
    })?;

    Ok(())
}

/// Extract the partition root directory from a normal file path, for reading cluster size per volume.
fn drive_root(path: &Path) -> Option<String> {
    let path = path.to_string_lossy();
    let bytes = path.as_bytes();
    if bytes.len() < 2 || bytes[1] != b':' || !bytes[0].is_ascii_alphabetic() {
        return None;
    }
    Some(format!("{}:\\", (bytes[0] as char).to_ascii_uppercase()))
}

impl Default for EnhancedDeleteEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_physical_size_calculation() {
        let engine = EnhancedDeleteEngine::new();

        // Assume a cluster size of 4096
        assert_eq!(engine.calculate_physical_size(1), 4096);
        assert_eq!(engine.calculate_physical_size(4096), 4096);
        assert_eq!(engine.calculate_physical_size(4097), 8192);
        assert_eq!(engine.calculate_physical_size(0), 0);
    }

    #[test]
    fn test_safe_ownership_check() {
        let engine = EnhancedDeleteEngine::new();

        assert!(engine.is_safe_for_ownership(Path::new("C:\\Windows\\Temp\\test.tmp")));
        assert!(engine
            .is_safe_for_ownership(Path::new("C:\\Users\\Test\\AppData\\Local\\Temp\\file.log")));
        assert!(engine.is_safe_for_ownership(Path::new(
            "C:\\ProgramData\\Microsoft\\Windows Defender\\Support\\MPLog.log"
        )));
        assert!(!engine.is_safe_for_ownership(Path::new(
            "C:\\ProgramData\\Microsoft\\Windows Defender\\Support2\\MPLog.log"
        )));
        assert!(!engine.is_safe_for_ownership(Path::new("C:\\Windows\\System32\\test.dll")));
        // Cache directories added by deep scanning (Defender scan history, font cache, clipboard, NVIDIA shader variants)
        // must also allow safe elevated deletion.
        assert!(engine.is_safe_for_ownership(Path::new(
            "C:\\ProgramData\\Microsoft\\Windows Defender\\Scans\\History\\Service\\entry.bin"
        )));
        assert!(engine.is_safe_for_ownership(Path::new(
            "C:\\Windows\\ServiceProfiles\\LocalService\\AppData\\Local\\FontCache\\font.bin"
        )));
        assert!(engine.is_safe_for_ownership(Path::new(
            "C:\\Users\\Test\\AppData\\Local\\Microsoft\\Windows\\Clipboard\\entry.bin"
        )));
        assert!(engine.is_safe_for_ownership(Path::new(
            "C:\\Users\\Test\\AppData\\Local\\NVIDIA\\GLCache\\shader.bin"
        )));
    }

    #[test]
    fn test_system_protected_check() {
        let engine = EnhancedDeleteEngine::new();

        assert!(engine.is_system_protected(Path::new("C:\\Windows\\System32\\ntdll.dll")));
        assert!(engine.is_system_protected(Path::new("C:\\pagefile.sys")));
        assert!(!engine.is_system_protected(Path::new("C:\\Temp\\test.tmp")));

        // C1 regression: the bypass spellings must hit the protection check as well
        assert!(engine.is_system_protected(Path::new("C:/Windows/System32/ntdll.dll")));
        assert!(engine.is_system_protected(Path::new(r"\\?\C:\Windows\System32\ntdll.dll")));
        // And traversal paths are rejected outright by the guard, before any deletion
        assert!(crate::path_guard::validate_deletable(Path::new(
            r"C:\Temp\..\Windows\System32\config"
        ))
        .is_err());
    }

    #[test]
    fn test_extended_junk_paths_are_deletable() {
        let engine = EnhancedDeleteEngine::new();

        // Caches/diagnostic directories newly included in deep scanning must be neither blocked by system protection nor disallowed for elevated deletion,
        // otherwise they are found but can never be deleted.
        let deletable_paths = [
            r"C:\Windows\SystemTemp\setup.tmp",
            r"C:\Windows\LiveKernelReports\WATCHDOG\report.dmp",
            r"C:\Users\Test\AppData\Local\Microsoft\Windows\Explorer\iconcache.db",
            r"C:\Users\Test\AppData\Local\Google\Chrome\User Data\Default\Service Worker\CacheStorage\abc",
            r"C:\Users\Test\AppData\Local\NVIDIA\ComputeCache\shader.bin",
            r"C:\ProgramData\Microsoft\Windows\WER\ReportQueue\AppCrash\report.wer",
        ];

        for path in deletable_paths {
            assert!(
                !engine.is_system_protected(Path::new(path)),
                "不应被系统保护拦截: {}",
                path
            );
            assert!(
                engine.is_safe_for_ownership(Path::new(path)),
                "应允许提权删除: {}",
                path
            );
        }
    }

    #[test]
    fn test_recycle_drive_root() {
        // The Shell API empties by drive letter; invalid paths must be rejected rather than falling back to C drive by default.
        assert_eq!(
            recycle_drive_root(r"d:\$Recycle.Bin\item"),
            Some("D:\\".to_string())
        );
        assert_eq!(recycle_drive_root(r"not-a-windows-path"), None);
    }

    #[test]
    fn test_recycle_entry_twin_path() {
        // H1: an entry is a $R data blob + a $I metadata twin sharing one suffix
        assert_eq!(
            recycle_entry_twin_path(r"D:\$Recycle.Bin\S-1-5-21\$R00000001").as_deref(),
            Some(r"D:\$Recycle.Bin\S-1-5-21\$I00000001")
        );
        // NTFS is case-insensitive: a lowercase $r name still maps to the $I twin
        assert_eq!(
            recycle_entry_twin_path(r"D:\$Recycle.Bin\S-1-5-21\$r00000001").as_deref(),
            Some(r"D:\$Recycle.Bin\S-1-5-21\$I00000001")
        );
        // Anything that is not an $R file has no twin
        assert_eq!(recycle_entry_twin_path(r"C:\Temp\notes.txt"), None);
        assert_eq!(
            recycle_entry_twin_path(r"D:\$Recycle.Bin\S-1-5-21\$I00000001"),
            None
        );
        // A bare "$R" without the mandatory suffix is not an entry either
        assert_eq!(
            recycle_entry_twin_path(r"D:\$Recycle.Bin\S-1-5-21\$R"),
            None
        );
    }

    #[test]
    fn test_recycle_entry_deletion_guards() {
        // H1 guards: a tampered frontend cannot aim the per-entry deletion at arbitrary files.
        // Traversal input is rejected by path_guard before anything is touched.
        assert!(
            delete_recycle_entry(r"C:\$Recycle.Bin\S-1-5-21\..\..\Windows\System32\config")
                .is_err()
        );
        // An arbitrary user file is not a current-user recycle data entry.
        assert!(delete_recycle_entry(r"C:\Temp\definitely-not-recycle.txt").is_err());
        // A recycle-bin path without the $R prefix (e.g. the $I metadata itself) is rejected.
        assert!(delete_recycle_entry(r"C:\$Recycle.Bin\S-1-5-21\$I00000001").is_err());
    }
}
