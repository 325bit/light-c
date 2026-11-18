// ============================================================================
// Social software path adapter
// ============================================================================
//
// Each adapter is only responsible for discovering the software's data directory; actual traversal and classification is handled uniformly by core.rs.
// This avoids duplicating scan, dedup and risk-grading logic when enhancing a single software's path.
// ============================================================================

use super::core::{FileCategory, SocialAppPath, SocialScanner};
use log::info;
use std::path::PathBuf;

impl SocialScanner {
    // DingTalk path detection
    // ========================================================================

    /// Detect the DingTalk path
    pub(super) fn detect_dingtalk_paths(&self) -> Option<Vec<SocialAppPath>> {
        let mut paths = Vec::new();

        let base_paths = [
            PathBuf::from(&self.appdata).join("DingTalk"),
            PathBuf::from(&self.local_appdata).join("DingTalk"),
            PathBuf::from(&self.appdata).join("Alibaba\\DingTalk"),
        ];

        for dingtalk_base in base_paths {
            if dingtalk_base.is_dir() {
                info!("发现钉钉目录: {}", dingtalk_base.display());
            }

            if let Ok(entries) = std::fs::read_dir(&dingtalk_base) {
                for entry in entries.filter_map(|e| e.ok()) {
                    if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        continue;
                    }

                    let sub_dir = entry.path();

                    // Images and videos
                    for dir_name in &["Image", "Video"] {
                        let dir = sub_dir.join(dir_name);
                        if dir.exists() {
                            Self::add_scan_path(
                                &mut paths,
                                "钉钉",
                                dir,
                                FileCategory::ImageVideo,
                                false,
                            );
                        }
                    }

                    // Files
                    let file_dir = sub_dir.join("File");
                    if file_dir.exists() {
                        Self::add_scan_path(
                            &mut paths,
                            "钉钉",
                            file_dir,
                            FileCategory::FileTransfer,
                            false,
                        );
                    }

                    // Cache and storage
                    for dir_name in &["Cache", "storage", "cache"] {
                        let dir = sub_dir.join(dir_name);
                        if dir.exists() {
                            Self::add_scan_path(
                                &mut paths,
                                "钉钉",
                                dir,
                                FileCategory::TempCache,
                                false,
                            );
                        }
                    }

                    // --------------------------------------------------------
                    // DingTalk database (CRITICAL)
                    // Characteristic: Database directory
                    // --------------------------------------------------------
                    let db_dir = sub_dir.join("Database");
                    if db_dir.exists() {
                        Self::add_scan_path(
                            &mut paths,
                            "钉钉",
                            db_dir,
                            FileCategory::ChatDatabase,
                            false,
                        );
                    }
                }
            }
        }

        // DingTalk documents directory
        let dingtalk_docs = PathBuf::from(format!("{}\\DingTalk", self.documents_dir));
        Self::add_scan_path(
            &mut paths,
            "钉钉",
            dingtalk_docs,
            FileCategory::FileTransfer,
            false,
        );

        if paths.is_empty() {
            None
        } else {
            Some(paths)
        }
    }
}

// ========================================================================
