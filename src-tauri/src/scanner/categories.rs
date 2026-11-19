// ============================================================================
// Junk file category definitions
// Defines the various types of cleanable junk files and their scan rules
// ============================================================================

use serde::{Deserialize, Serialize};

/// Junk file category enum
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum JunkCategory {
    /// Windows temporary files (%TEMP%, Windows\Temp)
    WindowsTemp,
    /// System cache files (Prefetch, SoftwareDistribution, etc.)
    SystemCache,
    /// Browser caches (Chrome, Edge, Firefox, etc.)
    BrowserCache,
    /// Recycle bin
    RecycleBin,
    /// Windows Update cache
    WindowsUpdate,
    /// Windows Delivery Optimization cache
    DeliveryOptimization,
    /// Microsoft Defender non-critical antivirus files
    WindowsDefenderCache,
    /// Thumbnail cache
    ThumbnailCache,
    /// Log files
    LogFiles,
    /// Memory dump files
    MemoryDump,
    /// Old Windows installation files (Windows.old)
    OldWindowsInstallation,
    /// Application caches
    AppCache,
    /// Third-party app caches (Discord, Teams, NetEase Cloud Music, etc.)
    ThirdPartyAppCache,
    /// Font cache
    FontCache,
    /// Windows Error Reporting
    WindowsErrorReports,
    /// Installer temporary files
    InstallerTemp,
    /// Clipboard cache
    ClipboardCache,
    /// DirectX/GPU shader cache
    ShaderCache,
}

impl JunkCategory {
    /// Gets the Chinese display name of a category
    pub fn display_name(&self) -> &'static str {
        match self {
            JunkCategory::WindowsTemp => "Windows临时文件",
            JunkCategory::SystemCache => "系统缓存",
            JunkCategory::BrowserCache => "浏览器缓存",
            JunkCategory::RecycleBin => "回收站",
            JunkCategory::WindowsUpdate => "Windows更新缓存",
            JunkCategory::DeliveryOptimization => "传递优化文件",
            JunkCategory::WindowsDefenderCache => "Microsoft Defender 防病毒",
            JunkCategory::ThumbnailCache => "缩略图缓存",
            JunkCategory::LogFiles => "日志文件",
            JunkCategory::MemoryDump => "内存转储文件",
            JunkCategory::OldWindowsInstallation => "旧版Windows安装",
            JunkCategory::AppCache => "应用程序缓存",
            JunkCategory::ThirdPartyAppCache => "第三方应用缓存",
            JunkCategory::FontCache => "字体缓存",
            JunkCategory::WindowsErrorReports => "Windows错误报告",
            JunkCategory::InstallerTemp => "安装程序临时文件",
            JunkCategory::ClipboardCache => "剪贴板缓存",
            JunkCategory::ShaderCache => "DirectX Shader 缓存",
        }
    }

    /// Gets the description of a category
    pub fn description(&self) -> &'static str {
        match self {
            JunkCategory::WindowsTemp => "系统和应用程序产生的临时文件，可安全删除",
            JunkCategory::SystemCache => "Windows系统预读取和分发缓存文件",
            JunkCategory::BrowserCache => {
                "浏览器保存的网页资源缓存（不含 Cookie 和登录状态），删除后网页首次访问较慢"
            }
            JunkCategory::RecycleBin => "已删除但未彻底清除的文件",
            JunkCategory::WindowsUpdate => "Windows更新下载的安装包缓存",
            JunkCategory::DeliveryOptimization => {
                "Windows 传递优化下载和分发过程中产生的可重建缓存"
            }
            JunkCategory::WindowsDefenderCache => {
                "Microsoft Defender 生成的非关键日志和临时文件，删除后会自动重建"
            }
            JunkCategory::ThumbnailCache => "文件夹中图片和视频的缩略图缓存",
            JunkCategory::LogFiles => "系统和应用程序的日志记录文件",
            JunkCategory::MemoryDump => "系统崩溃时产生的内存转储文件",
            JunkCategory::OldWindowsInstallation => "系统升级后保留的旧版Windows文件",
            JunkCategory::AppCache => "各类应用程序产生的缓存文件",
            JunkCategory::ThirdPartyAppCache => {
                "Discord、Teams、网易云音乐等第三方应用的可重建缓存，删除后应用会自动重新生成"
            }
            JunkCategory::FontCache => "Windows字体渲染缓存，删除后会自动重建",
            JunkCategory::WindowsErrorReports => "系统和应用崩溃时生成的错误报告文件",
            JunkCategory::InstallerTemp => "软件安装过程中产生的临时文件",
            JunkCategory::ClipboardCache => "剪贴板历史记录缓存文件",
            JunkCategory::ShaderCache => "GPU 着色器编译缓存，删除后游戏和应用首次运行时会重新生成",
        }
    }

    /// Gets the risk level of a category (1-5, 1 is safest)
    pub fn risk_level(&self) -> u8 {
        match self {
            JunkCategory::WindowsTemp => 1,
            JunkCategory::ThumbnailCache => 1,
            JunkCategory::FontCache => 1,
            JunkCategory::ClipboardCache => 1,
            JunkCategory::ShaderCache => 1,
            JunkCategory::BrowserCache => 2,
            JunkCategory::WindowsUpdate => 2,
            JunkCategory::DeliveryOptimization => 2,
            JunkCategory::WindowsDefenderCache => 2,
            JunkCategory::LogFiles => 2,
            JunkCategory::WindowsErrorReports => 2,
            JunkCategory::InstallerTemp => 2,
            JunkCategory::RecycleBin => 3,
            JunkCategory::SystemCache => 3,
            JunkCategory::AppCache => 3,
            JunkCategory::ThirdPartyAppCache => 2,
            JunkCategory::MemoryDump => 3,
            JunkCategory::OldWindowsInstallation => 3,
        }
    }

    /// Whether this category is system-managed — system-managed categories are not deleted directly by this app; users are only guided to system tools.
    ///
    /// Currently only old Windows installations (Windows.old / $Windows.~BT / $Windows.~WS):
    /// these directories are owned by the system and kept for 10 days by default for rollback; Windows only exposes
    /// the "Disk Cleanup / Storage Sense" entry points. Direct deletion from the app fails; even if it succeeded,
    /// it would mean losing the ability to roll back, so this app should not do it.
    pub fn is_system_managed(&self) -> bool {
        matches!(self, JunkCategory::OldWindowsInstallation)
    }

    /// Gets the list of paths to scan for this category
    pub fn get_scan_paths(&self) -> Vec<ScanPath> {
        match self {
            JunkCategory::WindowsTemp => vec![
                ScanPath::env_path("TEMP", None),
                ScanPath::env_path("TMP", None),
                // Use the system root instead of a hard-coded C drive, to support Windows installed on another drive.
                ScanPath::env_path("SYSTEMROOT", Some("Temp")),
            ],
            JunkCategory::SystemCache => vec![
                // Windows prefetch cache
                ScanPath::env_path("SYSTEMROOT", Some("Prefetch")),
                // Windows application cache
                ScanPath::env_path("LOCALAPPDATA", Some("Microsoft\\Windows\\Caches")),
            ],
            JunkCategory::BrowserCache => vec![
                // Compatible with the Temporary Internet Files reparse path from legacy IE/WinINet,
                // this directory contains only rebuildable web resource caches, no cookies.
                ScanPath::env_path("LOCALAPPDATA", Some("Microsoft\\Windows\\INetCache")),
                // Chrome - main cache
                ScanPath::glob_path("LOCALAPPDATA", "Google\\Chrome\\User Data\\Default\\Cache"),
                ScanPath::glob_path(
                    "LOCALAPPDATA",
                    "Google\\Chrome\\User Data\\Default\\Code Cache",
                ),
                ScanPath::glob_path(
                    "LOCALAPPDATA",
                    "Google\\Chrome\\User Data\\Default\\GPUCache",
                ),
                ScanPath::glob_path(
                    "LOCALAPPDATA",
                    "Google\\Chrome\\User Data\\Profile *\\Cache",
                ),
                ScanPath::glob_path(
                    "LOCALAPPDATA",
                    "Google\\Chrome\\User Data\\Profile *\\Code Cache",
                ),
                ScanPath::glob_path("LOCALAPPDATA", "Google\\Chrome\\User Data\\ShaderCache"),
                // Edge - main cache
                ScanPath::glob_path("LOCALAPPDATA", "Microsoft\\Edge\\User Data\\Default\\Cache"),
                ScanPath::glob_path(
                    "LOCALAPPDATA",
                    "Microsoft\\Edge\\User Data\\Default\\Code Cache",
                ),
                ScanPath::glob_path(
                    "LOCALAPPDATA",
                    "Microsoft\\Edge\\User Data\\Default\\GPUCache",
                ),
                ScanPath::glob_path(
                    "LOCALAPPDATA",
                    "Microsoft\\Edge\\User Data\\Profile *\\Cache",
                ),
                ScanPath::glob_path(
                    "LOCALAPPDATA",
                    "Microsoft\\Edge\\User Data\\Profile *\\Code Cache",
                ),
                ScanPath::glob_path("LOCALAPPDATA", "Microsoft\\Edge\\User Data\\ShaderCache"),
                // Firefox - specific cache directories
                ScanPath::glob_path("LOCALAPPDATA", "Mozilla\\Firefox\\Profiles\\*\\cache2"),
                ScanPath::glob_path("APPDATA", "Mozilla\\Firefox\\Profiles\\*\\cache2"),
                // Brave browser
                ScanPath::env_path(
                    "LOCALAPPDATA",
                    Some("BraveSoftware\\Brave-Browser\\User Data\\Default\\Cache"),
                ),
                ScanPath::env_path(
                    "LOCALAPPDATA",
                    Some("BraveSoftware\\Brave-Browser\\User Data\\Default\\Code Cache"),
                ),
                // Opera browser
                ScanPath::env_path("APPDATA", Some("Opera Software\\Opera Stable\\Cache")),
                // Domestic and international Chromium-based browsers (cache structure matches Chrome; rebuilt automatically after deletion)
                ScanPath::glob_path("LOCALAPPDATA", "Tencent\\QQBrowser\\User Data\\Default\\Cache"),
                ScanPath::glob_path(
                    "LOCALAPPDATA",
                    "Tencent\\QQBrowser\\User Data\\Default\\Code Cache",
                ),
                ScanPath::glob_path("LOCALAPPDATA", "360Chrome\\Chrome\\User Data\\Default\\Cache"),
                ScanPath::glob_path(
                    "LOCALAPPDATA",
                    "360Chrome\\Chrome\\User Data\\Default\\Code Cache",
                ),
                ScanPath::glob_path("LOCALAPPDATA", "360se6\\User Data\\Default\\Cache"),
                ScanPath::glob_path("LOCALAPPDATA", "UCBrowser\\User Data\\Default\\Cache"),
                ScanPath::glob_path("LOCALAPPDATA", "Vivaldi\\User Data\\Default\\Cache"),
                ScanPath::glob_path("LOCALAPPDATA", "YandexBrowser\\User Data\\Default\\Cache"),
                ScanPath::glob_path("LOCALAPPDATA", "CentBrowser\\User Data\\Default\\Cache"),
            ],
            JunkCategory::RecycleBin => get_all_drive_letters()
                .into_iter()
                .map(|letter| ScanPath::fixed_path(&format!("{}:\\$Recycle.Bin", letter)))
                .collect(),
            JunkCategory::WindowsUpdate => vec![ScanPath::env_path(
                "SYSTEMROOT",
                Some("SoftwareDistribution\\Download"),
            )],
            JunkCategory::DeliveryOptimization => vec![
                // Old and new Windows versions use different service account directories; both whitelist roots contain only rebuildable caches.
                ScanPath::env_path(
                    "SYSTEMROOT",
                    Some("SoftwareDistribution\\DeliveryOptimization"),
                ),
                ScanPath::env_path(
                    "SYSTEMROOT",
                    Some("ServiceProfiles\\NetworkService\\AppData\\Local\\Microsoft\\Windows\\DeliveryOptimization\\Cache"),
                ),
            ],
            JunkCategory::WindowsDefenderCache => vec![
                // The Defender item in the Windows Cleanup Wizard points only to these few non-critical directories, not a broad scan of the Defender data root.
                ScanPath::env_path(
                    "PROGRAMDATA",
                    Some("Microsoft\\Windows Defender\\LocalCopy"),
                ),
                ScanPath::env_path(
                    "PROGRAMDATA",
                    Some("Microsoft\\Windows Defender\\Support"),
                ),
                // Defender scan-history directory (rebuilt automatically after a scan), consistent with the Defender item in the Windows Cleanup Wizard.
                ScanPath::env_path(
                    "PROGRAMDATA",
                    Some("Microsoft\\Windows Defender\\Scans\\History\\Service"),
                ),
            ],
            JunkCategory::LogFiles => vec![
                ScanPath::env_path("SYSTEMROOT", Some("Logs")),
                ScanPath::env_path("LOCALAPPDATA", Some("CrashDumps")),
            ],
            JunkCategory::MemoryDump => vec![
                ScanPath::env_path("SYSTEMROOT", Some("Minidump")),
                ScanPath::env_path("SYSTEMROOT", Some("MEMORY.DMP")),
            ],
            JunkCategory::OldWindowsInstallation => vec![
                // The old system directory sits at the root of the system drive; use SYSTEMDRIVE to support non-C installations.
                ScanPath::system_drive_path("Windows.old"),
                ScanPath::system_drive_path("$Windows.~BT"),
                ScanPath::system_drive_path("$Windows.~WS"),
            ],
            JunkCategory::AppCache => vec![
                ScanPath::env_path("LOCALAPPDATA", Some("Microsoft\\Windows\\WebCache")),
                // No longer broadly scanning Packages\*\LocalCache: MSIX desktop apps store WebView2 user profiles,
                // session indexes, and app state here; cleaning by "*" directly would delete persistent data of apps like Claude by mistake.
            ],
            JunkCategory::ThirdPartyAppCache => vec![
                // Standard cache directories for Chromium-based desktop apps (Cache/Code Cache/GPUCache/ShaderCache),
                // rebuilt automatically at app startup after deletion; the glob pattern is skipped when the app is not installed, so there is no scan cost.
                // Discord
                ScanPath::glob_path("APPDATA", "discord\\Cache"),
                ScanPath::glob_path("APPDATA", "discord\\Code Cache"),
                ScanPath::glob_path("APPDATA", "discord\\GPUCache"),
                ScanPath::glob_path("APPDATA", "discord\\ShaderCache"),
                // Slack
                ScanPath::glob_path("APPDATA", "Slack\\Cache"),
                // Microsoft Teams
                ScanPath::glob_path("APPDATA", "Microsoft\\Teams\\Cache"),
                ScanPath::glob_path("APPDATA", "Microsoft\\Teams\\GPUCache"),
                // Steam built-in browser cache
                ScanPath::glob_path("LOCALAPPDATA", "Steam\\htmlcache"),
                // Epic Games Launcher built-in browser cache
                ScanPath::glob_path("LOCALAPPDATA", "EpicGamesLauncher\\Saved\\webcache"),
                // NetEase Cloud Music
                ScanPath::glob_path("LOCALAPPDATA", "Netease\\CloudMusic\\Cache"),
                // QQ Music
                ScanPath::glob_path("APPDATA", "Tencent\\QQMusic\\Cache"),
            ],
            JunkCategory::FontCache => vec![ScanPath::env_path(
                "SYSTEMROOT",
                Some("ServiceProfiles\\LocalService\\AppData\\Local\\FontCache"),
            )],
            JunkCategory::WindowsErrorReports => vec![
                ScanPath::env_path("LOCALAPPDATA", Some("Microsoft\\Windows\\WER")),
                // ProgramData also uses an environment variable to avoid missing it when the system drive is not C.
                ScanPath::env_path("PROGRAMDATA", Some("Microsoft\\Windows\\WER")),
            ],
            JunkCategory::InstallerTemp => vec![
                // Windows Installer patch cache
                ScanPath::env_path("SYSTEMROOT", Some("Installer\\$PatchCache$")),
                // Downloaded installers
                ScanPath::env_path("LOCALAPPDATA", Some("Downloaded Installations")),
                // C:\NVIDIA, C:\AMD, C:\Intel are driver installers saved deliberately by the user,
                // deleting them could prevent the user from rolling back drivers, so they are no longer in scan scope
            ],
            JunkCategory::ClipboardCache => vec![ScanPath::env_path(
                "LOCALAPPDATA",
                Some("Microsoft\\Windows\\Clipboard"),
            )],
            JunkCategory::ThumbnailCache => vec![
                ScanPath::env_path("LOCALAPPDATA", Some("Microsoft\\Windows\\Explorer")),
                // Thumbnails are stored per user profile; also scan other users on the same system drive, but do not expand to the whole Users directory.
                ScanPath::glob_path(
                    "SYSTEMDRIVE",
                    "Users\\*\\AppData\\Local\\Microsoft\\Windows\\Explorer",
                ),
            ],
            JunkCategory::ShaderCache => vec![
                // D3DCache sits under System32, but it is a rebuildable cache, so the scanner makes a whitelist exception for it.
                ScanPath::env_path("SYSTEMROOT", Some("System32\\d3d_cache")),
                ScanPath::env_path("LOCALAPPDATA", Some("D3DSCache")),
                ScanPath::env_path("LOCALAPPDATA", Some("AMD\\DxCache")),
                ScanPath::env_path("LOCALAPPDATA", Some("NVIDIA\\DXCache")),
                ScanPath::env_path("LOCALAPPDATA", Some("Intel\\ShaderCache")),
            ],
        }
    }

    /// Gets the file filter rules for this category
    pub fn get_file_patterns(&self) -> Vec<&'static str> {
        match self {
            JunkCategory::WindowsTemp => vec!["*"],
            // The system cache root is already an explicit whitelist; matching all files covers Prefetch and Caches
            // rebuildable files without the .pf extension, without expanding to other Windows directories.
            JunkCategory::SystemCache => vec!["*"],
            JunkCategory::BrowserCache => vec!["*"],
            JunkCategory::RecycleBin => vec!["*"],
            JunkCategory::WindowsUpdate => vec!["*"],
            JunkCategory::DeliveryOptimization => vec!["*"],
            JunkCategory::WindowsDefenderCache => vec!["*"],
            JunkCategory::ThumbnailCache => vec!["thumbcache_*.db", "iconcache_*.db"],
            JunkCategory::LogFiles => vec!["*.log", "*.etl", "*.evtx"],
            JunkCategory::MemoryDump => vec!["*.dmp", "MEMORY.DMP"],
            JunkCategory::OldWindowsInstallation => vec!["*"],
            JunkCategory::AppCache => vec!["*"],
            JunkCategory::ThirdPartyAppCache => vec!["*"],
            JunkCategory::FontCache => vec!["*"],
            JunkCategory::WindowsErrorReports => vec!["*"],
            JunkCategory::InstallerTemp => vec!["*"],
            JunkCategory::ClipboardCache => vec!["*"],
            JunkCategory::ShaderCache => vec!["*"],
        }
    }

    /// Gets all categories
    pub fn all() -> Vec<JunkCategory> {
        vec![
            JunkCategory::WindowsTemp,
            JunkCategory::SystemCache,
            JunkCategory::BrowserCache,
            JunkCategory::RecycleBin,
            JunkCategory::WindowsUpdate,
            JunkCategory::DeliveryOptimization,
            JunkCategory::WindowsDefenderCache,
            JunkCategory::ThumbnailCache,
            JunkCategory::LogFiles,
            JunkCategory::MemoryDump,
            JunkCategory::OldWindowsInstallation,
            JunkCategory::AppCache,
            JunkCategory::ThirdPartyAppCache,
            JunkCategory::FontCache,
            JunkCategory::WindowsErrorReports,
            JunkCategory::InstallerTemp,
            JunkCategory::ClipboardCache,
            JunkCategory::ShaderCache,
        ]
    }
}

/// Gets the drive letters present on the current system
fn get_all_drive_letters() -> Vec<char> {
    // L6: delegate to sys_drive's single GetLogicalDrives lookup instead of
    // blind-probing all 26 letters; the exists/is_dir belt-and-braces re-check
    // stays inside sys_drive. The last two ('A'..='Z') probes converged in M13
    // live here and in ai_models/mft_discovery.rs.
    #[cfg(windows)]
    {
        crate::sys_drive::existing_drive_letters()
    }

    #[cfg(not(windows))]
    {
        ('A'..='Z')
            .filter(|letter| {
                let path = std::path::PathBuf::from(format!("{}:\\", letter));
                path.exists() && path.is_dir()
            })
            .collect()
    }
}

/// Scan path config
#[derive(Debug, Clone)]
pub struct ScanPath {
    /// Path type
    pub path_type: PathType,
    /// Base path or environment variable name
    pub base: String,
    /// Subpath (optional)
    pub sub_path: Option<String>,
}

/// Path type
#[derive(Debug, Clone)]
pub enum PathType {
    /// Fixed path
    Fixed,
    /// Path based on an environment variable
    EnvBased,
    /// Wildcard-expanded path based on an environment variable
    GlobExpand,
    /// Absolute path based on a system-drive environment variable, to avoid `C:` being interpreted as a drive-relative path
    SystemDriveBased,
}

impl ScanPath {
    /// Creates a fixed path
    pub fn fixed_path(path: &str) -> Self {
        ScanPath {
            path_type: PathType::Fixed,
            base: path.to_string(),
            sub_path: None,
        }
    }

    /// Creates a path based on an environment variable
    pub fn env_path(env_var: &str, sub_path: Option<&str>) -> Self {
        ScanPath {
            path_type: PathType::EnvBased,
            base: env_var.to_string(),
            sub_path: sub_path.map(|s| s.to_string()),
        }
    }

    /// Creates a wildcard path based on an environment variable
    pub fn glob_path(env_var: &str, pattern: &str) -> Self {
        ScanPath {
            path_type: PathType::GlobExpand,
            base: env_var.to_string(),
            sub_path: Some(pattern.to_string()),
        }
    }

    /// Creates an absolute path based on SYSTEMDRIVE.
    pub fn system_drive_path(sub_path: &str) -> Self {
        ScanPath {
            path_type: PathType::SystemDriveBased,
            base: "SYSTEMDRIVE".to_string(),
            sub_path: Some(sub_path.to_string()),
        }
    }

    /// Resolves to an actual path
    pub fn resolve(&self) -> Option<std::path::PathBuf> {
        match &self.path_type {
            PathType::Fixed => {
                let path = std::path::PathBuf::from(&self.base);
                if path.exists() {
                    Some(path)
                } else {
                    log::debug!("扫描路径不存在，跳过: {:?}", path);
                    None
                }
            }
            PathType::EnvBased => std::env::var(&self.base).ok().and_then(|base_path| {
                let mut path = std::path::PathBuf::from(base_path);
                if let Some(sub) = &self.sub_path {
                    path.push(sub);
                }
                if path.exists() {
                    Some(path)
                } else {
                    log::debug!("扫描路径不存在，跳过: {:?}", path);
                    None
                }
            }),
            PathType::SystemDriveBased => {
                // SYSTEMDRIVE is usually `C:`; explicitly append the root to avoid producing `C:Windows.old` drive-relative path.
                let drive = std::env::var(&self.base)
                    .or_else(|_| {
                        // Some stripped-down environments may lack SYSTEMDRIVE; fall back to the drive from SYSTEMROOT.
                        std::env::var("SYSTEMROOT")
                            .map(|system_root| system_root.chars().take(2).collect::<String>())
                    })
                    .unwrap_or_else(|_| "C:".to_string());
                let drive_root =
                    format!("{}\\", drive.trim_end_matches('\\').trim_end_matches('/'));
                let mut path = std::path::PathBuf::from(drive_root);
                if let Some(sub) = &self.sub_path {
                    path.push(sub);
                }
                if path.exists() {
                    Some(path)
                } else {
                    log::debug!("扫描路径不存在，跳过: {:?}", path);
                    None
                }
            }
            PathType::GlobExpand => None,
        }
    }

    /// Resolves to a list of actual paths
    pub fn resolve_all(&self) -> Vec<std::path::PathBuf> {
        match &self.path_type {
            PathType::Fixed | PathType::EnvBased | PathType::SystemDriveBased => {
                self.resolve().into_iter().collect()
            }
            PathType::GlobExpand => {
                let Some(pattern) = &self.sub_path else {
                    return Vec::new();
                };

                let Ok(base_path) = std::env::var(&self.base) else {
                    log::debug!("环境变量不存在，跳过 Glob 展开: {}", self.base);
                    return Vec::new();
                };

                let mut full_pattern = std::path::PathBuf::from(base_path);
                full_pattern.push(pattern);
                let pattern_string = full_pattern.to_string_lossy().to_string();

                let results: Vec<std::path::PathBuf> = match glob::glob(&pattern_string) {
                    Ok(paths) => paths
                        .filter_map(|entry| entry.ok())
                        .filter(|path| path.exists())
                        .collect(),
                    Err(err) => {
                        log::debug!("Glob 模式无效，跳过: {} ({})", pattern_string, err);
                        Vec::new()
                    }
                };

                log::debug!(
                    "Glob 展开 '{}' 得到 {} 个路径",
                    pattern_string,
                    results.len()
                );
                results
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_categories_covered() {
        const JUNK_CATEGORY_VARIANT_COUNT: usize = 18;
        assert_eq!(JunkCategory::all().len(), JUNK_CATEGORY_VARIANT_COUNT);
    }

    #[test]
    fn test_recycle_bin_multi_drive() {
        assert!(get_all_drive_letters().contains(&'C'));
    }

    #[test]
    fn test_glob_path_resolve_all() {
        let fixed_path = ScanPath::fixed_path("C:\\");
        assert_eq!(
            fixed_path.resolve_all().len(),
            fixed_path.resolve().map_or(0, |_| 1)
        );

        let env_path = ScanPath::env_path("LOCALAPPDATA", None);
        assert_eq!(
            env_path.resolve_all().len(),
            env_path.resolve().map_or(0, |_| 1)
        );
    }

    #[test]
    fn test_legacy_internet_cache_belongs_to_browser_cache() {
        // INetCache is the actual target of Temporary Internet Files; put it under browser cache so the full cache rules apply.
        let browser_paths = JunkCategory::BrowserCache.get_scan_paths();
        assert!(browser_paths.iter().any(|path| {
            matches!(&path.path_type, PathType::EnvBased)
                && path.base == "LOCALAPPDATA"
                && path.sub_path.as_deref() == Some("Microsoft\\Windows\\INetCache")
        }));
        assert!(!JunkCategory::SystemCache
            .get_scan_paths()
            .iter()
            .any(|path| { path.sub_path.as_deref() == Some("Microsoft\\Windows\\INetCache") }));
        // Within an explicit whitelist root, match all cache files to avoid missing ordinary caches by only recognizing .pf.
        assert_eq!(JunkCategory::SystemCache.get_file_patterns(), vec!["*"]);
    }

    #[test]
    fn test_windows_paths_use_dynamic_environment_roots() {
        // These paths must not be hard-coded to the C drive, otherwise installations on other partitions would be silently missed.
        let dynamic_paths = [
            (JunkCategory::WindowsTemp, "Temp"),
            (JunkCategory::SystemCache, "Prefetch"),
            (
                JunkCategory::WindowsUpdate,
                "SoftwareDistribution\\Download",
            ),
            (JunkCategory::LogFiles, "Logs"),
            (JunkCategory::MemoryDump, "Minidump"),
            (
                JunkCategory::FontCache,
                "ServiceProfiles\\LocalService\\AppData\\Local\\FontCache",
            ),
            (JunkCategory::InstallerTemp, "Installer\\$PatchCache$"),
        ];

        for (category, sub_path) in dynamic_paths {
            assert!(JunkCategory::get_scan_paths(&category).iter().any(|path| {
                matches!(&path.path_type, PathType::EnvBased)
                    && path.base == "SYSTEMROOT"
                    && path.sub_path.as_deref() == Some(sub_path)
            }));
        }
        assert!(JunkCategory::OldWindowsInstallation
            .get_scan_paths()
            .iter()
            .all(|path| matches!(&path.path_type, PathType::SystemDriveBased)));
    }
}
