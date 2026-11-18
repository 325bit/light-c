// ============================================================================
// Social app path adapters
// ============================================================================
//
// Each adapter only discovers the app's data directory; actual traversal and classification is handled uniformly by core.rs.
// This avoids duplicating the scan, dedup, and risk-ranking logic when enhancing a single app's paths.
// ============================================================================

use super::core::{FileCategory, SocialAppPath, SocialScanner};
use log::info;
use std::path::{Path, PathBuf};

impl SocialScanner {
    /// Scan one nt_qq data directory (account subdirectories underneath, plus a global directory).
    ///
    /// The caller may pass the data root (`...\nt_qq`) or a custom path pointing directly at an account directory;
    /// the two forms, "account directory / data root", are handled separately here.
    pub(super) fn collect_ntqq_paths(&self, base: &Path, paths: &mut Vec<SocialAppPath>) {
        if Self::looks_like_ntqq_account(base) {
            self.scan_ntqq_account_directory(base, paths);
            // An account directory may itself contain another nt_qq level (common in the old architecture's account directories)
            let nested = base.join("nt_qq");
            if nested.is_dir() {
                self.scan_ntqq_directory(&nested, paths);
            }
            return;
        }

        self.scan_ntqq_directory(base, paths);
    }

    /// NTQQ account directory signature: marked directly by nt_data / nt_db / nt_msg, no longer relying on the directory name.
    pub(super) fn looks_like_ntqq_account(path: &Path) -> bool {
        ["nt_data", "nt_db", "nt_msg"]
            .iter()
            .any(|name| path.join(name).is_dir())
    }

    /// Scan the NTQQ account directory structure (one account subdirectory under nt_qq).
    pub(super) fn scan_ntqq_account_directory(
        &self,
        account_dir: &Path,
        paths: &mut Vec<SocialAppPath>,
    ) {
        // nt_data directory - media files
        let nt_data = account_dir.join("nt_data");
        if nt_data.is_dir() {
            // Images and videos (LOW)
            for media_dir in &["Pic", "Video", "Ptt"] {
                let dir = nt_data.join(media_dir);
                Self::add_scan_path(paths, "NTQQ", dir, FileCategory::ImageVideo, false);
            }

            // Files (MEDIUM)
            let file_dir = nt_data.join("File");
            Self::add_scan_path(paths, "NTQQ", file_dir, FileCategory::FileTransfer, false);

            // Emoji, avatars, and other business caches (NONE)
            self.add_named_subdirectories(
                &nt_data,
                &[
                    "Emoji",
                    "PokeFace",
                    "Skin",
                    "ntnnModel",
                    "onlineStatus",
                    "msf",
                ],
                "NTQQ",
                FileCategory::TempCache,
                false,
                paths,
            );

            // Runtime logs (NONE)
            self.add_named_subdirectories(
                &nt_data,
                &["Log", "Login", "mmkv", "search"],
                "NTQQ",
                FileCategory::TempCache,
                false,
                paths,
            );
        }

        // --------------------------------------------------------
        // nt_msg directory (message database) (CRITICAL)
        // Contains .db files:
        //   - nt_msg.db      - main message database
        //   - nt_msg.db-wal  - WAL log
        //   - nt_msg.db-shm  - shared memory
        // --------------------------------------------------------
        let nt_msg = account_dir.join("nt_msg");
        Self::add_scan_path(paths, "NTQQ", nt_msg, FileCategory::ChatDatabase, false);

        // nt_db directory (user databases) (CRITICAL)
        let nt_db = account_dir.join("nt_db");
        Self::add_scan_path(paths, "NTQQ", nt_db, FileCategory::ChatDatabase, false);

        // nt_temp directory (temporary data) (NONE)
        let nt_temp = account_dir.join("nt_temp");
        Self::add_scan_path(paths, "NTQQ", nt_temp, FileCategory::TempCache, false);
    }

    pub(super) fn detect_ntqq_paths(&self) -> Option<Vec<SocialAppPath>> {
        let mut paths = Vec::new();

        // NTQQ main directory
        let ntqq_base = PathBuf::from(format!("{}\\Tencent\\QQ\\nt_qq", self.local_appdata));
        if ntqq_base.is_dir() {
            info!("发现NTQQ目录: {}", ntqq_base.display());
            self.collect_ntqq_paths(&ntqq_base, &mut paths);
        }

        // Try reading the NTQQ custom path from the registry
        if let Some(registry_path) = self.read_ntqq_registry_path() {
            let custom_base = PathBuf::from(&registry_path);
            if custom_base.is_dir() && custom_base != ntqq_base {
                info!("发现NTQQ自定义目录: {}", custom_base.display());
                self.collect_ntqq_paths(&custom_base, &mut paths);
            }
        }

        // nt_qq inferred from the install directory: QQ often puts data in the install directory or a custom location beside it,
        // e.g. D:\software\qq\install + D:\software\qq\data\nt_qq
        for candidate in self.ntqq_install_adjacent_paths() {
            if candidate != ntqq_base {
                info!("发现NTQQ安装目录旁数据目录: {}", candidate.display());
                self.collect_ntqq_paths(&candidate, &mut paths);
            }
        }

        // NTQQ global cache
        let qq_root = PathBuf::from(&self.local_appdata).join("Tencent\\QQ");
        self.add_named_subdirectories(
            &qq_root,
            &["Cache", "Temp", "Logs", "WebCache"],
            "NTQQ",
            FileCategory::TempCache,
            false,
            &mut paths,
        );

        if paths.is_empty() {
            None
        } else {
            Some(paths)
        }
    }

    /// Scan the NTQQ data root: account directories underneath, plus a global directory.
    pub(super) fn scan_ntqq_directory(&self, base: &Path, paths: &mut Vec<SocialAppPath>) {
        let Ok(entries) = std::fs::read_dir(base) else {
            return;
        };

        for entry in entries.filter_map(|e| e.ok()) {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }

            let sub_dir = entry.path();
            let dir_name = sub_dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            if dir_name.starts_with('.') {
                continue;
            }

            // global is not an account directory, but it contains Emoji, stickers, temporary files, and other cleanable content;
            // skipping it entirely before meant machines with only the new QQ could not find any QQ data.
            if dir_name.eq_ignore_ascii_case("global") {
                info!("  NTQQ全局目录: {}", dir_name);
                self.scan_ntqq_account_directory(&sub_dir, paths);
                continue;
            }

            // Only process subdirectories with a clear account-directory signature, avoiding treating miscellaneous directories under global as accounts
            if Self::looks_like_ntqq_account(&sub_dir) {
                info!("  NTQQ用户目录: {}", dir_name);
                self.scan_ntqq_account_directory(&sub_dir, paths);
                continue;
            }

            // A subdirectory may also contain a nested nt_qq: both common forms must be recognized
            //   ① The data root itself is used as base (`<data root>\nt_qq\...`)
            //   ② New-format data placed inside an old-architecture account directory (`<data root>\<account>\nt_qq\nt_data\Pic`)
            // Neither of these has an nt_data signature and would be missed by the check above -
            // this is the main reason user reports said "the default path finds nothing".
            let nested = if dir_name.eq_ignore_ascii_case("nt_qq") {
                Some(sub_dir.clone())
            } else {
                let candidate = sub_dir.join("nt_qq");
                candidate.is_dir().then_some(candidate)
            };
            if let Some(nested_dir) = nested {
                info!("  NTQQ嵌套数据: {}", nested_dir.display());
                // Use collect rather than scan: a nested directory may directly contain nt_data
                self.collect_ntqq_paths(&nested_dir, paths);
            }
        }
    }

    /// Infer the nt_qq data directory location from the QQ install directory.
    ///
    /// New QQ allows data in a custom location, which may not be recorded in the registry;
    /// however the install directory is usually known (registry QQNT / QQ Install / InstallPath),
    /// and the data directory is mostly in or beside the install directory.
    #[cfg(target_os = "windows")]
    pub(super) fn ntqq_install_adjacent_paths(&self) -> Vec<PathBuf> {
        use winreg::enums::*;
        use winreg::RegKey;

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let mut bases: Vec<PathBuf> = Vec::new();

        // Collect install directory candidates
        for key_path in ["Software\\Tencent\\QQNT", "Software\\Tencent\\QQ"] {
            let Ok(key) = hkcu.open_subkey(key_path) else {
                continue;
            };
            for value_name in ["Install", "InstallPath", "InstallDir"] {
                let Ok(raw) = key.get_value::<String, _>(value_name) else {
                    continue;
                };
                let raw = raw.trim().trim_matches('"').to_string();
                if raw.is_empty() {
                    continue;
                }
                let install_dir = PathBuf::from(&raw);
                if !install_dir.is_dir() {
                    continue;
                }
                if let Some(parent) = install_dir.parent() {
                    bases.push(parent.to_path_buf());
                }
                bases.push(install_dir);
            }
        }

        // When the registry has no install info (custom install directory, portable build), fall back to filesystem inference,
        // e.g. D:\software\qq\install -> D:\software\qq\data\nt_qq
        for install_dir in self.find_qq_install_directories() {
            if let Some(parent) = install_dir.parent() {
                bases.push(parent.to_path_buf());
            }
            bases.push(install_dir);
        }

        // Look for nt_qq / data\nt_qq in the install directory and its siblings
        let mut found = Vec::new();
        for base in bases {
            for relative in ["nt_qq", "data\\nt_qq"] {
                let candidate = base.join(relative);
                if candidate.is_dir() && !found.contains(&candidate) {
                    found.push(candidate.clone());
                    // A `data\nt_qq` hit means data is QQ's data root,
                    // and old-architecture account directories (`<account>\nt_qq\nt_data\...`) hang off it;
                    // scanning only data\nt_qq can never reach those account directories, so include the root too.
                    if relative == "data\\nt_qq" {
                        if let Some(root) = candidate.parent() {
                            let root = root.to_path_buf();
                            if !found.contains(&root) {
                                found.push(root);
                            }
                        }
                    }
                }
            }
        }

        found
    }

    #[cfg(not(target_os = "windows"))]
    pub(super) fn ntqq_install_adjacent_paths(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    /// Read the NTQQ custom path from the registry
    #[cfg(target_os = "windows")]
    pub(super) fn read_ntqq_registry_path(&self) -> Option<String> {
        use winreg::enums::*;
        use winreg::RegKey;

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);

        // Try reading the NTQQ registry path; different versions write different value names, so try each in turn
        if let Ok(qq_key) = hkcu.open_subkey("Software\\Tencent\\QQNT") {
            for value_name in ["PersonalPath", "DataPath", "PersonalFolder"] {
                if let Ok(path) = qq_key.get_value::<String, _>(value_name) {
                    if !path.is_empty() && Path::new(&path).exists() {
                        return Some(path);
                    }
                }
            }
        }

        None
    }

    #[cfg(not(target_os = "windows"))]
    pub(super) fn read_ntqq_registry_path(&self) -> Option<String> {
        None
    }
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static FIXTURE_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    fn fixture(name: &str) -> PathBuf {
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lightc-ntqq-{}-{}-{}",
            name,
            std::process::id(),
            sequence
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建测试目录失败");
        dir
    }

    #[test]
    fn recognizes_ntqq_account_by_structure() {
        let root = fixture("account-shape");
        let account = root.join("123456789");
        std::fs::create_dir_all(account.join("nt_data/Pic")).unwrap();
        assert!(SocialScanner::looks_like_ntqq_account(&account));

        // Only an empty directory like global does not count as an account
        let global = root.join("global");
        std::fs::create_dir_all(&global).unwrap();
        assert!(!SocialScanner::looks_like_ntqq_account(&global));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scans_ntqq_global_directory() {
        // On a machine with only the new QQ installed, nt_qq may contain only global;
        // the old implementation skipped global, so such machines found no QQ cache at all.
        let root = fixture("global-scan");
        let global = root.join("global");
        std::fs::create_dir_all(global.join("nt_data/Emoji")).unwrap();
        std::fs::create_dir_all(global.join("nt_data/Log")).unwrap();
        std::fs::create_dir_all(global.join("nt_temp")).unwrap();
        std::fs::create_dir_all(global.join("nt_db")).unwrap();
        // Account directories must still be parsed as usual
        std::fs::create_dir_all(root.join("123456789/nt_data/Pic")).unwrap();

        let scanner = SocialScanner::new();
        let mut paths = Vec::new();
        scanner.scan_ntqq_directory(&root, &mut paths);

        let found = |suffix: &str, category: FileCategory| {
            paths
                .iter()
                .any(|p| p.path.ends_with(suffix) && p.category == category)
        };

        assert!(
            found("Emoji", FileCategory::TempCache),
            "global/nt_data/Emoji 应被收录"
        );
        assert!(
            found("Log", FileCategory::TempCache),
            "global/nt_data/Log 应被收录"
        );
        assert!(
            found("nt_temp", FileCategory::TempCache),
            "global/nt_temp 应被收录"
        );
        assert!(
            found("nt_db", FileCategory::ChatDatabase),
            "global/nt_db 应为聊天记录"
        );
        assert!(
            found("Pic", FileCategory::ImageVideo),
            "账号目录媒体仍应被收录"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scans_nt_qq_nested_inside_legacy_account() {
        // Measured structure (D:\software\qq\data):
        //   <account>\Image\...                  old architecture
        //   <account>\nt_qq\nt_data\Pic\...      <- new-format data written inside the old account directory
        //   nt_qq\global\nt_data\Emoji\...    new-format global
        // Under `nt_qq` there is **directly nt_data**, not an account directory;
        // the old implementation set the scan root at `data\nt_qq` while account directories hung off `data`,
        // so neither could reach this tree - the symptom was that new QQ only found a little temporary cache under global.
        let root = fixture("legacy-account-nested");
        let account = root.join("1378813463");
        std::fs::create_dir_all(account.join("Image")).unwrap();
        std::fs::create_dir_all(account.join("nt_qq/nt_data/Pic")).unwrap();
        std::fs::create_dir_all(account.join("nt_qq/nt_data/Video")).unwrap();
        std::fs::create_dir_all(root.join("nt_qq/global/nt_data/Emoji")).unwrap();

        let scanner = SocialScanner::new();

        // Entry 1: the data root is used as base (data inferred beside the install directory)
        let mut from_root = Vec::new();
        scanner.scan_ntqq_directory(&root, &mut from_root);
        let found = |paths: &Vec<SocialAppPath>, suffix: &str, category: FileCategory| {
            paths
                .iter()
                .any(|p| p.path.ends_with(suffix) && p.category == category)
        };
        assert!(
            found(&from_root, "Pic", FileCategory::ImageVideo),
            "账号内 nt_qq/nt_data/Pic 应被收录"
        );
        assert!(
            found(&from_root, "Video", FileCategory::ImageVideo),
            "账号内 nt_qq/nt_data/Video 应被收录"
        );
        assert!(
            found(&from_root, "Emoji", FileCategory::TempCache),
            "数据根下 nt_qq/global 应被收录"
        );

        // Entry 2: the old scanner passes `<account>\nt_qq` directly to collect (new-format data inside an account directory)
        let mut from_nested = Vec::new();
        scanner.collect_ntqq_paths(&account.join("nt_qq"), &mut from_nested);
        assert!(
            found(&from_nested, "Pic", FileCategory::ImageVideo),
            "collect 直接指向 <账号>\\nt_qq 时也要认出 nt_data"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn collects_paths_when_root_points_at_account() {
        // A custom path may point directly at an account directory rather than the nt_qq root
        let root = fixture("account-root");
        std::fs::create_dir_all(root.join("nt_data/Pic")).unwrap();

        let scanner = SocialScanner::new();
        let mut paths = Vec::new();
        scanner.collect_ntqq_paths(&root, &mut paths);

        assert!(paths.iter().any(|p| p.path.ends_with("Pic")));

        let _ = std::fs::remove_dir_all(&root);
    }
}

// ========================================================================
