// ============================================================================
// Social app path adapter
// ============================================================================
//
// Each adapter only discovers its software's data directory; actual traversal and classification are handled uniformly by core.rs.
// This avoids duplicating scan, dedup and risk-grading logic when enhancing a single app's paths.
// ============================================================================

use super::core::{FileCategory, SocialAppPath, SocialScanner};
use log::info;
use std::path::{Path, PathBuf};

impl SocialScanner {
    // Feishu path detection
    // ========================================================================

    /// Detect Feishu paths
    pub(super) fn detect_feishu_paths(&self) -> Option<Vec<SocialAppPath>> {
        let mut paths = Vec::new();

        // Feishu main directory
        let feishu_base = PathBuf::from(format!("{}\\feishu", self.appdata));
        if feishu_base.exists() {
            info!("发现飞书目录: {}", feishu_base.display());
            self.scan_feishu_directory(&feishu_base, "飞书", &mut paths);
        }

        // LarkShell directory (new Feishu version)
        let larkshell_base = PathBuf::from(format!("{}\\LarkShell", self.appdata));
        if larkshell_base.exists() {
            info!("发现LarkShell目录: {}", larkshell_base.display());

            // sdk_storage directory
            let sdk_storage = larkshell_base.join("sdk_storage");
            Self::add_scan_path(
                &mut paths,
                "飞书",
                sdk_storage,
                FileCategory::TempCache,
                false,
            );

            // file_storage directory
            let file_storage = larkshell_base.join("file_storage");
            Self::add_scan_path(
                &mut paths,
                "飞书",
                file_storage,
                FileCategory::FileTransfer,
                false,
            );
        }

        // Lark (international Feishu)
        let lark_base = PathBuf::from(format!("{}\\Lark", self.appdata));
        if lark_base.exists() {
            info!("发现Lark目录: {}", lark_base.display());
            self.scan_feishu_directory(&lark_base, "Lark", &mut paths);
        }

        // Feishu documents directory
        let feishu_docs = PathBuf::from(format!("{}\\Feishu", self.documents_dir));
        Self::add_scan_path(
            &mut paths,
            "飞书",
            feishu_docs,
            FileCategory::FileTransfer,
            false,
        );

        if paths.is_empty() {
            None
        } else {
            Some(paths)
        }
    }

    /// Scan the Feishu directory structure
    pub(super) fn scan_feishu_directory(
        &self,
        base: &Path,
        app_name: &str,
        paths: &mut Vec<SocialAppPath>,
    ) {
        if let Ok(entries) = std::fs::read_dir(base) {
            for entry in entries.filter_map(|e| e.ok()) {
                if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }

                let sub_dir = entry.path();

                // Images
                let image_dir = sub_dir.join("Image");
                Self::add_scan_path(paths, app_name, image_dir, FileCategory::ImageVideo, false);

                // Files
                let file_dir = sub_dir.join("File");
                Self::add_scan_path(paths, app_name, file_dir, FileCategory::FileTransfer, false);

                // Cache
                let cache_dir = sub_dir.join("Cache");
                Self::add_scan_path(paths, app_name, cache_dir, FileCategory::TempCache, false);

                // sdk_storage
                let sdk_storage = sub_dir.join("sdk_storage");
                Self::add_scan_path(paths, app_name, sdk_storage, FileCategory::TempCache, false);

                // file_storage
                let file_storage = sub_dir.join("file_storage");
                Self::add_scan_path(
                    paths,
                    app_name,
                    file_storage,
                    FileCategory::FileTransfer,
                    false,
                );
            }
        }
    }
}

// ========================================================================
