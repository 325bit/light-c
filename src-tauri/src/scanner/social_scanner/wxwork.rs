// ============================================================================
// Social app path adapters
// ============================================================================
//
// Each adapter only discovers the app's data directories; actual traversal and classification are handled uniformly by core.rs.
// This way, enhancing one app's paths avoids duplicating the scan, dedupe and risk-grading logic.
// ============================================================================

use super::core::{FileCategory, SocialAppPath, SocialScanner};
use log::info;
use std::path::PathBuf;

impl SocialScanner {
    // WeCom (Enterprise WeChat) path detection
    // ========================================================================

    /// Detect WeCom paths
    pub(super) fn detect_wxwork_paths(&self) -> Option<Vec<SocialAppPath>> {
        let mut paths = Vec::new();

        // WeCom may store data in the Documents directory or in AppData
        let base_paths = vec![
            PathBuf::from(format!("{}\\WXWork", self.documents_dir)),
            PathBuf::from(format!("{}\\WXWork", self.default_documents)),
            PathBuf::from(format!("{}\\WXWork", self.appdata)), // Roaming
            PathBuf::from(format!("{}\\WXWork", self.local_appdata)), // Local
            PathBuf::from(format!("{}\\Tencent\\WXWork", self.appdata)),
            PathBuf::from(format!("{}\\Tencent\\WXWork", self.local_appdata)),
        ];

        for base_path in base_paths {
            if !base_path.exists() {
                continue;
            }

            info!("发现企业微信目录: {}", base_path.display());

            if let Ok(entries) = std::fs::read_dir(&base_path) {
                for entry in entries.filter_map(|e| e.ok()) {
                    if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        continue;
                    }

                    let user_dir = entry.path();
                    let cache_dir = user_dir.join("Cache");

                    if cache_dir.is_dir() {
                        // Images and videos
                        for dir_name in &["Image", "Video"] {
                            let dir = cache_dir.join(dir_name);
                            if dir.exists() {
                                Self::add_scan_path(
                                    &mut paths,
                                    "企业微信",
                                    dir,
                                    FileCategory::ImageVideo,
                                    false,
                                );
                            }
                        }

                        // Files
                        let file_dir = cache_dir.join("File");
                        if file_dir.exists() {
                            Self::add_scan_path(
                                &mut paths,
                                "企业微信",
                                file_dir,
                                FileCategory::FileTransfer,
                                false,
                            );
                        }
                    }

                    // Newer WeCom puts caches directly under the user directory, so don't rely only on the Cache subdirectory.
                    self.add_named_subdirectories(
                        &user_dir,
                        &["Image", "Video", "Media"],
                        "企业微信",
                        FileCategory::ImageVideo,
                        false,
                        &mut paths,
                    );
                    self.add_named_subdirectories(
                        &user_dir,
                        &["File", "FileStorage", "MsgAttach", "FileRecv"],
                        "企业微信",
                        FileCategory::FileTransfer,
                        false,
                        &mut paths,
                    );
                    self.add_named_subdirectories(
                        &user_dir,
                        &["Temp", "Logs", "Log", "WebView", "CacheData"],
                        "企业微信",
                        FileCategory::TempCache,
                        false,
                        &mut paths,
                    );

                    // Message databases
                    let msg_dir = user_dir.join("Msg");
                    Self::add_scan_path(
                        &mut paths,
                        "企业微信",
                        msg_dir,
                        FileCategory::ChatDatabase,
                        false,
                    );
                }
            }
        }

        for package in self.find_package_directories(&["wxwork", "wecom", "wechatwork"]) {
            self.add_named_subdirectories(
                &package.join("LocalCache"),
                &["Cache", "Code Cache", "GPUCache"],
                "企业微信",
                FileCategory::TempCache,
                false,
                &mut paths,
            );
            self.add_named_subdirectories(
                &package.join("LocalState"),
                &["Cache", "Media", "FileStorage"],
                "企业微信",
                FileCategory::TempCache,
                false,
                &mut paths,
            );
        }

        if paths.is_empty() {
            None
        } else {
            Some(paths)
        }
    }
}

// ========================================================================
