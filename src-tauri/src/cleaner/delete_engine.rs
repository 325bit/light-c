// ============================================================================
// Delete engine - core deletion logic implementation
// Multiple layers of safety protection to ensure important files are never deleted by mistake
// ============================================================================

use log::{debug, error, info, warn};
use std::fs;
use std::path::Path;

use crate::error::{AppError, ErrorCode};
use crate::path_guard::{ensure_deletable, DeleteScope};
use crate::scanner::{DeleteResult, FileInfo};

/// Delete engine
pub struct DeleteEngine {
    /// Whether to skip files that are in use
    skip_in_use: bool,
}

impl DeleteEngine {
    /// Create a new delete engine
    pub fn new() -> Self {
        DeleteEngine {
            skip_in_use: true, // skip files in use by default
        }
    }

    /// Set whether to skip files in use
    pub fn with_skip_in_use(mut self, enabled: bool) -> Self {
        self.skip_in_use = enabled;
        self
    }

    /// Delete a list of files
    pub fn delete_files(&self, files: &[FileInfo]) -> DeleteResult {
        let mut result = DeleteResult::new();

        info!("开始删除 {} 个文件", files.len());

        for file in files {
            match self.delete_single_file(&file.path, file.size) {
                Ok((freed, marked_for_reboot)) => {
                    if marked_for_reboot {
                        result.add_reboot_pending(freed);
                        debug!("已标记重启删除: {}", file.path);
                    } else {
                        result.add_success(freed);
                        debug!("成功删除: {}", file.path);
                    }
                }
                Err(e) => {
                    result.add_failure(file.path.clone(), e);
                    warn!(
                        "删除失败: {} - {}",
                        file.path,
                        result
                            .failed_files
                            .last()
                            .map(|f| &f.reason)
                            .unwrap_or(&String::new())
                    );
                }
            }
        }

        info!(
            "删除完成: 成功 {} 个, 失败 {} 个, 待重启 {} 个, 释放空间 {} 字节",
            result.success_count,
            result.failed_count,
            result.reboot_pending_count,
            result.freed_size
        );

        result
    }

    /// Delete a list of specified paths
    pub fn delete_paths(&self, paths: &[String]) -> DeleteResult {
        let mut result = DeleteResult::new();

        info!("开始删除 {} 个路径", paths.len());

        for path in paths {
            let file_path = Path::new(path);
            let size = self.get_path_size(file_path);

            match self.delete_single_file(path, size) {
                Ok((freed, marked_for_reboot)) => {
                    if marked_for_reboot {
                        result.add_reboot_pending(freed);
                        debug!("已标记重启删除: {}", path);
                    } else {
                        result.add_success(freed);
                        debug!("成功删除: {}", path);
                    }
                }
                Err(e) => {
                    result.add_failure(path.clone(), e);
                    warn!("删除失败: {}", path);
                }
            }
        }

        info!(
            "删除完成: 成功 {} 个, 失败 {} 个, 待重启 {} 个, 释放空间 {} 字节",
            result.success_count,
            result.failed_count,
            result.reboot_pending_count,
            result.freed_size
        );

        result
    }

    /// Delete a single file or directory (multiple safety checks)
    /// Returns (freed size, whether marked for deletion on reboot)
    fn delete_single_file(&self, path: &str, size: u64) -> Result<(u64, bool), AppError> {
        let file_path = Path::new(path);

        // S3a: one decision point covers what used to be two separate layers here
        // (protected path + allowed scope). It also covers the `..` / `.` traversal
        // gate that C1 added, because `ensure_deletable` starts with it.
        //
        // The error keeps its code instead of being flattened to a sentence: the
        // per-file result carries `failure_code` to the UI, and the command layer
        // re-validates so a traversal attempt arrives as `invalid_path` rather than
        // an untyped `internal` message.
        if let Err(error) = ensure_deletable(file_path, DeleteScope::JunkClean) {
            error!("安全拦截: 拒绝删除 {} - {}", path, error);
            return Err(error);
        }

        // Check whether the path exists
        if !file_path.exists() {
            return Err(AppError::new(ErrorCode::NotFound).arg("path", path));
        }

        // Try to delete
        if file_path.is_dir() {
            self.delete_directory(file_path, size)
        } else {
            self.delete_file(file_path, size)
        }
    }

    /// Delete a file, returning (size, whether marked for deletion on reboot)
    fn delete_file(&self, path: &Path, size: u64) -> Result<(u64, bool), AppError> {
        // Try to delete the file
        match fs::remove_file(path) {
            Ok(_) => Ok((size, false)),
            Err(e) => {
                // Check whether it is a permission problem or the file is in use
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    // Try removing the read-only attribute before deleting
                    if let Ok(metadata) = fs::metadata(path) {
                        let mut permissions = metadata.permissions();
                        #[allow(clippy::permissions_set_readonly_false)]
                        permissions.set_readonly(false);
                        if fs::set_permissions(path, permissions).is_ok() {
                            if fs::remove_file(path).is_ok() {
                                return Ok((size, false));
                            }
                        }
                    }
                    Err(AppError::new(ErrorCode::PermissionDenied).detail(e.to_string()))
                } else {
                    // Detect a sharing violation (error code 32, ERROR_SHARING_VIOLATION),
                    // when the file is in use by another process it cannot be deleted directly, so mark it for deletion on reboot.
                    // The old guard here re-checked the allowed scope; the entry gate above
                    // already guarantees that, so only the OS condition is left.
                    #[cfg(windows)]
                    let is_sharing_violation = e.raw_os_error() == Some(32); // ERROR_SHARING_VIOLATION
                    #[cfg(not(windows))]
                    let is_sharing_violation = false;

                    if is_sharing_violation {
                        // Call MoveFileExW to mark for deletion on reboot
                        #[cfg(windows)]
                        {
                            let path_str = path.to_string_lossy();
                            match super::enhanced_delete::windows_api::mark_for_delete_on_reboot(
                                &path_str,
                            ) {
                                Ok(_) => {
                                    info!("文件已标记为重启删除: {}", path_str);
                                    return Ok((size, true)); // marked successfully, delete on reboot
                                }
                                Err(mark_err) => {
                                    warn!("标记重启删除失败: {} - {}", path_str, mark_err);
                                }
                            }
                        }
                        Err(AppError::new(ErrorCode::TargetInUse).detail(e.to_string()))
                    } else {
                        Err(AppError::new(ErrorCode::Internal).detail(e.to_string()))
                    }
                }
            }
        }
    }

    /// Delete a directory, returning (size, whether marked for deletion on reboot)
    ///
    /// Note that here we **do not queue "delete on reboot"**, unlike delete_file:
    /// MoveFileExW + MOVEFILE_DELAY_UNTIL_REBOOT requires the directory to be empty,
    /// and when remove_dir_all fails there is usually still content inside, so queueing would not work anyway,
    /// it would only make users think "it will be deleted after reboot". Instead we classify the error and explain the cause clearly.
    fn delete_directory(&self, path: &Path, size: u64) -> Result<(u64, bool), AppError> {
        match fs::remove_dir_all(path) {
            Ok(_) => Ok((size, false)),
            Err(e) => {
                // Error classification aligned with delete_file: distinguish "insufficient permissions" from "in use".
                // System-managed directories (such as the old Windows installation Windows.old) should be left to
                // system disk cleanup; forcing deletion would also lose the ability to roll back to the old version.
                #[cfg(windows)]
                let is_sharing_violation = e.raw_os_error() == Some(32); // ERROR_SHARING_VIOLATION
                #[cfg(not(windows))]
                let is_sharing_violation = false;

                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    Err(AppError::new(ErrorCode::PermissionDenied).detail(e.to_string()))
                } else if is_sharing_violation {
                    Err(AppError::new(ErrorCode::TargetInUse).detail(e.to_string()))
                } else {
                    Err(AppError::new(ErrorCode::Internal).detail(e.to_string()))
                }
            }
        }
    }

    /// Get the size of a path
    fn get_path_size(&self, path: &Path) -> u64 {
        if path.is_file() {
            fs::metadata(path).map(|m| m.len()).unwrap_or(0)
        } else if path.is_dir() {
            walkdir::WalkDir::new(path)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        } else {
            0
        }
    }
}

impl Default for DeleteEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path_guard::ensure_deletable;

    /// Junk clean must refuse the system table. The table itself lives in
    /// `path_guard` now (S3a) and is covered there in full; these cases guard the
    /// wiring: the engine really calls it with the junk-clean scope.
    fn refused(path: &str) -> bool {
        ensure_deletable(Path::new(path), DeleteScope::JunkClean).is_err()
    }

    #[test]
    fn protected_paths_are_refused() {
        assert!(refused(r"C:\Windows\System32\test.dll"));
        assert!(refused(r"C:\Program Files\App"));
        assert!(refused(r"C:\Windows.old"));
        assert!(refused(r"C:\Windows.old\Windows\System32\old.dll"));
        assert!(refused(r"C:\$Windows.~BT"));
    }

    #[test]
    fn rebuildable_system_caches_pass_through() {
        // Only rebuildable cache subdirectories explicitly listed by the Windows
        // cleanup wizard may pass through system-directory protection.
        assert!(!refused(r"C:\Windows\System32\d3d_cache\shader.bin"));
        assert!(!refused(
            r"C:\ProgramData\Microsoft\Windows Defender\Support\MPLog.log"
        ));
        assert!(refused(
            r"C:\ProgramData\Microsoft\Windows Defender\Quarantine\entry.bin"
        ));
        assert!(!refused(r"C:\Temp\test.tmp"));
    }

    /// C1 regression: the three bypass spellings from the audit must all be blocked.
    #[test]
    fn bypass_spellings_are_blocked() {
        // 1. Forward slashes: previously matched no `\`-based prefix at all
        assert!(refused("C:/Windows/System32/foo.dll"));
        assert!(refused("C:/Program Files/App"));
        assert!(refused("C:/"));

        // 2. Extended-length prefix: previously kept `\\?\` in the string, so every prefix missed
        assert!(refused(r"\\?\C:\Windows\System32\foo.dll"));
        assert!(refused(r"\\?\C:\"));

        // 3. Relative components: rejected outright before any check or deletion,
        //    because the prefix checks match the literal while the OS resolves `..`.
        assert!(crate::path_guard::validate_deletable(Path::new(
            r"C:\Temp\..\Windows\System32\config"
        ))
        .is_err());
    }

    /// C1 regression: the allowed-scope check is a hard error, not a warning.
    #[test]
    fn out_of_scope_paths_are_rejected() {
        // A user document outside every allowed scope must be rejected by
        // delete_single_file, not deleted with a warning like before the fix.
        assert!(refused(r"C:\Users\Test\Documents\report.docx"));

        // In-scope junk paths still pass, so normal cleanup is unaffected.
        assert!(!refused(r"C:\Users\Test\AppData\Local\Temp\x.tmp"));
        assert!(!refused("C:/Windows/Temp/install.log"));
    }
}
