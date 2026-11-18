// ============================================================================
// Common path utilities for social app scanning
// ============================================================================
//
// Adapters only describe "where data might be"; here we uniformly handle existence, path dedup, and
// Windows case/slash differences, to avoid counting the same file twice or submitting it for deletion twice.
// ============================================================================

use super::core::{FileCategory, SocialAppPath, SocialScanner};
use std::path::{Path, PathBuf};

impl SocialScanner {
    /// Append a directory candidate and deduplicate; when the candidate does not exist, do no IO recursion, to avoid slowing normal scans with leftover-fix work.
    pub(super) fn add_candidate(candidates: &mut Vec<PathBuf>, path: PathBuf) {
        if !path.is_dir() {
            return;
        }

        let path_key = Self::normalize_path_key(&path);
        if !candidates
            .iter()
            .any(|candidate| Self::normalize_path_key(candidate) == path_key)
        {
            candidates.push(path);
        }
    }

    /// Add existing directories to the scan paths, deduplicating by app, category, and normalized path.
    pub(super) fn add_scan_path(
        paths: &mut Vec<SocialAppPath>,
        app_name: &str,
        path: PathBuf,
        category: FileCategory,
        is_custom_path: bool,
    ) {
        if !path.is_dir() {
            return;
        }

        let path_key = Self::normalize_path_key(&path);
        let duplicated = paths.iter().any(|item| {
            item.app_name == app_name
                && item.category == category
                && Self::normalize_path_key(&item.path) == path_key
        });

        if !duplicated {
            paths.push(SocialAppPath {
                app_name: app_name.to_string(),
                path,
                category,
                is_custom_path,
            });
        }
    }

    /// Add multiple fixed-name subdirectories, suitable for Electron/WebView clients.
    pub(super) fn add_named_subdirectories(
        &self,
        base_path: &Path,
        names: &[&str],
        app_name: &str,
        category: FileCategory,
        is_custom_path: bool,
        paths: &mut Vec<SocialAppPath>,
    ) {
        for name in names {
            Self::add_scan_path(
                paths,
                app_name,
                base_path.join(name),
                category,
                is_custom_path,
            );
        }
    }

    /// Find Windows Store package directories matching given keywords.
    pub(super) fn find_package_directories(&self, markers: &[&str]) -> Vec<PathBuf> {
        let packages_dir = PathBuf::from(&self.local_appdata).join("Packages");
        let Ok(entries) = std::fs::read_dir(packages_dir) else {
            return Vec::new();
        };

        entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let is_dir = entry.file_type().ok()?.is_dir();
                if !is_dir {
                    return None;
                }

                let name = entry.file_name().to_string_lossy().to_lowercase();
                markers
                    .iter()
                    .any(|marker| name.contains(&marker.to_lowercase()))
                    .then_some(entry.path())
            })
            .collect()
    }

    /// Uniformly generate a path key, compatible with Windows case-insensitivity and both separators.
    ///
    /// This path is taken once per file (on the order of 100,000), so the normal path allows only one heap allocation:
    /// `to_string_lossy` does not allocate for valid UTF-8, lowercasing allocates once;
    /// forward slashes only appear when a caller hand-writes a path for compatibility, and only a hit costs one extra replacement.
    pub(super) fn normalize_path_key(path: &Path) -> String {
        let mut key = path.to_string_lossy().to_lowercase();
        if key.contains('/') {
            key = key.replace('/', "\\");
        }
        // A trailing separator does not affect file path comparison; strip it uniformly
        while key.ends_with('\\') {
            key.pop();
        }
        key
    }

    /// Determine whether a directory under a drive root is built into the system.
    ///
    /// When drilling down from a drive root to find social app data directories, these must be skipped first: they cannot hold chat data,
    /// yet contain thousands of subdirectories, so descending into them is a pure waste of time (especially C:\Windows).
    /// Conversely, **all top-level directories except these must be descended into** - user-customized
    /// `D:\QQ\Tencent Files` and `D:\weixin\xwechat_files` hide in there (Issue #87).
    pub(super) fn is_system_top_level_dir(path: &Path) -> bool {
        const SYSTEM_DIR_NAMES: [&str; 10] = [
            "windows",
            "program files",
            "program files (x86)",
            "programdata",
            "$recycle.bin",
            "system volume information",
            "recovery",
            "perflogs",
            "msocache",
            "config.msi",
        ];
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                SYSTEM_DIR_NAMES
                    .iter()
                    .any(|system| name.eq_ignore_ascii_case(system))
            })
    }
}
