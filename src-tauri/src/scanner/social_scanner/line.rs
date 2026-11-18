// ============================================================================
// LINE Windows data path adapter
// ============================================================================
//
// LINE Desktop may use Roaming, Local, or a Microsoft Store package directory. Here we only
// select explicit cache/media directories; the message database stays Critical to avoid deleting chat data.
// ============================================================================

use super::core::{FileCategory, SocialAppPath, SocialScanner};
use std::path::PathBuf;

impl SocialScanner {
    /// Find LINE Desktop's cache and database directories.
    pub(super) fn detect_line_paths(&self) -> Option<Vec<SocialAppPath>> {
        let mut paths = Vec::new();
        let bases = [
            PathBuf::from(&self.appdata).join("LINE"),
            PathBuf::from(&self.appdata).join(r"LINE Corporation\LINE"),
            PathBuf::from(&self.local_appdata).join("LINE"),
        ];

        for base in bases {
            self.add_line_paths(&base, &mut paths);
        }

        for package in self.find_package_directories(&["linecorp", "line"]) {
            self.add_line_paths(&package.join("LocalCache"), &mut paths);
            self.add_line_paths(&package.join("LocalState"), &mut paths);
            self.add_line_paths(&package.join(r"LocalCache\Roaming\LINE"), &mut paths);
        }

        (!paths.is_empty()).then_some(paths)
    }

    fn add_line_paths(&self, base: &std::path::Path, paths: &mut Vec<SocialAppPath>) {
        Self::add_scan_path(
            paths,
            "LINE",
            base.join("Cache"),
            FileCategory::TempCache,
            false,
        );
        Self::add_scan_path(
            paths,
            "LINE",
            base.join("cache"),
            FileCategory::TempCache,
            false,
        );
        Self::add_scan_path(
            paths,
            "LINE",
            base.join("Image"),
            FileCategory::ImageVideo,
            false,
        );
        Self::add_scan_path(
            paths,
            "LINE",
            base.join("Video"),
            FileCategory::ImageVideo,
            false,
        );
        Self::add_scan_path(
            paths,
            "LINE",
            base.join("File"),
            FileCategory::FileTransfer,
            false,
        );
        Self::add_scan_path(
            paths,
            "LINE",
            base.join("Database"),
            FileCategory::ChatDatabase,
            false,
        );
        Self::add_scan_path(
            paths,
            "LINE",
            base.join("db"),
            FileCategory::ChatDatabase,
            false,
        );
    }
}
