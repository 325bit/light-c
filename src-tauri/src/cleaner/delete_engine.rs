// ============================================================================
// Delete engine - core deletion logic implementation
// Multiple layers of safety protection to ensure important files are never deleted by mistake
// ============================================================================

use log::{debug, error, info, warn};
use std::fs;
use std::path::Path;

use crate::scanner::{DeleteResult, FileInfo};

// ============================================================================
// Safety protection config — referenced uniformly from the shared module
// ============================================================================

use super::safety_constants::{
    is_rebuildable_system_cache_path, PROTECTED_EXTENSIONS_IN_WINDOWS, PROTECTED_FILES,
    PROTECTED_PATH_PREFIXES,
};

/// Delete engine
pub struct DeleteEngine {
    /// Whether to use safe delete mode (move to recycle bin instead of direct deletion)
    safe_mode: bool,
    /// Whether to skip files that are in use
    skip_in_use: bool,
}

impl DeleteEngine {
    /// Create a new delete engine
    pub fn new() -> Self {
        DeleteEngine {
            safe_mode: false,  // direct deletion by default
            skip_in_use: true, // skip files in use by default
        }
    }

    /// Set safe mode
    pub fn with_safe_mode(mut self, enabled: bool) -> Self {
        self.safe_mode = enabled;
        self
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
    fn delete_single_file(&self, path: &str, size: u64) -> Result<(u64, bool), String> {
        let file_path = Path::new(path);

        // Check whether the path exists
        if !file_path.exists() {
            return Err("文件不存在".to_string());
        }

        // Safety check layer 1: check whether it is a protected path
        if self.is_protected_path(file_path) {
            return Err("系统保护路径，禁止删除".to_string());
        }

        // Safety check layer 2: check whether it is within the allowed deletion range
        if !self.is_in_allowed_scope(file_path) {
            warn!("路径不在允许删除范围内: {}", path);
        }

        // Try to delete
        if file_path.is_dir() {
            self.delete_directory(file_path, size)
        } else {
            self.delete_file(file_path, size)
        }
    }

    /// Delete a file, returning (size, whether marked for deletion on reboot)
    fn delete_file(&self, path: &Path, size: u64) -> Result<(u64, bool), String> {
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
                    Err(format!("权限不足: {}", e))
                } else {
                    // Detect a sharing violation (error code 32, ERROR_SHARING_VIOLATION),
                    // when the file is in use by another process it cannot be deleted directly, so mark it for deletion on reboot
                    #[cfg(windows)]
                    let is_sharing_violation = e.raw_os_error() == Some(32); // ERROR_SHARING_VIOLATION
                    #[cfg(not(windows))]
                    let is_sharing_violation = false;

                    if is_sharing_violation && self.is_in_allowed_scope(path) {
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
                        Err(format!("文件被系统占用: {}", e))
                    } else {
                        Err(format!("删除失败: {}", e))
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
    fn delete_directory(&self, path: &Path, size: u64) -> Result<(u64, bool), String> {
        match fs::remove_dir_all(path) {
            Ok(_) => Ok((size, false)),
            Err(e) => {
                // Error classification aligned with delete_file: distinguish "insufficient permissions" from "in use".
                // For insufficient permissions, add an actionable hint — directories managed by the system (such as the old Windows installation
                // Windows.old) should be left to system disk cleanup; forcing deletion would also lose the ability to roll back to the old version.
                #[cfg(windows)]
                let is_sharing_violation = e.raw_os_error() == Some(32); // ERROR_SHARING_VIOLATION
                #[cfg(not(windows))]
                let is_sharing_violation = false;

                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    Err(format!("权限不足: {}（系统托管的目录请改用系统磁盘清理）", e))
                } else if is_sharing_violation {
                    Err(format!("目录内有文件被占用: {}", e))
                } else {
                    Err(format!("删除目录失败: {}", e))
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

    /// Check whether it is a protected path (multiple safety checks)
    fn is_protected_path(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy().to_lowercase();

        // Layer 1: check the path prefix
        for protected in PROTECTED_PATH_PREFIXES {
            if path_str.starts_with(protected) && !is_rebuildable_system_cache_path(&path_str) {
                error!("安全拦截: 尝试删除受保护路径 {}", path_str);
                return true;
            }
        }

        // Layer 2: check the file name
        if let Some(file_name) = path.file_name() {
            let name = file_name.to_string_lossy().to_lowercase();
            for protected in PROTECTED_FILES {
                if name == *protected {
                    error!("安全拦截: 尝试删除系统关键文件 {}", name);
                    return true;
                }
            }
        }

        // Layer 3: protect specific extensions under the Windows directory
        if path_str.contains("\\windows\\") {
            if let Some(ext) = path.extension() {
                let ext_str = ext.to_string_lossy().to_lowercase();
                if PROTECTED_EXTENSIONS_IN_WINDOWS.contains(&ext_str.as_str()) {
                    error!("安全拦截: 尝试删除Windows目录下的系统文件 {}", path_str);
                    return true;
                }
            }
        }

        // Layer 4: check whether it is the root of a user profile folder
        let user_critical_paths = [
            "\\appdata\\local",
            "\\appdata\\roaming",
            "\\documents",
            "\\desktop",
            "\\downloads",
        ];
        for critical in &user_critical_paths {
            // Only protect the root directory, not subdirectories
            if path_str.ends_with(critical) {
                error!("安全拦截: 尝试删除用户关键目录 {}", path_str);
                return true;
            }
        }

        // Layer 5: check whether it is a drive root directory
        if path_str.len() <= 3 && path_str.ends_with("\\") {
            error!("安全拦截: 尝试删除驱动器根目录 {}", path_str);
            return true;
        }

        false
    }

    /// Validate whether the path is within the allowed deletion range
    fn is_in_allowed_scope(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy().to_lowercase();

        // Path ranges allowed for deletion
        let allowed_scopes = [
            "\\temp",
            "\\tmp",
            "\\cache",
            "\\caches",
            "\\temporary",
            "\\appdata\\local\\temp",
            "\\appdata\\local\\microsoft\\windows\\temporary",
            "\\appdata\\local\\microsoft\\windows\\inetcache",
            "\\appdata\\local\\microsoft\\windows\\explorer", // thumbnail cache
            "\\windows\\temp",
            "\\windows\\prefetch",
            "\\windows\\softwaredistribution\\download",
            "\\windows\\softwaredistribution\\deliveryoptimization",
            "\\deliveryoptimization\\",
            "\\programdata\\microsoft\\windows defender\\localcopy",
            "\\programdata\\microsoft\\windows defender\\support",
            "\\d3d_cache",
            "\\$recycle.bin", // recycle bin contents
            "\\.log",
            "\\.tmp",
            "\\.bak",
            "\\crash",
            "\\dumps",
        ];

        for scope in &allowed_scopes {
            if path_str.contains(scope) {
                return true;
            }
        }

        // Check whether the file extension is in the allowed list
        if let Some(ext) = path.extension() {
            let ext_str = ext.to_string_lossy().to_lowercase();
            let allowed_exts = ["tmp", "temp", "log", "bak", "old", "dmp", "etl"];
            if allowed_exts.contains(&ext_str.as_str()) {
                return true;
            }
        }

        false
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

    #[test]
    fn test_protected_paths() {
        let engine = DeleteEngine::new();

        assert!(engine.is_protected_path(Path::new("C:\\Windows\\System32\\test.dll")));
        assert!(engine.is_protected_path(Path::new("C:\\Program Files\\App")));
        // Only rebuildable cache subdirectories explicitly listed by the Windows cleanup wizard may pass through system-directory protection.
        assert!(
            !engine.is_protected_path(Path::new("C:\\Windows\\System32\\d3d_cache\\shader.bin"))
        );
        assert!(!engine.is_protected_path(Path::new(
            "C:\\ProgramData\\Microsoft\\Windows Defender\\Support\\MPLog.log"
        )));
        assert!(engine.is_protected_path(Path::new(
            "C:\\ProgramData\\Microsoft\\Windows Defender\\Quarantine\\entry.bin"
        )));
        assert!(!engine.is_protected_path(Path::new("C:\\Temp\\test.tmp")));
        // The old Windows installation is system-managed: it cannot be deleted inside the app, and a mid-way failure leaves a "half-deleted" state,
        // so it must be blocked here (the scan side is unaffected and still lists the size normally).
        assert!(engine.is_protected_path(Path::new("C:\\Windows.old")));
        assert!(engine.is_protected_path(Path::new(
            "C:\\Windows.old\\Windows\\System32\\old.dll"
        )));
        assert!(engine.is_protected_path(Path::new("C:\\$Windows.~BT")));
    }
}
