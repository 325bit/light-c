// ============================================================================
// Social app path adapters
// ============================================================================
//
// Each adapter is only responsible for discovering the app's data directory; actual traversal and classification are handled uniformly by core.rs.
// This avoids duplicating scanning, deduplication, and risk-grading logic when enhancing a single app's paths.
// ============================================================================

use super::core::{FileCategory, RegistryPathResult, SocialAppPath, SocialScanner};
use log::{debug, info};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// WeChat data root directory names: legacy 3.x uses "WeChat Files", new 4.x uses "xwechat_files".
/// Searching only by the old name misses new clients entirely, the main source of "WeChat not found" complaints.
const WECHAT_DATA_DIR_NAMES: [&str; 2] = ["WeChat Files", "xwechat_files"];
/// Drives whose data directories have already been searched by exact account name, to avoid re-enumeration.
fn visited_roots(visited: &HashSet<String>, root: &Path) -> bool {
    visited.contains(&SocialScanner::normalize_path_key(root))
}

/// Structure version of WeChat account directories: new and old clients have completely different layouts, so they must be parsed separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WechatAccountKind {
    /// Legacy 3.x: Msg + FileStorage
    Legacy,
    /// New 4.x: db_storage + msg
    V4,
}

impl SocialScanner {
    // WeChat path detection
    // ========================================================================

    /// Detect WeChat paths
    ///
    /// Path tracing priority:
    /// 1. Custom path from the registry
    ///    - Legacy 3.x: HKCU\Software\Tencent\WeChat\FileSavePath
    ///    - New 4.x: HKCU\Software\Tencent\Weixin (FileSavePath / InstallPath at the same level)
    ///    - When the value is "MyDocument:", use the system documents directory
    /// 2. "WeChat Files" / "xwechat_files" under the documents directory
    /// 3. Shallow search of common directories on each drive (including drive roots), as a fallback
    pub(super) fn detect_wechat_paths(&self) -> Option<Vec<SocialAppPath>> {
        let mut paths = Vec::new();
        let mut found_base_paths: Vec<PathBuf> = Vec::new();

        // ----------------------------------------------------------------
        // Step 1: read the custom WeChat path from the registry
        // ----------------------------------------------------------------
        let registry_result = self.read_wechat_registry_path();

        match &registry_result {
            Some(RegistryPathResult::AbsolutePath(abs_path)) => {
                // New WeChat and multi-instance versions may write the account directory directly into the registry, so do not force another level of appending.
                info!("微信注册表路径(绝对): {}", abs_path);
                let path = PathBuf::from(abs_path);
                self.add_wechat_path_candidates(&path, &mut found_base_paths);
            }
            Some(RegistryPathResult::MyDocument) => {
                // The registry returned MyDocument:, use the documents directory
                info!("微信注册表路径: MyDocument: -> {}", self.documents_dir);
                for dir_name in WECHAT_DATA_DIR_NAMES {
                    Self::add_candidate(
                        &mut found_base_paths,
                        PathBuf::from(format!("{}\\{}", self.documents_dir, dir_name)),
                    );
                }
            }
            None => {
                debug!("微信注册表路径未找到，使用默认路径");
            }
        }

        // ----------------------------------------------------------------
        // Step 2: add default paths (if the registry path does not exist or was not found)
        // ----------------------------------------------------------------
        if found_base_paths.is_empty() {
            for dir_name in WECHAT_DATA_DIR_NAMES {
                // Try the documents directory
                Self::add_candidate(
                    &mut found_base_paths,
                    PathBuf::from(format!("{}\\{}", self.documents_dir, dir_name)),
                );

                // Try the default documents directory (if different)
                if self.documents_dir != self.default_documents {
                    Self::add_candidate(
                        &mut found_base_paths,
                        PathBuf::from(format!("{}\\{}", self.default_documents, dir_name)),
                    );
                }
            }
        }

        // ----------------------------------------------------------------
        // Step 3: full-disk search fallback (always executed)
        // ----------------------------------------------------------------
        // ⚠️ Here we **must not** use "all default paths are missing" as a condition again (Issue #89):
        // After WeChat is moved, the old directory often remains at "Documents\xwechat_files" (possibly already empty),
        // and add_candidate only checks directory existence -> the old directory takes the slot -> the whole search is skipped ->
        // the real data on D:\xwechat_files is never scanned.
        // Now search unconditionally; drill down only one level, with controllable cost (one root readdir per drive + one level per non-system directory).
        info!("搜索各磁盘根目录与常见位置下的微信数据目录...");
        if let Some(search_paths) = self.search_wechat_files_on_all_drives() {
            for path in search_paths {
                Self::add_candidate(&mut found_base_paths, path);
            }
        }

        // ----------------------------------------------------------------
        // Step 4: scan all found base paths
        // ----------------------------------------------------------------
        let is_custom = registry_result.is_some();

        for base_path in found_base_paths {
            info!("发现微信目录: {}", base_path.display());
            if Self::looks_like_wechat4_account(&base_path) {
                // The registry/search may point directly to a new-format account directory
                self.scan_wechat4_account_directory(&base_path, is_custom, &mut paths);
            } else {
                self.scan_wechat_base_directory(&base_path, is_custom, &mut paths);
            }
        }

        if paths.is_empty() {
            None
        } else {
            Some(paths)
        }
    }

    /// Normalize the WeChat root, account directories, and multi-instance directories into a single "scannable account root".
    fn add_wechat_path_candidates(&self, path: &Path, candidates: &mut Vec<PathBuf>) {
        if !path.is_dir() {
            return;
        }

        if Self::looks_like_wechat_account(path) || Self::looks_like_wechat4_account(path) {
            Self::add_candidate(candidates, path.to_path_buf());
            return;
        }

        // The legacy data directory is always named WeChat Files; in new 4.x the data directory itself is named xwechat_files,
        // so appending another level would find nothing - first check whether the path itself is the data directory.
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(Self::is_wechat_data_dir_name)
        {
            Self::add_candidate(candidates, path.to_path_buf());
            return;
        }

        for data_dir_name in WECHAT_DATA_DIR_NAMES {
            let nested_root = path.join(data_dir_name);
            if nested_root.is_dir() {
                Self::add_candidate(candidates, nested_root);
                return;
            }
        }

        // The root usually contains several numeric account directories; keep the root so later functions enumerate uniformly.
        Self::add_candidate(candidates, path.to_path_buf());
    }

    /// Whether the name is a WeChat data root directory name (case-insensitive).
    fn is_wechat_data_dir_name(name: &str) -> bool {
        WECHAT_DATA_DIR_NAMES
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
    }

    /// Determine whether the path is already a WeChat account directory, to avoid mistaking an account directory for a collection directory.
    ///
    /// Use only the authoritative legacy 3.x markers, case-sensitively:
    /// if lowercase directories such as `msg` / `file` in a 4.x account were treated as 3.x markers,
    /// new accounts would be routed to the legacy parsing logic, causing whole blocks of data to be missed.
    pub(super) fn looks_like_wechat_account(path: &Path) -> bool {
        ["Msg", "FileStorage", "MicroMsg", "Image"]
            .iter()
            .any(|name| path.join(name).exists())
    }

    /// WeChat account directory structure version.
    fn wechat_account_kind(path: &Path) -> Option<WechatAccountKind> {
        // Check the new version first: db_storage exists only in 4.x and is the most reliable marker for distinguishing the two structures.
        // Note: directory names must be compared individually rather than using path.join(name).is_dir():
        // The Windows filesystem is case-insensitive, so "msg" would match the legacy "Msg" and misclassify a 3.x account as 4.x.
        if Self::contains_exact_directory(path, &["db_storage", "msg"]) {
            return Some(WechatAccountKind::V4);
        }

        if Self::looks_like_wechat_account(path) {
            return Some(WechatAccountKind::Legacy);
        }

        None
    }

    /// Determine whether a subdirectory with an exact (case-sensitive) name exists under a directory.
    fn contains_exact_directory(path: &Path, names: &[&str]) -> bool {
        let Ok(entries) = std::fs::read_dir(path) else {
            return false;
        };

        entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .any(|entry| {
                let entry_name = entry.file_name();
                let Some(entry_name) = entry_name.to_str() else {
                    return false;
                };
                names.iter().any(|name| entry_name == *name)
            })
    }

    /// New WeChat 4.x account directory signature: database in db_storage, messages and media in msg.
    pub(super) fn looks_like_wechat4_account(path: &Path) -> bool {
        matches!(Self::wechat_account_kind(path), Some(WechatAccountKind::V4))
    }

    /// Determine whether a subdirectory is a scannable WeChat account directory, filtering out empty directories from newly created or migrating accounts.
    fn is_wechat_account_dir(path: &Path) -> bool {
        if Self::wechat_account_kind(path).is_some() {
            return true;
        }

        // When account markers are unclear, fall back to judging by directory name, to stay compatible with account directories that have no data yet
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase()
            .starts_with("wxid_")
    }

    /// Scan a single account directory of new WeChat 4.x.
    ///
    /// The directory structure is completely different from 3.x: the chat database moved from Msg to db_storage,
    /// images/videos/files are all collected under msg, plus additional cache directories such as cache / business / temp.
    fn scan_wechat4_account_directory(
        &self,
        account_dir: &Path,
        is_custom: bool,
        paths: &mut Vec<SocialAppPath>,
    ) {
        // Chat record database (CRITICAL)
        let db_storage = account_dir.join("db_storage");
        if db_storage.is_dir() {
            Self::add_scan_path(
                paths,
                "微信",
                db_storage,
                FileCategory::ChatDatabase,
                is_custom,
            );
        }

        // Message directory: file / attach are received files, video holds videos, migrate holds intermediate migration data
        self.add_named_subdirectories(
            &account_dir.join("msg"),
            &["file", "attach"],
            "微信",
            FileCategory::FileTransfer,
            is_custom,
            paths,
        );
        self.add_named_subdirectories(
            &account_dir.join("msg"),
            &["video"],
            "微信",
            FileCategory::ImageVideo,
            is_custom,
            paths,
        );
        self.add_named_subdirectories(
            &account_dir.join("msg"),
            &["migrate"],
            "微信",
            FileCategory::TempCache,
            is_custom,
            paths,
        );

        // In 4.x, `temp\RWTemp` is not a runtime cache but a plaintext copy of recently sent/received chat content
        // (`temp\RWTemp\2026-11\xxx.jpg` and user-saved documents are all here).
        // Classify as "image/video" so the user can see them in that category; document types are claimed by classify_file's
        // extension rule as "transferred files". Must be registered as a deeper path before the parent `temp` directory,
        // so during scanning (deep to shallow by level), child paths get their own category first and parent paths then skip duplicate files.
        Self::add_scan_path(
            paths,
            "微信",
            account_dir.join("temp").join("RWTemp"),
            FileCategory::ImageVideo,
            is_custom,
        );

        // Temporary cache (NONE)
        self.add_named_subdirectories(
            account_dir,
            &["cache", "temp", "apm_record", "resource"],
            "微信",
            FileCategory::TempCache,
            is_custom,
            paths,
        );

        // Business cache for mini programs, favorites, emoji, etc. (NONE)
        self.add_named_subdirectories(
            &account_dir.join("business"),
            &[
                "emoticon",
                "favorite",
                "xweb",
                "xeditor",
                "InputTemp",
                "migrate",
            ],
            "微信",
            FileCategory::TempCache,
            is_custom,
            paths,
        );

        // Moments cache (NONE)
        self.add_named_subdirectories(
            &account_dir.join("business"),
            &["sns"],
            "微信",
            FileCategory::MomentsCache,
            is_custom,
            paths,
        );
    }

    /// Scan the WeChat base directory and extract cache paths for all users
    fn scan_wechat_base_directory(
        &self,
        base_path: &Path,
        is_custom: bool,
        paths: &mut Vec<SocialAppPath>,
    ) {
        if Self::looks_like_wechat4_account(base_path) {
            self.scan_wechat4_account_directory(base_path, is_custom, paths);
            return;
        }

        if Self::looks_like_wechat_account(base_path) {
            self.scan_wechat_account_directory(base_path, is_custom, paths);
            return;
        }

        let Ok(entries) = std::fs::read_dir(base_path) else {
            return;
        };

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

            // Skip system and backup directories: backups are user-initiated and should not be mixed into "cache cleanup"
            if Self::is_wechat_non_account_dir(&user_name) {
                continue;
            }

            // 3.x and 4.x account directories have different structures; dispatch by markers to the appropriate parsing function
            match Self::wechat_account_kind(&user_dir) {
                Some(WechatAccountKind::V4) => {
                    info!("  微信4.x用户: {}", user_name);
                    self.scan_wechat4_account_directory(&user_dir, is_custom, paths);
                }
                Some(WechatAccountKind::Legacy) => {
                    info!("  微信用户: {}", user_name);
                    self.scan_wechat_account_directory(&user_dir, is_custom, paths);
                }
                None => {
                    // The directory name looks like an account but has no data yet, no need to scan
                }
            }
        }
    }

    /// Non-account directories under the WeChat data root: system shared directories and backup directories.
    fn is_wechat_non_account_dir(dir_name: &str) -> bool {
        ["all_users", "backup", "applet", "wmpf", "all users"]
            .iter()
            .any(|name| dir_name.eq_ignore_ascii_case(name))
            || dir_name.starts_with('.')
    }

    /// Scan a single WeChat account directory, compatible with legacy, new, and multi-instance directories.
    fn scan_wechat_account_directory(
        &self,
        user_dir: &Path,
        is_custom: bool,
        paths: &mut Vec<SocialAppPath>,
    ) {
        // --------------------------------------------------------
        // Chat record database (CRITICAL)
        // Marker: .db files under the Msg directory
        // WeChat database structure:
        //   Msg/
        //     MicroMsg.db      - main message database
        //     MediaMSG*.db     - media message database
        //     Multi/           - multi-instance messages
        //       MSG*.db
        // --------------------------------------------------------
        let msg_dir = user_dir.join("Msg");
        if msg_dir.exists() {
            Self::add_scan_path(
                paths,
                "微信",
                msg_dir.clone(),
                FileCategory::ChatDatabase,
                is_custom,
            );

            // Msg\Multi directory (multi-instance messages)
            let multi_dir = msg_dir.join("Multi");
            if multi_dir.exists() {
                Self::add_scan_path(
                    paths,
                    "微信",
                    multi_dir,
                    FileCategory::ChatDatabase,
                    is_custom,
                );
            }
        }

        // FileStorage subdirectories
        let file_storage = user_dir.join("FileStorage");
        if file_storage.exists() {
            // --------------------------------------------------------
            // Images and videos (LOW)
            // Marker: files in the Image, Video directories
            // WeChat encrypted images: .dat files in the Image directory
            // --------------------------------------------------------
            for dir_name in &["Image", "Video"] {
                let dir = file_storage.join(dir_name);
                if dir.exists() {
                    Self::add_scan_path(paths, "微信", dir, FileCategory::ImageVideo, is_custom);
                }
            }

            // --------------------------------------------------------
            // Transferred files (MEDIUM)
            // Marker: File, MsgAttach directories
            // --------------------------------------------------------
            for dir_name in &["File", "MsgAttach"] {
                let dir = file_storage.join(dir_name);
                if dir.exists() {
                    Self::add_scan_path(paths, "微信", dir, FileCategory::FileTransfer, is_custom);
                }
            }

            // --------------------------------------------------------
            // Moments/cache (NONE)
            // Marker: Sns is feed cache; the rest are mostly runtime cache, thumbnails, and mini-program cache.
            // --------------------------------------------------------
            for dir_name in &[
                "Sns",
                "Cache",
                "Temp",
                "General",
                "Thumb",
                "Web",
                "VideoCache",
                "Fav",
                "CustomEmotion",
            ] {
                let dir = file_storage.join(dir_name);
                if dir.exists() {
                    Self::add_scan_path(
                        paths,
                        "微信",
                        dir,
                        if *dir_name == "Sns" {
                            FileCategory::MomentsCache
                        } else {
                            FileCategory::TempCache
                        },
                        is_custom,
                    );
                }
            }
        }

        // New WeChat puts WebView, mini programs, and some temporary cache in the account root; relying on FileStorage alone would miss them.
        for dir_name in &[
            "Sns",
            "Moments",
            "Cache",
            "Temp",
            "Logs",
            "log",
            "WebView",
            "WMPF",
            "Applet",
            "WeChatAppEx",
        ] {
            let dir = user_dir.join(dir_name);
            if dir.exists() {
                Self::add_scan_path(
                    paths,
                    "微信",
                    dir,
                    if *dir_name == "Sns" || *dir_name == "Moments" {
                        FileCategory::MomentsCache
                    } else {
                        FileCategory::TempCache
                    },
                    is_custom,
                );
            }
        }
    }

    /// Full-disk search for WeChat data directories (legacy WeChat Files and new xwechat_files)
    /// Used as a fallback when both registry and default paths fail
    pub(super) fn search_wechat_files_on_all_drives(&self) -> Option<Vec<PathBuf>> {
        let mut found_paths = Vec::new();
        // Record directories whose contents have been enumerated: after the new version's full name-based recursive search, its subdirectories need not be enumerated again
        let mut enumerated_roots: HashSet<String> = HashSet::new();

        // Common storage locations (including drive roots, covering cases where data is placed directly on D: or E:).
        // Deliberately do not use the whole drive root as a deep-search starting point: drilling down by name on C: would enter system directories such as Windows,
        // which is slow and pointless; custom data directories are basically structured as "drive root -> a user-created directory -> WeChat data directory".
        let common_locations = [
            "",
            "Users",
            "Data",
            "Documents",
            "data",
            "software",
            "Programs",
            "downloads",
        ];

        for drive in &self.available_drives {
            for location in &common_locations {
                let search_base = if location.is_empty() {
                    PathBuf::from(drive)
                } else {
                    PathBuf::from(drive).join(location)
                };
                if !search_base.is_dir() {
                    continue;
                }

                // Search only one level deep to avoid long runtimes
                let Ok(entries) = std::fs::read_dir(&search_base) else {
                    continue;
                };

                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }

                    // Condition 1: the directory itself is named WeChat Files / xwechat_files
                    if path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(Self::is_wechat_data_dir_name)
                    {
                        info!("全盘搜索发现: {}", path.display());
                        found_paths.push(path);
                        continue;
                    }

                    // Condition 2: a WeChat data directory name exists inside the directory (including deeper levels).
                    // Skip **system directories** under the drive root (Windows / Program Files / $Recycle.Bin...),
                    // but drill into other top-level directories as usual - this is the fix for Issue #87:
                    // when a user puts WeChat data in a self-created directory like `D:\weixin\xwechat_files`,
                    // the top-level directory name itself is not the data directory name, so only drilling down finds it. Originally this just continued,
                    // effectively ignoring all self-created directories under every drive root.
                    if location.is_empty() && Self::is_system_top_level_dir(&path) {
                        continue;
                    }
                    if visited_roots(&enumerated_roots, &path) {
                        continue;
                    }
                    enumerated_roots.insert(SocialScanner::normalize_path_key(&path));
                    for data_dir_name in WECHAT_DATA_DIR_NAMES {
                        // Use depth 2 (consistent with the QQ side): users often put data in self-created category directories,
                        // e.g., issue #97's `D:\chat-records\WeChat\xwechat_files` -
                        // counted from the drive root that is "category -> app -> data", and one level of drilling is not enough.
                        // One extra readdir, with an internal 20,000-directory cap, will not get stuck in huge directories.
                        if let Some(found) = self.find_directory_by_name(&path, data_dir_name, 2) {
                            info!("全盘搜索发现: {}", found.display());
                            found_paths.push(found);
                        }
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

    /// Find a subdirectory by name within a given directory, limiting recursion depth and check count to control runtime.
    ///
    /// ⚠️ Traversal goes through `read_subdirectories_cached` rather than `walkdir`:
    /// WeChat's and QQ's fallback searches overlap completely, so on the second search these directories are already cached,
    /// producing almost no IO. Depth semantics match walkdir's `max_depth` (base is level 0).
    pub(super) fn find_directory_by_name(
        &self,
        base: &Path,
        name: &str,
        max_depth: usize,
    ) -> Option<PathBuf> {
        const MAX_CHECKED_DIRS: usize = 20_000;
        let mut checked = 0usize;
        let mut frontier: Vec<(PathBuf, usize)> = vec![(base.to_path_buf(), 0)];

        while let Some((dir, depth)) = frontier.pop() {
            checked += 1;
            // Directory indexing itself has a cost; limiting the check count prevents long stalls in huge directories
            if checked > MAX_CHECKED_DIRS {
                debug!(
                    "目录检查数量超限，停止在 {} 内查找 {}",
                    base.display(),
                    name
                );
                return None;
            }

            if dir
                .file_name()
                .is_some_and(|candidate| candidate.to_string_lossy().eq_ignore_ascii_case(name))
            {
                return Some(dir);
            }

            if depth >= max_depth {
                continue;
            }
            for child in self.read_subdirectories_cached(&dir) {
                frontier.push((child, depth + 1));
            }
        }

        None
    }

    /// Read the custom WeChat path from the registry
    ///
    /// Registry paths:
    /// - Legacy 3.x: HKEY_CURRENT_USER\Software\Tencent\WeChat -> FileSavePath
    /// - New 4.x: HKEY_CURRENT_USER\Software\Tencent\Weixin  -> FileSavePath
    ///
    /// Return value meaning:
    /// - `MyDocument:` -> use the system documents directory
    /// - Absolute path (e.g., `E:\data\xwechat_files`) -> use directly
    #[cfg(target_os = "windows")]
    pub(super) fn read_wechat_registry_path(&self) -> Option<RegistryPathResult> {
        use winreg::enums::*;
        use winreg::RegKey;

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);

        // New 4.x clients register under Weixin, legacy ones under WeChat; read the new version first
        for key_path in ["Software\\Tencent\\Weixin", "Software\\Tencent\\WeChat"] {
            let Ok(wechat_key) = hkcu.open_subkey(key_path) else {
                debug!("打开微信注册表键失败: {}", key_path);
                continue;
            };

            match wechat_key.get_value::<String, _>("FileSavePath") {
                Ok(path) => {
                    if path.is_empty() {
                        debug!("微信 FileSavePath 为空: {}", key_path);
                        continue;
                    }

                    // Check whether it is the special value MyDocument:
                    if path.trim().eq_ignore_ascii_case("MyDocument:") {
                        info!("微信 FileSavePath = MyDocument: ({})", key_path);
                        return Some(RegistryPathResult::MyDocument);
                    }

                    // Absolute path
                    info!("微信 FileSavePath = {} ({})", path, key_path);
                    return Some(RegistryPathResult::AbsolutePath(path));
                }
                Err(e) => {
                    debug!("读取微信 FileSavePath 失败({}): {}", key_path, e);
                }
            }

            // New clients do not always write FileSavePath; fall back to inferring the data directory from the install directory:
            // Custom data directories are often at the same level as the install directory, e.g. D:\software\wechat\Weixin + D:\software\wechat\xwechat_files
            if let Ok(install_path) = wechat_key.get_value::<String, _>("InstallPath") {
                if let Some(data_root) = Self::wechat_data_root_near_install(&install_path) {
                    info!("微信安装目录推断数据目录: {}", data_root.display());
                    return Some(RegistryPathResult::AbsolutePath(
                        data_root.to_string_lossy().to_string(),
                    ));
                }
            }
        }

        None
    }

    /// Infer the data directory from the install directory: check the install directory itself first, then sibling directories.
    ///
    /// New WeChat puts the program in `...\Weixin` and the data in `...\xwechat_files` (at the same level),
    /// and on some machines the FileSavePath in the registry cannot be read at all.
    /// The candidate order is fixed as "legacy WeChat Files first" to keep behavior unchanged when new and old clients coexist.
    #[cfg(target_os = "windows")]
    pub(super) fn wechat_data_root_near_install(install_path: &str) -> Option<PathBuf> {
        let install_dir = Path::new(install_path);
        if !install_dir.is_dir() {
            return None;
        }

        let mut bases = vec![install_dir.to_path_buf()];
        if let Some(parent) = install_dir.parent() {
            bases.push(parent.to_path_buf());
        }

        for base in bases {
            for data_dir_name in WECHAT_DATA_DIR_NAMES {
                let candidate = base.join(data_dir_name);
                if candidate.is_dir() {
                    return Some(candidate);
                }
            }
        }

        None
    }

    #[cfg(not(target_os = "windows"))]
    pub(super) fn read_wechat_registry_path(&self) -> Option<RegistryPathResult> {
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

    /// Concurrent test cases in one process would collide on names; a sequence number gives each fixture its own path.
    static FIXTURE_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    /// Build a temporary directory; the caller cleans it up after the test.
    fn fixture(name: &str) -> PathBuf {
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lightc-wechat-{}-{}-{}",
            name,
            std::process::id(),
            sequence
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建测试目录失败");
        dir
    }

    #[test]
    fn detects_new_wechat_data_directory_name() {
        // The new 4.x data directory is named xwechat_files and should not be treated as a "parent directory" with another level appended
        assert!(SocialScanner::is_wechat_data_dir_name("xwechat_files"));
        assert!(SocialScanner::is_wechat_data_dir_name("WeChat Files"));
        assert!(SocialScanner::is_wechat_data_dir_name("XWECHAT_FILES"));
        assert!(!SocialScanner::is_wechat_data_dir_name("WeChat"));
    }

    #[test]
    fn recognizes_wechat4_account_by_structure() {
        let root = fixture("v4-account");
        let account = root.join("wxid_abc123_4f4c");
        std::fs::create_dir_all(account.join("db_storage")).unwrap();
        std::fs::create_dir_all(account.join("msg/attach")).unwrap();
        // A legacy account directory should not be classified as 4.x
        let legacy = root.join("wxid_legacy");
        std::fs::create_dir_all(legacy.join("Msg")).unwrap();
        std::fs::create_dir_all(legacy.join("FileStorage/Image")).unwrap();

        assert!(SocialScanner::looks_like_wechat4_account(&account));
        // A legacy account directory should not be classified as 4.x
        {
            let dump = format!(
                "account={}\naccount_kind={:?}\nlegacy={}\nlegacy_kind={:?}\nlegacy_db_storage_exists={}\nlegacy_msg_exists={}\nlegacy_children={:?}\n",
                account.display(),
                SocialScanner::wechat_account_kind(&account),
                legacy.display(),
                SocialScanner::wechat_account_kind(&legacy),
                legacy.join("db_storage").exists(),
                legacy.join("msg").exists(),
                std::fs::read_dir(&legacy)
                    .map(|e| e.filter_map(|x| x.ok()).map(|x| x.file_name().to_string_lossy().to_string()).collect::<Vec<_>>())
                    .unwrap_or_default()
            );
            std::fs::write(std::env::temp_dir().join("lightc-wechat-debug.txt"), dump).unwrap();
        }
        assert!(!SocialScanner::looks_like_wechat4_account(&legacy));
        assert!(SocialScanner::looks_like_wechat_account(&legacy));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scans_wechat4_account_directories() {
        let root = fixture("v4-scan");
        let account = root.join("wxid_abc123_4f4c");
        std::fs::create_dir_all(account.join("db_storage")).unwrap();
        std::fs::create_dir_all(account.join("msg/file")).unwrap();
        std::fs::create_dir_all(account.join("msg/video")).unwrap();
        std::fs::create_dir_all(account.join("cache")).unwrap();
        // In 4.x, temp\RWTemp is a plaintext copy of chat messages; classify by content rather than as cache
        std::fs::create_dir_all(account.join("temp/RWTemp/2026-11")).unwrap();
        std::fs::create_dir_all(account.join("business/sns")).unwrap();
        // Backup directories should not be scanned as account directories
        std::fs::create_dir_all(root.join("Backup/wxid_abc123")).unwrap();

        let scanner = SocialScanner::new();
        let mut paths = Vec::new();
        scanner.scan_wechat_base_directory(&root, false, &mut paths);

        let found = |name: &str, category: FileCategory| {
            paths
                .iter()
                .any(|p| p.path.ends_with(name) && p.category == category)
        };

        assert!(found("db_storage", FileCategory::ChatDatabase));
        assert!(found("file", FileCategory::FileTransfer));
        assert!(found("video", FileCategory::ImageVideo));
        assert!(found("cache", FileCategory::TempCache));
        assert!(found("RWTemp", FileCategory::ImageVideo));
        assert!(found("sns", FileCategory::MomentsCache));
        assert!(!paths
            .iter()
            .any(|p| p.path.to_string_lossy().contains("Backup")));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn finds_nested_data_directory_by_name() {
        let scanner = SocialScanner::new();
        let root = fixture("nested");
        let target = root.join("wechat");
        std::fs::create_dir_all(target.join("xwechat_files")).unwrap();

        let found = scanner.find_directory_by_name(&root, "WeChat Files", 4);
        assert!(found.is_none(), "不应把 xwechat_files 匹配成 WeChat Files");

        let found = scanner.find_directory_by_name(&root, "xwechat_files", 4);
        assert_eq!(found, Some(target.join("xwechat_files")));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Issue #97: a user puts chat data in a self-created category directory,
    /// counted from the drive root that is three levels ("category -> app -> data") (D://chat-records//QQ//Tencent Files).
    /// A full-disk search that drills down only 1 level can never reach it; changing to 2 levels covers it.
    #[test]
    fn finds_data_dir_under_a_named_category_folder() {
        let scanner = SocialScanner::new();
        let root = fixture("categorized-data");
        std::fs::create_dir_all(root.join("聊天记录/QQ/Tencent Files")).unwrap();
        std::fs::create_dir_all(root.join("聊天记录/微信/xwechat_files")).unwrap();
        let category = root.join("聊天记录");

        assert!(
            scanner
                .find_directory_by_name(&category, "Tencent Files", 2)
                .is_some(),
            "分类目录下的 Tencent Files 应能找到"
        );
        assert!(
            scanner
                .find_directory_by_name(&category, "xwechat_files", 2)
                .is_some(),
            "分类目录下的 xwechat_files 应能找到"
        );
        // Drilling only 1 level truly cannot find it - proving this test really guards the depth rather than passing by chance
        assert!(
            scanner
                .find_directory_by_name(&category, "Tencent Files", 1)
                .is_none(),
            "深度 1 不应命中，否则用例失去意义"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}

// ========================================================================
