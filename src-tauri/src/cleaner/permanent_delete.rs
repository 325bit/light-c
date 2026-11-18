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
// Check 1: protection rules - the shared `path_guard` table plus the user whitelist
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

use crate::error::ErrorCode;
use crate::path_guard::{ensure_deletable, DeleteScope};
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
    /// A match was found in the registry (legacy variant; the check is no longer run)
    FoundInRegistry {
        matched_field: String,
        matched_value: String,
    },
    /// Executable files were found (executable check failed)
    ContainsExecutables { files: Vec<String> },
    /// The path is inside a system-protected directory, or a whitelist refused it
    InProtectedPath { reason: String },
}

impl SafetyCheckResult {
    pub fn is_safe(&self) -> bool {
        matches!(self, SafetyCheckResult::Safe)
    }

    /// Machine-readable category for the frontend. Every failing check is a
    /// protection rule, so they all map to `Protected` — the distinction that
    /// matters to the UI is carried by the variant itself, which is already
    /// serialized alongside.
    pub fn error_code(&self) -> Option<ErrorCode> {
        match self {
            SafetyCheckResult::Safe => None,
            SafetyCheckResult::FoundInRegistry { .. }
            | SafetyCheckResult::ContainsExecutables { .. }
            | SafetyCheckResult::InProtectedPath { .. } => Some(ErrorCode::Protected),
        }
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

/// Classify a raw deletion failure message.
///
/// This is the one place where sniffing the OS text is legitimate: the string
/// comes straight from `remove_dir_all` / `icacls`, before anyone formatted it
/// for a human. Everywhere else the UI must switch on `failure_code` instead.
fn classify_delete_failure(message: &str) -> ErrorCode {
    let lower = message.to_ascii_lowercase();
    if lower.contains("access is denied")
        || message.contains("拒绝访问")
        || lower.contains("os error 5")
    {
        ErrorCode::PermissionDenied
    } else if lower.contains("being used")
        || message.contains("占用")
        || lower.contains("os error 32")
    {
        ErrorCode::TargetInUse
    } else if lower.contains("not find")
        || message.contains("找不到")
        || lower.contains("os error 2")
        || lower.contains("os error 3")
    {
        ErrorCode::NotFound
    } else {
        ErrorCode::Internal
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
    /// Failure reason (free-form text, shown in tooltips and logs)
    pub failure_reason: Option<String>,
    /// Machine-readable failure category, so the UI never has to sniff `failure_reason` text.
    /// `None` on success; absent from the wire when `None` keeps old payloads compatible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<ErrorCode>,
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
// Safety check constants
//
// The protected-path table lives in `path_guard` (S3a). This module only owns
// the executable-extension list, which no other engine uses.
// ============================================================================

/// Executable file extensions (the executable check)
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

        // S3a: traversal, the shared protection table, and the shell-folder rules
        // for this scope are one call now. `Leftovers` is the strictest scope: a
        // "leftover" inside Documents or on the Desktop is a scanner bug, so those
        // subtrees are refused here even though large-file cleanup may touch them.
        if let Err(error) = ensure_deletable(path, DeleteScope::Leftovers) {
            return SafetyCheckResult::InProtectedPath {
                reason: error.detail.unwrap_or(error.message),
            };
        }

        // If the protection rules cannot be loaded, reject by default, so a config problem cannot weaken the permanent-delete safety boundary.
        let user_whitelist = match &self.user_whitelist {
            Ok(entries) => entries,
            Err(error) => {
                return SafetyCheckResult::InProtectedPath {
                    reason: format!("user whitelist could not be loaded: {error}"),
                };
            }
        };

        if leftover_whitelist::contains_path(user_whitelist, path) {
            return SafetyCheckResult::InProtectedPath {
                reason: format!("path is on the user whitelist: {path_str}"),
            };
        }

        // ====================================================================
        // Executable file check
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

    /// Scan the directory for executable files
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
                            failure_code: safety_check.error_code(),
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
                            failure_code: safety_check.error_code(),
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
                    failure_code: None,
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
                            failure_code: Some(ErrorCode::TargetInUse),
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
                    failure_code: Some(classify_delete_failure(&e)),
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
    fn raw_os_failures_map_to_actionable_codes() {
        // `failure_code` is what the UI switches on, so the classification of the
        // raw OS text has to stay stable even when the wording differs by locale.
        assert_eq!(
            classify_delete_failure("拒绝访问。 (os error 5)"),
            ErrorCode::PermissionDenied
        );
        assert_eq!(
            classify_delete_failure("Access is denied. (os error 5)"),
            ErrorCode::PermissionDenied
        );
        assert_eq!(
            classify_delete_failure("另一个程序正在使用此文件 (os error 32)"),
            ErrorCode::TargetInUse
        );
        assert_eq!(
            classify_delete_failure("系统找不到指定的文件。 (os error 2)"),
            ErrorCode::NotFound
        );
        assert_eq!(classify_delete_failure("boom"), ErrorCode::Internal);
    }

    #[test]
    fn safety_failures_carry_the_protected_code() {
        assert_eq!(SafetyCheckResult::Safe.error_code(), None);
        assert_eq!(
            SafetyCheckResult::InProtectedPath {
                reason: "system path".to_string()
            }
            .error_code(),
            Some(ErrorCode::Protected)
        );
        assert_eq!(
            SafetyCheckResult::ContainsExecutables {
                files: vec!["a.exe".to_string()]
            }
            .error_code(),
            Some(ErrorCode::Protected)
        );
    }

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
    fn protected_paths_come_from_the_shared_table() {
        fn refused(path: &str) -> bool {
            ensure_deletable(Path::new(path), DeleteScope::Leftovers).is_err()
        }

        // Paths that should be protected
        assert!(refused(r"C:\Windows\System32\test"));
        assert!(refused(r"C:\Users\test\Desktop\file"));
        assert!(refused(r"C:\"));

        // Paths that should be allowed
        assert!(!refused(r"C:\Users\test\AppData\Local\SomeApp"));
    }

    /// C1 regression: bypass spellings must not slip past the protected-path check.
    #[test]
    fn bypass_spellings_are_blocked() {
        fn refused(path: &str) -> bool {
            ensure_deletable(Path::new(path), DeleteScope::Leftovers).is_err()
        }

        // Forward slashes previously matched no `\`-based prefix at all
        assert!(refused(r"C:/Windows/System32/test"));
        assert!(refused(r"C:/"));

        // Extended-length prefix previously kept `\\?\` in the string
        assert!(refused(r"\\?\C:\Windows\System32\test"));
        assert!(refused(r"\\?\C:\"));

        // Traversal paths are rejected by the guard before any check runs
        let engine = PermanentDeleteEngine::new();
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
