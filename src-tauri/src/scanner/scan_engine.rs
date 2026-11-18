// ============================================================================
// Scan engine - core scanning logic implementation
// Uses parallel scanning to optimize performance
// ============================================================================

use log::{debug, info};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;
use walkdir::WalkDir;

use super::{CategoryScanResult, FileInfo, JunkCategory, ScanResult};
use crate::cleaner::safety_constants::is_rebuildable_system_cache_path;

/// Scan engine
pub struct ScanEngine {
    /// List of categories to scan
    categories: Vec<JunkCategory>,
    /// Maximum scan depth
    max_depth: usize,
}

impl ScanEngine {
    /// Create a new scan engine
    pub fn new() -> Self {
        ScanEngine {
            categories: JunkCategory::all(),
            max_depth: 10,
        }
    }

    /// Set the categories to scan
    pub fn with_categories(mut self, categories: Vec<JunkCategory>) -> Self {
        self.categories = categories;
        self
    }

    /// Set the maximum scan depth
    pub fn with_max_depth(mut self, depth: usize) -> Self {
        self.max_depth = depth;
        self
    }

    /// Run a full scan (all categories in parallel)
    pub fn scan(&self) -> ScanResult {
        let start_time = Instant::now();
        let categories = self.categories.clone();
        let max_depth = self.max_depth;

        info!("开始并行扫描，共 {} 个分类", categories.len());

        // Scan all categories in parallel using threads
        let results: Arc<Mutex<Vec<CategoryScanResult>>> = Arc::new(Mutex::new(Vec::new()));
        let mut handles = vec![];

        for category in categories {
            let results_clone = Arc::clone(&results);
            let handle = thread::spawn(move || {
                let engine = ScanEngine {
                    categories: vec![category.clone()],
                    max_depth,
                };
                let category_result = engine.scan_category(&category);

                info!(
                    "分类 {} 扫描完成: {} 个文件, {}",
                    category.display_name(),
                    category_result.file_count,
                    category_result.human_readable_total_size()
                );

                let mut results = results_clone.lock().unwrap();
                results.push(category_result);
            });
            handles.push(handle);
        }

        // Wait for all threads to finish
        for handle in handles {
            let _ = handle.join();
        }

        // Aggregate the results
        let mut result = ScanResult::new();
        let category_results = results.lock().unwrap();
        for category_result in category_results.iter() {
            result.add_category_result(category_result.clone());
        }

        let duration = start_time.elapsed();
        result.set_duration(duration.as_millis() as u64);

        info!(
            "扫描完成，共发现 {} 个文件，总大小 {}，耗时 {}ms",
            result.total_file_count,
            result.human_readable_total_size(),
            result.scan_duration_ms
        );

        result
    }

    /// Scan a single category
    pub fn scan_category(&self, category: &JunkCategory) -> CategoryScanResult {
        let mut result = CategoryScanResult::new(category.clone());

        // The Recycle Bin's physical directory contains multi-user SIDs and Shell metadata; a dedicated scanner is required to stay consistent with Explorer.
        if matches!(category, JunkCategory::RecycleBin) {
            super::recycle_bin::scan_current_user(category, &mut result);
            result.sort_files_by_size_desc();
            return result;
        }

        let scan_paths = category.get_scan_paths();
        let patterns = category.get_file_patterns();

        // Collect all resolved paths, dedupe, then scan
        // e.g. %TEMP% and %TMP% may point to the same directory, avoid scanning twice
        let mut unique_paths: HashSet<std::path::PathBuf> = HashSet::new();
        let mut resolved_list: Vec<std::path::PathBuf> = Vec::new();

        for scan_path in scan_paths {
            for resolved_path in scan_path.resolve_all() {
                // Try to canonicalize the path to eliminate differences from symlinks, casing, etc.
                let canonical = match std::fs::canonicalize(&resolved_path) {
                    Ok(p) => p,
                    Err(_) => resolved_path.clone(), // if canonicalization fails, use a clone of the original path
                };
                if unique_paths.insert(canonical.clone()) {
                    resolved_list.push(canonical);
                } else {
                    debug!("跳过重复路径: {:?}", resolved_path);
                }
            }
        }

        for resolved_path in &resolved_list {
            debug!("扫描路径: {:?}", resolved_path);
            self.scan_path(resolved_path, category, &patterns, &mut result);
        }

        // Parallel threads and directory traversal randomize file order; sort uniformly by size descending,
        // so users see the biggest space hogs first within each category.
        result.sort_files_by_size_desc();
        result
    }

    /// Scan a specific path
    fn scan_path(
        &self,
        path: &Path,
        category: &JunkCategory,
        patterns: &[&str],
        result: &mut CategoryScanResult,
    ) {
        // Check whether the path exists
        if !path.exists() {
            debug!("路径不存在: {:?}", path);
            return;
        }

        // If it's a file, handle it directly
        if path.is_file() {
            if let Some(file_info) = self.get_file_info(path, category) {
                result.add_file(file_info);
            }
            return;
        }

        // Traverse the directory, counting files only; skip directory entries to avoid double counting with files
        let walker = WalkDir::new(path)
            .max_depth(self.max_depth)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                !self.is_system_protected(e.path())
                    && !self.is_persistent_app_profile_path(e.path())
            });

        for entry in walker.filter_map(|e| e.ok()) {
            let entry_path = entry.path();

            // Skip the root directory itself
            if entry_path == path {
                continue;
            }

            // Only process files, skip directories (avoids duplicate counting between calculate_dir_size and per-file stats)
            if !entry.file_type().is_file() {
                continue;
            }

            // Check whether it matches the patterns
            if !self.matches_patterns(entry_path, patterns) {
                continue;
            }

            // Second line of defense: even if new scan roots are added later, never treat WebView/Chromium persistent Profile data as junk.
            if self.is_persistent_app_profile_path(entry_path) {
                continue;
            }

            // Get file information
            if let Some(file_info) = self.get_file_info(entry_path, category) {
                result.add_file(file_info);
            }
        }
    }

    /// Get file information (files only; directories are already skipped in scan_path)
    fn get_file_info(&self, path: &Path, category: &JunkCategory) -> Option<FileInfo> {
        let metadata = match fs::metadata(path) {
            Ok(m) => m,
            Err(e) => {
                debug!("无法获取文件元数据 {:?}: {}", path, e);
                return None;
            }
        };

        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "未知".to_string());

        let size = metadata.len();

        let modified_time = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        Some(FileInfo::new(
            path.to_string_lossy().to_string(),
            name,
            size,
            modified_time,
            false, // always a file, directories never reach this function
            category.clone(),
        ))
    }

    /// Check whether a file name matches the patterns
    fn matches_patterns(&self, path: &Path, patterns: &[&str]) -> bool {
        // If the patterns contain "*", match all files
        if patterns.contains(&"*") {
            return true;
        }

        let file_name = match path.file_name() {
            Some(name) => name.to_string_lossy().to_lowercase(),
            None => return false,
        };

        for pattern in patterns {
            if self.matches_glob(&file_name, pattern) {
                return true;
            }
        }

        false
    }

    /// Simple glob pattern matching
    fn matches_glob(&self, name: &str, pattern: &str) -> bool {
        // Windows file-name matching is case-insensitive; name and pattern must use the same normalization.
        let name = name.to_lowercase();
        let pattern = pattern.to_lowercase();

        if pattern == "*" {
            return true;
        }

        // Treat a single asterisk as a string of any length, supporting combinations of prefix, suffix and middle wildcards.
        let starts_with_wildcard = pattern.starts_with('*');
        let ends_with_wildcard = pattern.ends_with('*');
        let parts: Vec<&str> = pattern.split('*').filter(|part| !part.is_empty()).collect();
        if parts.is_empty() {
            return true;
        }

        let mut cursor = 0usize;
        for (index, part) in parts.iter().enumerate() {
            let is_first = index == 0;
            let is_last = index == parts.len() - 1;

            if is_first && !starts_with_wildcard {
                if !name[cursor..].starts_with(part) {
                    return false;
                }
                cursor += part.len();
                continue;
            }

            let Some(relative_position) = name[cursor..].find(part) else {
                return false;
            };
            let match_start = cursor + relative_position;
            if is_last && !ends_with_wildcard && !name[match_start..].ends_with(part) {
                return false;
            }
            cursor = match_start + part.len();
        }

        true
    }

    /// Check whether a path is system-protected (should not be scanned)
    fn is_system_protected(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy().to_lowercase();

        // Protect critical system directories
        let protected_paths = [
            "system32",
            "syswow64",
            "winsxs",
            "assembly",
            "\\windows\\servicing",
            "\\windows\\installer",
            "\\windows\\logs\\cbs",
            "\\program files",
            "\\program files (x86)",
            "\\programdata\\microsoft\\windows defender",
            "\\users\\default",
        ];

        for protected in &protected_paths {
            // Under Defender and System32, only cache subdirectories explicitly marked by the cleanup wizard are scannable; parent dirs stay protected.
            if path_str.contains(protected) && !is_rebuildable_system_cache_path(&path_str) {
                return true;
            }
        }

        // Protect critical system file extensions
        if let Some(ext) = path.extension() {
            let ext_str = ext.to_string_lossy().to_lowercase();
            let protected_exts = ["sys", "dll", "exe", "msi", "cat", "mum", "manifest"];
            // Only protect these extensions under the Windows directory
            if path_str.contains("\\windows\\") && protected_exts.contains(&ext_str.as_str()) {
                return true;
            }
        }

        false
    }

    /// Check whether a path is persistent WebView/Chromium app Profile data.
    ///
    /// Note:
    /// MSIX/Tauri/Electron/WebView2 apps often keep user state under LocalCache, e.g. Claude's
    /// EBWebView\Default. The IndexedDB, Local Storage, and various SQLite/LevelDB files in there
    /// are not pure caches; deleting them loses conversation lists, app preferences, or indexes.
    fn is_persistent_app_profile_path(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy().to_lowercase();
        if !path_str.contains("\\appdata\\local\\packages\\") && !path_str.contains("\\ebwebview\\")
        {
            return false;
        }

        let persistent_markers = [
            "\\ebwebview\\default\\",
            "\\local storage\\",
            "\\indexeddb\\",
            "\\session storage\\",
            "\\databases\\",
            "\\file system\\",
            "\\shared_proto_db\\",
            "\\shared dictionary\\",
            "\\autofillaimodelcache\\",
            "\\autofillstrikedatabase\\",
            "\\budgetdatabase\\",
            "\\browsingtopicssitedata",
            "\\browsingtopicsstate",
            "\\bookmarkmergedsurfaceordering",
        ];

        persistent_markers
            .iter()
            .any(|marker| path_str.contains(marker))
    }
}

// Implement Send and Sync for parallel scanning
unsafe impl Send for ScanEngine {}
unsafe impl Sync for ScanEngine {}

impl Default for ScanEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glob_matching() {
        let engine = ScanEngine::new();

        assert!(engine.matches_glob("test.log", "*.log"));
        assert!(engine.matches_glob("test.LOG", "*.log"));
        assert!(!engine.matches_glob("test.txt", "*.log"));
        assert!(engine.matches_glob("thumbcache_256.db", "thumbcache_*.db"));
    }

    #[test]
    fn test_webview_profile_data_is_protected() {
        let engine = ScanEngine::new();

        assert!(engine.is_persistent_app_profile_path(Path::new(
            r"C:\Users\PC\AppData\Local\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Local\app.formatif.desktop\EBWebView\Default\IndexedDB\index.leveldb\LOG"
        )));
        assert!(engine.is_persistent_app_profile_path(Path::new(
            r"C:\Users\PC\AppData\Local\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Local\app.formatif.desktop\EBWebView\Default\BrowsingTopicsSiteData"
        )));
        assert!(!engine.is_persistent_app_profile_path(Path::new(
            r"C:\Users\PC\AppData\Local\Google\Chrome\User Data\Default\Code Cache\js\cache"
        )));
    }

    #[test]
    fn test_rebuildable_system_cache_exception_is_narrow() {
        let engine = ScanEngine::new();

        // The exception only covers explicit cache directories; it must not open up all of System32 or the Defender root.
        assert!(!engine.is_system_protected(Path::new(r"C:\Windows\System32\d3d_cache\shader.bin")));
        assert!(!engine.is_system_protected(Path::new(
            r"C:\ProgramData\Microsoft\Windows Defender\Support\MPLog.log"
        )));
        assert!(engine.is_system_protected(Path::new(r"C:\Windows\System32\kernel32.dll")));
        assert!(engine.is_system_protected(Path::new(
            r"C:\ProgramData\Microsoft\Windows Defender\Quarantine\entry.bin"
        )));
    }

    #[test]
    fn test_category_files_are_sorted_by_size_desc() {
        let mut result = CategoryScanResult::new(JunkCategory::WindowsTemp);
        for (name, size) in [("small.tmp", 10_u64), ("large.tmp", 4096), ("medium.tmp", 512)] {
            result.add_file(FileInfo::new(
                format!(r"C:\Users\Test\AppData\Local\Temp\{}", name),
                name.to_string(),
                size,
                0,
                false,
                JunkCategory::WindowsTemp,
            ));
        }

        result.sort_files_by_size_desc();

        let sizes = result.files.iter().map(|file| file.size).collect::<Vec<_>>();
        assert_eq!(sizes, vec![4096, 512, 10]);
        // Sorting only changes display order, not the category statistics.
        assert_eq!(result.file_count, 3);
        assert_eq!(result.total_size, 4618);
    }
}
