// ============================================================================
// Social software path adapters
// ============================================================================
//
// Each adapter is only responsible for discovering a piece of software's data directory; the actual traversal and classification is handled uniformly by core.rs.
// This avoids duplicating the scan, deduplication, and risk-grading logic when enhancing a single software's paths.
// ============================================================================

use super::core::{FileCategory, SocialAppPath, SocialScanner};
use log::info;
use std::path::{Path, PathBuf};

impl SocialScanner {
    // QQ path detection
    // ========================================================================

    /// Detects legacy QQ paths
    ///
    /// Path trace priority:
    /// 1. Registry HKCU\Software\Tencent\QQ\PersonalFolder
    /// 2. "Tencent Files" under the default Documents directory
    /// 3. Full-disk search for the "Tencent Files" folder
    pub(super) fn detect_qq_paths(&self) -> Option<Vec<SocialAppPath>> {
        let mut paths = Vec::new();
        let mut found_base_paths = Vec::new();
        let registry_path = self.read_qq_registry_path();
        let is_custom = registry_path.is_some();

        // Try reading QQ's custom path from the registry
        if let Some(registry_path) = registry_path {
            info!("QQ 注册表路径: {}", registry_path);
            Self::add_candidate(&mut found_base_paths, PathBuf::from(registry_path));
        }

        // Also keep the Documents directory candidate; the user may have installed multiple versions or migrated data.
        Self::add_candidate(
            &mut found_base_paths,
            PathBuf::from(format!("{}\\Tencent Files", self.documents_dir)),
        );
        if self.documents_dir != self.default_documents {
            Self::add_candidate(
                &mut found_base_paths,
                PathBuf::from(format!("{}\\Tencent Files", self.default_documents)),
            );
        }

        // The data directory beside the install directory: a custom data location isn't always written to the registry,
        // e.g. D:\software\qq\install + D:\software\qq\data\<QQ number>
        for candidate in self.qq_install_adjacent_data_paths() {
            info!("QQ 安装目录旁数据目录: {}", candidate.display());
            Self::add_candidate(&mut found_base_paths, candidate);
        }

        // Full-disk search fallback (unconditional)
        // ⚠️ Same as on the WeChat side (Issue #89): a built-in candidate directory occupies a slot merely by "existing",
        // and after the user moves data to another drive the old directory at the original location often remains (possibly empty) —
        // that causes the search to be skipped entirely, so data on D:\ is never scanned.
        if let Some(search_paths) = self.search_qq_files_on_all_drives() {
            for path in search_paths {
                Self::add_candidate(&mut found_base_paths, path);
            }
        }

        for base_path in found_base_paths {
            info!("发现QQ目录: {}", base_path.display());

            if Self::looks_like_qq_account(&base_path) {
                self.scan_qq_account_directory(&base_path, is_custom, &mut paths);
            } else if let Ok(entries) = std::fs::read_dir(&base_path) {
                for entry in entries.filter_map(|e| e.ok()) {
                    if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        continue;
                    }

                    let user_dir = entry.path();
                    let user_name = user_dir
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();

                    // QQ numbers are usually all digits; skip non-user directories
                    if Self::is_qq_non_account_name(&user_name) {
                        continue;
                    }

                    // An account directory is characterized by "containing subdirectories like Msg/Image/FileRecv";
                    // skip those that don't, to avoid scanning miscellaneous directories other than All Users.
                    if !Self::looks_like_qq_account(&user_dir) {
                        continue;
                    }

                    info!("  QQ用户: {}", user_name);
                    self.scan_qq_account_directory(&user_dir, is_custom, &mut paths);
                }
            }
        }

        self.add_named_subdirectories(
            &PathBuf::from(&self.appdata).join("Tencent\\QQ"),
            &["Temp", "Cache", "Logs"],
            "QQ",
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

    /// Non-account directories to skip under the QQ data root.
    fn is_qq_non_account_name(dir_name: &str) -> bool {
        dir_name.eq_ignore_ascii_case("all users")
            || dir_name.eq_ignore_ascii_case("global")
            || dir_name.starts_with('.')
    }

    /// Infers the data directory from a legacy QQ installation directory.
    ///
    /// Legacy QQ puts data in `data` (custom location) or `Tencent Files` alongside the install directory;
    /// the registry may have no record, so this serves as a supplementary source.
    #[cfg(target_os = "windows")]
    pub(super) fn qq_install_adjacent_data_paths(&self) -> Vec<PathBuf> {
        use winreg::enums::*;
        use winreg::RegKey;

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let mut bases: Vec<PathBuf> = Vec::new();

        if let Ok(qq_key) = hkcu.open_subkey("Software\\Tencent\\QQ") {
            for value_name in ["Install", "InstallPath", "InstallDir"] {
                let Ok(raw) = qq_key.get_value::<String, _>(value_name) else {
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
                // QQ's registry Install often points to the exe; take its containing directory
                let base_dir = if install_dir.is_file() {
                    install_dir.parent().map(|p| p.to_path_buf())
                } else {
                    Some(install_dir)
                };
                if let Some(base_dir) = base_dir {
                    if let Some(parent) = base_dir.parent() {
                        bases.push(parent.to_path_buf());
                    }
                    bases.push(base_dir);
                }
            }
        }

        // When the registry has no install info (custom install directory, portable version), fall back to filesystem inference
        for install_dir in self.find_qq_install_directories() {
            if let Some(parent) = install_dir.parent() {
                bases.push(parent.to_path_buf());
            }
            bases.push(install_dir);
        }

        let mut found = Vec::new();
        for base in bases {
            for relative in ["data", "Tencent Files", "data\\Tencent Files"] {
                let candidate = base.join(relative);
                // Only accept directories with account-directory characteristics, to avoid treating an unrelated data directory as QQ data
                if candidate.is_dir() && Self::looks_like_qq_data_root(&candidate) {
                    if !found.contains(&candidate) {
                        found.push(candidate);
                    }
                }
            }
        }

        found
    }

    /// Finds the QQ install directory by name under common install roots.
    ///
    /// Covers the case "no install path in the registry": the user installed QQ in a custom directory (e.g. D:\software\qq\install),
    /// or only installed the new NTQQ (C:\Program Files\Tencent\QQNT).
    pub(super) fn find_qq_install_directories(&self) -> Vec<PathBuf> {
        // Install-root candidates: program directories on each drive + common custom software directories
        let install_roots: [&str; 6] = [
            "Program Files\\Tencent",
            "Program Files (x86)\\Tencent",
            "Program Files",
            "Program Files (x86)",
            "software",
            "Programs",
        ];

        let mut install_dirs = Vec::new();
        for drive in &self.available_drives {
            for root in install_roots {
                let base = PathBuf::from(drive).join(root);
                if !base.is_dir() {
                    continue;
                }

                // Only one level: the install directory itself, or the first-level subdirectory under an install root
                if Self::looks_like_qq_install_dir(&base) {
                    install_dirs.push(base.clone());
                }

                let Ok(entries) = std::fs::read_dir(&base) else {
                    continue;
                };
                for entry in entries.filter_map(|e| e.ok()) {
                    if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        continue;
                    }
                    let candidate = entry.path();
                    if Self::looks_like_qq_install_dir(&candidate) {
                        install_dirs.push(candidate);
                    }
                }
            }
        }

        install_dirs.sort();
        install_dirs.dedup();
        install_dirs
    }

    /// Determines whether a directory looks like a QQ install directory: its name contains QQ, or it contains QQ-family executables.
    pub(super) fn looks_like_qq_install_dir(path: &Path) -> bool {
        let name_matches = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.to_ascii_lowercase().contains("qq"));

        if name_matches {
            return true;
        }

        // Directories whose name doesn't contain QQ (custom naming like D:\software\qq\install) are judged by executables
        ["QQ.exe", "QQScLauncher.exe", "QQUninst.exe", "PCQQ2021.exe"]
            .iter()
            .any(|exe| path.join(exe).is_file())
    }

    #[cfg(not(target_os = "windows"))]
    pub(super) fn qq_install_adjacent_data_paths(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    /// Determines whether a directory is a QQ data root: it directly contains account directories or nt_qq.
    pub(super) fn looks_like_qq_data_root(path: &Path) -> bool {
        if path.join("nt_qq").is_dir() {
            return true;
        }

        let Ok(entries) = std::fs::read_dir(path) else {
            return false;
        };

        entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .any(|entry| Self::looks_like_qq_account(&entry.path()))
    }

    /// Scans a single user directory of legacy QQ, covering messages, media, received files, and cache.
    fn scan_qq_account_directory(
        &self,
        account_path: &Path,
        is_custom: bool,
        paths: &mut Vec<SocialAppPath>,
    ) {
        self.add_named_subdirectories(
            account_path,
            &["Msg", "Msg2", "Database", "History", "MsgDb"],
            "QQ",
            FileCategory::ChatDatabase,
            is_custom,
            paths,
        );
        self.add_named_subdirectories(
            account_path,
            &["Image", "Video", "Audio", "Ptt", "QQImage"],
            "QQ",
            FileCategory::ImageVideo,
            is_custom,
            paths,
        );
        self.add_named_subdirectories(
            account_path,
            &["FileRecv", "File", "MsgAttach"],
            "QQ",
            FileCategory::FileTransfer,
            is_custom,
            paths,
        );
        self.add_named_subdirectories(
            account_path,
            &["Cache", "Temp", "Thumb", "Log", "Logs"],
            "QQ",
            FileCategory::TempCache,
            is_custom,
            paths,
        );

        // On machines with mixed old/new architecture, nt_qq can also appear inside old account directories (new data landing in old paths),
        // so we parse those here too, to avoid missing the same data just because of where it lives.
        // ⚠️ Must go through collect_ntqq_paths, not scan_ntqq_directory:
        // such nt_qq directories contain **nt_data directly** (`<account>\nt_qq\nt_data\Pic`),
        // while scan_ntqq_directory only traverses subdirectories and won't treat "itself" as an account directory —
        // so the entire subtree's images and videos would be skipped (users reported new QQ only scanning a little temporary cache).
        let nested_ntqq = account_path.join("nt_qq");
        if nested_ntqq.is_dir() {
            info!("  QQ账号内 NTQQ 数据: {}", nested_ntqq.display());
            self.collect_ntqq_paths(&nested_ntqq, paths);
        }
    }

    fn looks_like_qq_account(path: &Path) -> bool {
        // 🚨 Must include nt_qq: new QQ installed in the default path (Documents\Tencent Files) or a custom data directory
        // has only `nt_qq` in the account directory, without the old markers Msg / Image / FileRecv.
        // Without this, the whole `<account>\nt_qq\nt_data\Pic` fails even the "is it an account directory" check,
        // and the nested parsing never runs — the user report "nothing scanned in the default path" comes from here.
        ["Msg", "Msg2", "FileRecv", "Image", "Database", "nt_qq"]
            .iter()
            .any(|name| path.join(name).is_dir())
    }

    /// Reads QQ's custom path from the registry
    #[cfg(target_os = "windows")]
    pub(super) fn read_qq_registry_path(&self) -> Option<String> {
        use winreg::enums::*;
        use winreg::RegKey;

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);

        // Try reading QQ's registry path
        if let Ok(qq_key) = hkcu.open_subkey("Software\\Tencent\\QQ") {
            // Try PersonalFolder
            if let Ok(path) = qq_key.get_value::<String, _>("PersonalFolder") {
                if !path.is_empty() && Path::new(&path).exists() {
                    return Some(path);
                }
            }
            // Try Install
            if let Ok(path) = qq_key.get_value::<String, _>("Install") {
                let tencent_files = PathBuf::from(&path)
                    .parent()
                    .map(|p| p.join("Tencent Files"))
                    .filter(|p| p.exists())
                    .map(|p| p.to_string_lossy().to_string());
                if tencent_files.is_some() {
                    return tencent_files;
                }
            }
        }

        None
    }

    #[cfg(not(target_os = "windows"))]
    pub(super) fn read_qq_registry_path(&self) -> Option<String> {
        None
    }

    /// Full-disk search for the Tencent Files folder
    ///
    /// Issue #87: after the user changed the chat-record location to `D:\QQ\Tencent Files` it wasn't scanned —
    /// the reason was that candidate base directories only included Users / Documents / Data, while `QQ` is a self-created
    /// directory at the drive root and wasn't in any of the three. Now drive roots are also searched (skipping system directories),
    /// and each top-level directory under the root gets a bounded drill-down rather than checking only one level.
    pub(super) fn search_qq_files_on_all_drives(&self) -> Option<Vec<PathBuf>> {
        let mut found_paths = Vec::new();

        // Includes "" (drive root): custom save locations basically always look like this — `D:\QQ\Tencent Files`.
        // System directories are skipped below, so Windows / Program Files won't be scanned.
        let common_locations = [
            "",
            "Users",
            "Documents",
            "Data",
            "software",
            "Programs",
            "downloads",
        ];

        for drive in &self.available_drives {
            for location in &common_locations {
                let search_base = PathBuf::from(drive).join(location);
                if !search_base.is_dir() {
                    continue;
                }
                // Use the cache: the WeChat-side fallback search already read this batch of directories, so we hit directly here.
                let subdirectories = self.read_subdirectories_cached(&search_base);

                for path in subdirectories {
                    // Don't drill into system directories at the drive root: chat data can't be there, it only wastes time.
                    if location.is_empty() && Self::is_system_top_level_dir(&path) {
                        continue;
                    }

                    // Case one: the directory itself is named Tencent Files
                    if path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_ascii_lowercase() == "tencent files")
                        .unwrap_or(false)
                    {
                        info!("全盘搜索发现QQ目录: {}", path.display());
                        found_paths.push(path.clone());
                        continue;
                    }

                    // Case two: there's one more level below the directory before Tencent Files.
                    // Reuse the bounded lookup from the WeChat side (with a built-in cap on directory checks).
                    //
                    // Use depth 2 instead of 1: many users put chat data in a **self-created category directory**,
                    // e.g. issue #97's `D:\ChatRecords\QQ\Tencent Files` —
                    // counting from the drive root that's three levels "category → software → data"; a one-level drill-down can never reach it.
                    // Only one extra readdir, with a 20000-directory cap as fallback, so big directories won't hang.
                    if let Some(found) = self.find_directory_by_name(&path, "Tencent Files", 2) {
                        info!("全盘搜索发现QQ目录: {}", found.display());
                        found_paths.push(found);
                    }
                }
            }
        }

        if found_paths.is_empty() {
            None
        } else {
            Some(found_paths)
        }
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

    /// A machine with only new QQ installed under the default path (Documents\Tencent Files):
    /// the account directory has nothing but nt_qq; the whole chain must be able to dig out Pic.
    #[test]
    fn scans_account_holding_only_ntqq_data() {
        let root = std::env::temp_dir().join(format!(
            "lightc-qq-default-path-{}-{}",
            std::process::id(),
            FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let account = root.join("123456789");
        std::fs::create_dir_all(account.join("nt_qq/nt_data/Pic")).unwrap();
        std::fs::create_dir_all(account.join("nt_qq/nt_data/Video")).unwrap();

        let scanner = SocialScanner::new();
        let mut paths = Vec::new();
        // detect_qq_paths calls this function for each account under Tencent Files
        scanner.scan_qq_account_directory(&account, false, &mut paths);

        let found = |suffix: &str| paths.iter().any(|p| p.path.ends_with(suffix));
        assert!(found("Pic"), "默认路径下只有 nt_qq 的账号也要扫到图片");
        assert!(found("Video"), "默认路径下只有 nt_qq 的账号也要扫到视频");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// When new QQ is installed in the default path (Documents\Tencent Files) or a custom data directory,
    /// the account directory has only nt_qq, without the old markers Msg / Image.
    /// The old characteristics would judge such a directory as "not an account", skipping the whole `<account>\nt_qq\nt_data\Pic`.
    #[test]
    fn recognizes_account_that_only_contains_nt_qq() {
        let root = std::env::temp_dir().join(format!(
            "lightc-qq-only-ntqq-{}-{}",
            std::process::id(),
            FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let account = root.join("123456789");
        std::fs::create_dir_all(account.join("nt_qq/nt_data/Pic")).unwrap();

        assert!(
            SocialScanner::looks_like_qq_account(&account),
            "只有 nt_qq 的账号目录也必须被认出来"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Guard for Issue #87: system directories at the drive root must be skipped, all other directories must not be skipped.
    /// The user's self-created directories (`D:\QQ`, `D:\weixin`) are in "all others" —
    /// if this is relaxed to "never drill into the root directory", #87 immediately reappears.
    #[test]
    fn skips_only_system_directories_at_drive_root() {
        for system in [
            r"C:\Windows",
            r"C:\Program Files",
            r"C:\Program Files (x86)",
            r"C:\ProgramData",
            r"D:\$Recycle.Bin",
            r"D:\System Volume Information",
        ] {
            assert!(
                SocialScanner::is_system_top_level_dir(Path::new(system)),
                "{} 应被识别为系统目录",
                system
            );
        }

        for custom in [r"D:\QQ", r"D:\weixin", r"D:\Tencent Files", r"D:\my data"] {
            assert!(
                !SocialScanner::is_system_top_level_dir(Path::new(custom)),
                "{} 不是系统目录，跳过它就会漏扫（Issue #87）",
                custom
            );
        }
    }

    fn fixture(name: &str) -> PathBuf {
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lightc-qq-{}-{}-{}",
            name,
            std::process::id(),
            sequence
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建测试目录失败");
        dir
    }

    #[test]
    fn recognizes_qq_install_directory() {
        let root = fixture("install-shape");

        // Directory name contains QQ: recognize directly
        let named = root.join("QQ");
        std::fs::create_dir_all(&named).unwrap();
        assert!(SocialScanner::looks_like_qq_install_dir(&named));

        // Directory name doesn't contain QQ (custom naming): recognize via executables
        let custom = root.join("install");
        std::fs::create_dir_all(&custom).unwrap();
        assert!(!SocialScanner::looks_like_qq_install_dir(&custom));
        std::fs::write(custom.join("QQUninst.exe"), b"stub").unwrap();
        assert!(SocialScanner::looks_like_qq_install_dir(&custom));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn recognizes_qq_data_root_by_account_directories() {
        let root = fixture("data-shape");

        // Contains a numeric account directory (old architecture)
        let legacy = root.join("legacy");
        std::fs::create_dir_all(legacy.join("1378813463/Image")).unwrap();
        assert!(SocialScanner::looks_like_qq_data_root(&legacy));

        // Contains nt_qq (new architecture)
        let nt = root.join("nt");
        std::fs::create_dir_all(nt.join("nt_qq")).unwrap();
        assert!(SocialScanner::looks_like_qq_data_root(&nt));

        // A plain directory shouldn't be treated as a QQ data root, to avoid scanning unrelated data directories by mistake
        let unrelated = root.join("unrelated");
        std::fs::create_dir_all(unrelated.join("notes")).unwrap();
        assert!(!SocialScanner::looks_like_qq_data_root(&unrelated));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn collects_legacy_account_media_paths() {
        let root = fixture("legacy-account");
        let account = root.join("1378813463");
        std::fs::create_dir_all(account.join("Image")).unwrap();
        std::fs::create_dir_all(account.join("FileRecv")).unwrap();
        std::fs::create_dir_all(account.join("Msg2")).unwrap();
        // Nested nt_qq inside old account directories must also be parsed
        std::fs::create_dir_all(account.join("nt_qq/123/nt_data/Pic")).unwrap();

        let scanner = SocialScanner::new();
        let mut paths = Vec::new();
        scanner.scan_qq_account_directory(&account, false, &mut paths);

        let found = |suffix: &str, category: FileCategory| {
            paths
                .iter()
                .any(|p| p.path.ends_with(suffix) && p.category == category)
        };

        assert!(found("Image", FileCategory::ImageVideo));
        assert!(found("FileRecv", FileCategory::FileTransfer));
        assert!(found("Msg2", FileCategory::ChatDatabase));
        assert!(
            found("Pic", FileCategory::ImageVideo),
            "嵌套 nt_qq 数据应被解析"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
