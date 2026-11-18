// ============================================================================
// Permanent delete engine - deep cleanup of uninstall leftovers
//
// ⚠️ WARNING: this module performs direct physical deletion; data cannot be recovered!
//
// [Core functionality]
// Directly use std::fs::remove_dir_all and std::fs::remove_file to permanently delete files from disk,
// rather than moving them to the recycle bin. This is to thoroughly clean up leftover data from uninstalled software.
//
// [Why check .exe files?]
// Checking whether .exe/.dll/.sys files exist in a folder before deletion is crucial, for these reasons:
// 1. Portable apps: users may run portable software placed under AppData,
//    which has no installation record but is still in active use.
// 2. Auto-start programs: some executables may reside in AppData and be configured to start automatically.
// 3. System components: DLL files of some system services may be located in scanned directories.
// 4. Driver files: .sys files are kernel drivers; deleting them by mistake may crash the system.
//
// If such files are detected, we skip the folder and mark it as "requires manual review",
// letting the user decide whether to delete it, thus avoiding accidental deletion of software in use.
//
// [Safety check protocol]
// Check 1: core whitelist check - ensure the path is not inside a critical system directory
// Check 2: executable file check - scan for .exe/.dll/.sys files; if found, mark for manual review
//
// Note: the registry matching check is no longer performed, because the scoring engine has already confirmed the target is an uninstall leftover,
// and registry keys of uninstalled programs are themselves leftover data (zombie entry), so they must not be used to block cleanup.
//
// [Error handling]
// - All IO operations are wrapped in Result
// - When a file is locked, automatically switch to the "delete on reboot" queue
// - When permissions are insufficient, try taking ownership and retry
// ============================================================================

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use log::{debug, info, warn};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::scanner::leftover_whitelist::{self, LeftoverWhitelistEntry};

#[cfg(windows)]
use crate::cleaner::enhanced_delete::windows_api;

// ============================================================================
// Safety check result type
// ============================================================================

/// Safety check result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SafetyCheckResult {
    /// Passed all checks, safe to delete
    Safe,
    /// A match was found in the registry (Check 1 failed)
    FoundInRegistry {
        matched_field: String,
        matched_value: String,
    },
    /// Executable files were found (Check 2 failed)
    ContainsExecutables { files: Vec<String> },
    /// The path is inside a system-protected directory (Check 3 failed)
    InProtectedPath { reason: String },
}

impl SafetyCheckResult {
    pub fn is_safe(&self) -> bool {
        matches!(self, SafetyCheckResult::Safe)
    }

    pub fn display_message(&self) -> String {
        match self {
            SafetyCheckResult::Safe => "安全".to_string(),
            SafetyCheckResult::FoundInRegistry {
                matched_field,
                matched_value,
            } => {
                format!("注册表中存在匹配: {} = {}", matched_field, matched_value)
            }
            SafetyCheckResult::ContainsExecutables { files } => {
                let count = files.len();
                let preview: Vec<_> = files.iter().take(3).cloned().collect();
                if count > 3 {
                    format!("包含 {} 个可执行文件: {} 等", count, preview.join(", "))
                } else {
                    format!("包含可执行文件: {}", preview.join(", "))
                }
            }
            SafetyCheckResult::InProtectedPath { reason } => {
                format!("系统保护路径: {}", reason)
            }
        }
    }
}

/// Deletion result for a single leftover
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeftoverDeleteResult {
    /// Folder path
    pub path: String,
    /// Whether deletion succeeded
    pub success: bool,
    /// Number of files deleted
    pub deleted_files: u32,
    /// Space freed (bytes)
    pub freed_size: u64,
    /// Failure reason
    pub failure_reason: Option<String>,
    /// Whether it is marked for deletion on reboot
    pub marked_for_reboot: bool,
    /// Whether manual review is required
    pub needs_manual_review: bool,
    /// Safety check result
    pub safety_check: SafetyCheckResult,
}

/// Overall result of permanent deletion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermanentDeleteResult {
    /// Number of folders successfully deleted
    pub success_count: usize,
    /// Number of folders that failed
    pub failed_count: usize,
    /// Number requiring manual review
    pub manual_review_count: usize,
    /// Number marked for deletion on reboot
    pub reboot_pending_count: usize,
    /// Actual space freed (bytes)
    pub freed_size: u64,
    /// Detailed results for each folder
    pub details: Vec<LeftoverDeleteResult>,
    /// Deletion duration (milliseconds)
    pub duration_ms: u64,
}

// ============================================================================
// Core whitelist - system paths that are absolutely forbidden to delete
// ============================================================================

/// Core system-protected paths (Check 3)
/// Nothing under these paths may be deleted through this module
const PROTECTED_PATHS: &[&str] = &[
    // Windows system directories
    r"C:\Windows",
    r"C:\Windows\System32",
    r"C:\Windows\SysWOW64",
    r"C:\Windows\WinSxS",
    // Critical user directories
    r"\Desktop",
    r"\Documents",
    r"\Downloads",
    r"\Pictures",
    r"\Videos",
    r"\Music",
    // System root directories
    r"C:\Program Files",
    r"C:\Program Files (x86)",
    r"C:\ProgramData\Microsoft",
    // Boot-related
    r"C:\Boot",
    r"C:\Recovery",
    r"C:\$Recycle.Bin",
    r"C:\System Volume Information",
];

/// Executable file extensions (Check 2)
const EXECUTABLE_EXTENSIONS: &[&str] = &["exe", "dll", "sys", "drv", "ocx", "cpl", "scr"];

// ============================================================================
// Permanent delete engine
// ============================================================================

/// Permanent delete engine
///
/// Explanation
/// This engine performs deep cleanup of uninstall leftovers. Unlike normal deletion, it physically deletes files directly from disk,
/// without going through the recycle bin, so data cannot be recovered.
///
/// To ensure safety, a "triple safety check protocol" is run before every deletion:
/// 1. Registry check: confirm the target is not part of an installed program
/// 2. Executable check: confirm the target does not contain a program in use
/// 3. Whitelist check: confirm the target is not inside a critical system path
pub struct PermanentDeleteEngine {
    /// Whether delete-on-reboot fallback is enabled
    enable_reboot_fallback: bool,
    /// Fix the whitelist snapshot when building the delete engine, so concurrent batch deletions do not need to re-read the disk for every path.
    user_whitelist: Result<Vec<LeftoverWhitelistEntry>, String>,
}

impl PermanentDeleteEngine {
    /// Create a new permanent delete engine
    pub fn new() -> Self {
        info!("永久删除引擎初始化完成");

        PermanentDeleteEngine {
            enable_reboot_fallback: true,
            user_whitelist: leftover_whitelist::list_entries(),
        }
    }

    // ========================================================================
    // Safety check protocol (whitelist + executable check)
    // ========================================================================

    /// Perform the full triple safety check
    ///
    /// Explanation
    /// This function runs the three safety checks in sequence; any failure blocks the deletion.
    /// This is the core mechanism protecting user data safety.
    pub fn perform_safety_checks(&self, path: &Path) -> SafetyCheckResult {
        self.perform_safety_checks_with(path, false)
    }

    /// Same as above, but may optionally **allow the "contains executable files" check**.
    ///
    /// This is only reached when the user explicitly clicks "Force Delete" in the results dialog:
    /// whitelist protection, system-directory protection, and validation of the leftover path itself **are never skipped** —
    /// the only check relaxed is "reject if the directory contains .exe/.dll/.sys",
    /// because the most common uninstall leftovers are directories containing programs (emulators, portable software).
    fn perform_safety_checks_with(
        &self,
        path: &Path,
        allow_executables: bool,
    ) -> SafetyCheckResult {
        let path_str = path.to_string_lossy().to_string();

        // C1 fix, gate 0: reject `..` / `.` traversal before any check.
        // The protected-path matching below works on the literal string while
        // remove_dir_all resolves `..`, so a traversal path must never get here.
        if let Err(error) = crate::path_guard::validate_deletable(path) {
            return SafetyCheckResult::InProtectedPath { reason: error };
        }

        // If the protection rules cannot be loaded, reject by default, so a config problem cannot weaken the permanent-delete safety boundary.
        let user_whitelist = match &self.user_whitelist {
            Ok(entries) => entries,
            Err(error) => {
                return SafetyCheckResult::InProtectedPath {
                    reason: format!("无法读取用户白名单，已拒绝删除: {}", error),
                };
            }
        };

        if leftover_whitelist::contains_path(user_whitelist, path) {
            return SafetyCheckResult::InProtectedPath {
                reason: format!("路径受用户白名单保护: {}", path_str),
            };
        }

        // ====================================================================
        // Check 1: core whitelist check (runs first, strictest)
        // ====================================================================
        if let Some(reason) = self.check_protected_path(&path_str) {
            return SafetyCheckResult::InProtectedPath { reason };
        }

        // ====================================================================
        // Check 2: executable file check
        // ====================================================================
        if !allow_executables {
            let executables = self.scan_executables(path);
            if !executables.is_empty() {
                return SafetyCheckResult::ContainsExecutables { files: executables };
            }
        }

        // Note: the registry matching check is no longer performed.
        // Reason: calling deep cleanup requires the scoring engine to have already determined the folder is an uninstall leftover,
        // and registry keys of uninstalled programs may themselves be leftovers (zombie entry),
        // so using leftover registry keys to block deletion of leftover folders is self-contradictory.
        // The whitelist + executable checks already provide sufficient safety guarantees.

        SafetyCheckResult::Safe
    }

    /// Check 2: scan the directory for executable files
    ///
    /// Explanation
    /// Recursively scan the target directory for executable files such as .exe, .dll and .sys.
    /// Finding these files means the directory may contain software in use and requires manual review.
    fn scan_executables(&self, path: &Path) -> Vec<String> {
        let mut executables = Vec::new();

        // Limit scan depth and count to avoid performance issues
        let max_depth = 5;
        let max_results = 10;

        for entry in WalkDir::new(path)
            .max_depth(max_depth)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if executables.len() >= max_results {
                break;
            }

            if entry.file_type().is_file() {
                if let Some(ext) = entry.path().extension() {
                    let ext_lower = ext.to_string_lossy().to_lowercase();
                    if EXECUTABLE_EXTENSIONS.contains(&ext_lower.as_str()) {
                        let file_name = entry.file_name().to_string_lossy().to_string();
                        executables.push(file_name);
                    }
                }
            }
        }

        executables
    }

    /// Check 3: check whether the path is inside a system-protected directory
    ///
    /// Explanation
    /// Verify the target path is not inside critical system directories such as C:\Windows, Desktop or Documents.
    /// This is the last line of defense, ensuring system files are never deleted by mistake.
    ///
    /// Path matching rules:
    /// - Absolute paths (e.g. C:\Windows): only use starts_with plus a path-separator boundary check
    /// - Relative paths (e.g. \Desktop, \Documents): use path-component matching (\Desktop\ or ends with \Desktop)
    fn check_protected_path(&self, path: &str) -> Option<String> {
        // C1 fix: normalize separators and strip the `\\?\` prefix before matching,
        // so `C:/Windows/...` and `\\?\C:\Windows\...` can no longer bypass the
        // `\`-based prefix checks below.
        let normalized = crate::path_guard::normalize(Path::new(path));
        let path_lower = normalized.to_lowercase();

        for protected in PROTECTED_PATHS {
            let protected_lower = protected.to_lowercase();

            // Determine whether it is an absolute path (with a drive letter) or a relative path (starting with \)
            let is_absolute = protected_lower.len() >= 2
                && protected_lower.as_bytes().get(1).copied() == Some(b':');

            if is_absolute {
                // Absolute path: only starts_with plus a path-separator boundary check
                // For example C:\Windows matches C:\Windows\System32, but not C:\WindowsApp
                if path_lower.starts_with(&protected_lower) {
                    let next_char = path_lower[protected_lower.len()..].chars().next();
                    if next_char.is_none() || next_char == Some('\\') {
                        return Some(format!("路径包含受保护目录: {}", protected));
                    }
                }
            } else {
                // Relative path (e.g. \Desktop): match at the path-component level
                // Check whether the path contains \Desktop\ or ends with \Desktop
                // Prevent \Desktop from wrongly matching \DesktopApp
                let comp_with_sep = format!("{}\\", protected_lower);
                if path_lower
                    .starts_with(&format!("{}\\", protected_lower.trim_start_matches('\\')))
                    || path_lower.contains(&comp_with_sep)
                    || path_lower.ends_with(&protected_lower)
                {
                    let reason =
                        format!("路径包含受保护目录: {}", protected.trim_start_matches('\\'));
                    return Some(reason);
                }
            }
        }

        // Extra check: do not allow deleting a drive root directory
        if normalized.len() <= 3 {
            return Some("不允许删除驱动器根目录".to_string());
        }

        // Check whether it is a direct child of a user directory (e.g. C:\Users\xxx itself)
        if path_lower.starts_with(r"c:\users\") {
            let parts: Vec<&str> = normalized.split('\\').collect();
            if parts.len() <= 3 {
                return Some("不允许删除用户根目录".to_string());
            }
        }

        None
    }

    // ========================================================================
    // Deletion execution
    // ========================================================================

    /// Perform permanent deletion (concurrent processing)
    ///
    /// Explanation
    /// Uses a rayon thread pool to delete multiple directories concurrently, keeping the UI responsive.
    /// Every directory runs the triple safety check before deletion.
    pub fn delete_leftovers(
        &self,
        paths: Vec<String>,
        allow_executables: bool,
    ) -> PermanentDeleteResult {
        let start_time = std::time::Instant::now();

        // Use atomic counters for concurrent statistics
        let success_count = AtomicUsize::new(0);
        let failed_count = AtomicUsize::new(0);
        let manual_review_count = AtomicUsize::new(0);
        let reboot_pending_count = AtomicUsize::new(0);
        let freed_size = AtomicU64::new(0);

        // Execute deletions concurrently
        let details: Vec<LeftoverDeleteResult> = paths
            .par_iter()
            .map(|path_str| {
                let path = Path::new(path_str);

                // Run the triple safety check
                let safety_check = self.perform_safety_checks_with(path, allow_executables);

                match &safety_check {
                    SafetyCheckResult::Safe => {
                        // Passed the safety check, perform the deletion
                        let result = self.delete_single_leftover(path);

                        if result.success {
                            success_count.fetch_add(1, Ordering::Relaxed);
                            freed_size.fetch_add(result.freed_size, Ordering::Relaxed);
                        } else if result.marked_for_reboot {
                            reboot_pending_count.fetch_add(1, Ordering::Relaxed);
                        } else {
                            failed_count.fetch_add(1, Ordering::Relaxed);
                        }

                        result
                    }
                    SafetyCheckResult::ContainsExecutables { .. } => {
                        // Contains executable files, mark as requiring manual review
                        manual_review_count.fetch_add(1, Ordering::Relaxed);

                        LeftoverDeleteResult {
                            path: path_str.clone(),
                            success: false,
                            deleted_files: 0,
                            freed_size: 0,
                            failure_reason: Some(safety_check.display_message()),
                            marked_for_reboot: false,
                            needs_manual_review: true,
                            safety_check,
                        }
                    }
                    _ => {
                        // Other safety checks failed
                        failed_count.fetch_add(1, Ordering::Relaxed);

                        LeftoverDeleteResult {
                            path: path_str.clone(),
                            success: false,
                            deleted_files: 0,
                            freed_size: 0,
                            failure_reason: Some(safety_check.display_message()),
                            marked_for_reboot: false,
                            needs_manual_review: false,
                            safety_check,
                        }
                    }
                }
            })
            .collect();

        let duration_ms = start_time.elapsed().as_millis() as u64;

        info!(
            "永久删除完成: 成功 {}, 失败 {}, 待审核 {}, 待重启 {}, 释放 {} 字节, 耗时 {}ms",
            success_count.load(Ordering::Relaxed),
            failed_count.load(Ordering::Relaxed),
            manual_review_count.load(Ordering::Relaxed),
            reboot_pending_count.load(Ordering::Relaxed),
            freed_size.load(Ordering::Relaxed),
            duration_ms
        );

        PermanentDeleteResult {
            success_count: success_count.load(Ordering::Relaxed),
            failed_count: failed_count.load(Ordering::Relaxed),
            manual_review_count: manual_review_count.load(Ordering::Relaxed),
            reboot_pending_count: reboot_pending_count.load(Ordering::Relaxed),
            freed_size: freed_size.load(Ordering::Relaxed),
            details,
            duration_ms,
        }
    }

    /// Delete a single leftover directory
    ///
    /// Explanation
    /// This function performs the actual physical deletion. It first tries direct deletion,
    /// and automatically switches to the "delete on reboot" queue when locked files are encountered.
    fn delete_single_leftover(&self, path: &Path) -> LeftoverDeleteResult {
        let path_str = path.to_string_lossy().to_string();

        // First calculate the directory size
        let (total_size, file_count) = self.calculate_dir_size(path);

        // ====================================================================
        // ⚠️ WARNING: the following code performs permanent deletion; data cannot be recovered!
        // ====================================================================

        // Try to delete the entire directory directly
        match self.try_remove_dir_all(path) {
            Ok(()) => {
                info!(
                    "成功永久删除: {} ({} 文件, {} 字节)",
                    path_str, file_count, total_size
                );

                LeftoverDeleteResult {
                    path: path_str,
                    success: true,
                    deleted_files: file_count,
                    freed_size: total_size,
                    failure_reason: None,
                    marked_for_reboot: false,
                    needs_manual_review: false,
                    safety_check: SafetyCheckResult::Safe,
                }
            }
            Err(e) => {
                warn!("直接删除失败: {} - {}", path_str, e);

                // Try deletion on reboot
                if self.enable_reboot_fallback {
                    if self.mark_for_reboot_delete(path) {
                        return LeftoverDeleteResult {
                            path: path_str,
                            success: false,
                            deleted_files: 0,
                            freed_size: 0,
                            failure_reason: Some("已标记为重启后删除".to_string()),
                            marked_for_reboot: true,
                            needs_manual_review: false,
                            safety_check: SafetyCheckResult::Safe,
                        };
                    }
                }

                LeftoverDeleteResult {
                    path: path_str,
                    success: false,
                    deleted_files: 0,
                    freed_size: 0,
                    failure_reason: Some(format!("删除失败: {}", e)),
                    marked_for_reboot: false,
                    needs_manual_review: false,
                    safety_check: SafetyCheckResult::Safe,
                }
            }
        }
    }

    /// Try to delete the entire directory
    ///
    /// Explanation
    /// Uses std::fs::remove_dir_all to recursively delete the directory and all its contents.
    /// If read-only files are encountered, it first tries to remove the read-only attribute and retry.
    fn try_remove_dir_all(&self, path: &Path) -> Result<(), String> {
        // ====================================================================
        // ⚠️ WARNING: this operation permanently deletes disk data and cannot be undone!
        // ====================================================================

        // One kind of uninstall leftover is a **file** (an orphaned ext4.vhdx / vmdk, etc.).
        // remove_dir_all always reports "invalid directory name" for a file, then falls through to "delete on reboot" —
        // the user thinks it was deleted, but no space was actually freed.
        if fs::symlink_metadata(path)
            .map(|metadata| metadata.is_file())
            .unwrap_or(false)
        {
            return fs::remove_file(path).map_err(|error| format!("{}", error));
        }

        // First try direct deletion
        match fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                debug!("权限不足，尝试移除保护属性后重试: {:?}", path);
            }
            Err(e) => return Err(format!("{}", e)),
        }

        // Try removing protection attributes from all files and retry
        self.remove_all_protection_attributes(path);

        // ====================================================================
        // ⚠️ WARNING: this operation permanently deletes disk data and cannot be undone!
        // ====================================================================
        fs::remove_dir_all(path).map_err(|e| format!("{}", e))
    }

    /// Remove protection attributes from all files under a directory
    fn remove_all_protection_attributes(&self, path: &Path) {
        for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
            let entry_path = entry.path().to_string_lossy().to_string();
            #[cfg(windows)]
            let _ = windows_api::remove_protection_attributes(&entry_path);
        }
    }

    /// Mark a directory for deletion on reboot
    ///
    /// MoveFileExW + MOVEFILE_DELAY_UNTIL_REBOOT requires a directory to be deleted only after all its contents
    /// have been removed. Therefore the marking order must be: deepest subdirectory → files → top-level directory.
    fn mark_for_reboot_delete(&self, path: &Path) -> bool {
        let mut any_marked = false;

        // Collect all entries and sort by depth from deepest to shallowest (directories before files at the same depth)
        let mut entries: Vec<(usize, std::path::PathBuf, bool)> = WalkDir::new(path)
            .into_iter()
            .filter_map(|e| e.ok())
            .map(|e| (e.depth(), e.path().to_path_buf(), e.file_type().is_dir()))
            .collect();
        entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.2.cmp(&a.2)));

        for (_depth, entry_path, _is_dir) in &entries {
            let path_str = entry_path.to_string_lossy().to_string();
            #[cfg(windows)]
            if windows_api::mark_for_delete_on_reboot(&path_str).is_ok() {
                any_marked = true;
            }
        }

        any_marked
    }

    /// Calculate directory size
    fn calculate_dir_size(&self, path: &Path) -> (u64, u32) {
        let mut total_size = 0u64;
        let mut file_count = 0u32;

        for entry in WalkDir::new(path)
            .max_depth(20)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                if let Ok(metadata) = entry.metadata() {
                    total_size += metadata.len();
                    file_count += 1;
                }
            }
        }

        (total_size, file_count)
    }
}

impl Default for PermanentDeleteEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_whitelist_blocks_permanent_deletion_before_other_checks() {
        let protected_path = Path::new(r"C:\Fixture\App");
        let engine = PermanentDeleteEngine {
            enable_reboot_fallback: true,
            user_whitelist: Ok(vec![LeftoverWhitelistEntry {
                path: protected_path.to_string_lossy().to_string(),
                added_at: "2026-01-01T00:00:00Z".to_string(),
            }]),
        };

        let result = engine.perform_safety_checks(Path::new(r"c:\fixture\app\cache"));
        assert!(matches!(result, SafetyCheckResult::InProtectedPath { .. }));
    }

    /// "Force Delete" only relaxes the "contains executable files" check:
    /// directories with an exe can pass, but system directories are still blocked — that is the safety boundary of that button.
    #[test]
    fn force_delete_only_skips_the_executable_check() {
        let root = std::env::temp_dir().join(format!("lightc-force-delete-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let app_dir = root.join("OldUpdater");
        fs::create_dir_all(&app_dir).expect("创建测试目录失败");
        fs::write(app_dir.join("installer.exe"), vec![0u8; 1024]).expect("写入可执行文件失败");

        let engine = PermanentDeleteEngine::new();

        // Default: contains executable files → requires manual review
        assert!(matches!(
            engine.perform_safety_checks(&app_dir),
            SafetyCheckResult::ContainsExecutables { .. }
        ));

        // After allowing: this check no longer blocks (whitelist and system-directory checks are still verified below)
        assert!(matches!(
            engine.perform_safety_checks_with(&app_dir, true),
            SafetyCheckResult::Safe
        ));

        // Allowing executables ≠ allowing the whitelist: the user whitelist still takes priority and blocks.
        // (The system-directory check is covered separately by test_protected_path_check, not repeated here.)
        let guarded = PermanentDeleteEngine {
            enable_reboot_fallback: true,
            user_whitelist: Ok(vec![LeftoverWhitelistEntry {
                path: app_dir.to_string_lossy().to_string(),
                added_at: "2026-01-01T00:00:00Z".to_string(),
            }]),
        };
        assert!(matches!(
            guarded.perform_safety_checks_with(&app_dir, true),
            SafetyCheckResult::InProtectedPath { .. }
        ));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_protected_path_check() {
        let engine = PermanentDeleteEngine::new();

        // Paths that should be protected
        assert!(engine
            .check_protected_path(r"C:\Windows\System32\test")
            .is_some());
        assert!(engine
            .check_protected_path(r"C:\Users\test\Desktop\file")
            .is_some());
        assert!(engine.check_protected_path(r"C:\").is_some());

        // Paths that should be allowed
        assert!(engine
            .check_protected_path(r"C:\Users\test\AppData\Local\SomeApp")
            .is_none());
    }

    /// C1 regression: bypass spellings must not slip past the protected-path check.
    #[test]
    fn bypass_spellings_are_blocked() {
        let engine = PermanentDeleteEngine::new();

        // Forward slashes previously matched no `\`-based prefix at all
        assert!(engine
            .check_protected_path(r"C:/Windows/System32/test")
            .is_some());
        assert!(engine.check_protected_path(r"C:/").is_some());

        // Extended-length prefix previously kept `\\?\` in the string
        assert!(engine
            .check_protected_path(r"\\?\C:\Windows\System32\test")
            .is_some());
        assert!(engine.check_protected_path(r"\\?\C:\").is_some());

        // Traversal paths are rejected by the guard before any check runs
        let result = engine.perform_safety_checks(Path::new(r"C:\Temp\..\Windows\System32\config"));
        assert!(matches!(result, SafetyCheckResult::InProtectedPath { .. }));
    }

    #[test]
    fn test_executable_extensions() {
        assert!(EXECUTABLE_EXTENSIONS.contains(&"exe"));
        assert!(EXECUTABLE_EXTENSIONS.contains(&"dll"));
        assert!(EXECUTABLE_EXTENSIONS.contains(&"sys"));
        assert!(!EXECUTABLE_EXTENSIONS.contains(&"txt"));
    }
}
