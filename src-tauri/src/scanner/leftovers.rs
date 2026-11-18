// ============================================================================
// Uninstall leftover scanner module - confidence scoring detection engine
// ============================================================================
//
// [Architecture]
// This module uses a weighted scoring model instead of the original boolean judgment logic:
//   1. InstalledAppMap   - Builds an installed-app to path map from the registry to infer AppData ownership
//   2. ScoringEngine     - Computes a confidence score (0.0~1.0) for each directory from multiple signals
//   3. WhitelistRule     - Structured whitelist (Exact / Prefix / Pattern); global contains is forbidden
//   4. FileSystemProbe   - Limited-depth file probe (.exe / .dll / uninstall*.exe)
//
// [Scoring signals] (baseline 0.0, positive identification + protective penalties)
//   Positive (added):
//     +0.35  Found uninstall*.exe / uninst*.exe in the folder
//     +0.25  Folder name matches the last segment of InstallLocation (and app is no longer in the registry)
//     +0.20  Contains .exe or .dll files
//     +0.10  Not modified for more than min_days_old days
//     +0.10  Matches a known emulator signature
//   Negative (subtracted):
//     -0.45  Folder name exactly matches a currently installed app's DisplayName (normalized)
//     -0.60  Folder name exactly matches an installed app's InstallLocation last segment
//     -0.40  Generic folder name (cache, logs, temp, data)
//     -0.30  Located under ProgramData
//     -0.20  Modified within 7 days
//     -0.15  Package-name format folder (com.xxx.yyy)
//     -0.15  Pure version-number folder (1.2.3.4 / v2.0)
//     -0.50  Known shared vendor folder (Adobe, Microsoft, etc.)
//
// [Classification thresholds]
//   score >= 0.75 -> HighConfidenceLeftover (checked by default in the frontend)
//   0.30 <= score < 0.75 -> Suspicious (not checked in the frontend, display only)
//   score < 0.30 -> not output
//   Both thresholds are defined as constants (HIGH_CONFIDENCE_SCORE / SUSPICIOUS_SCORE); bucketing and filtering share the same source.
//
// [Scan paths]
// - %LOCALAPPDATA%      (C:\Users\<user>\AppData\Local)
// - %APPDATA%           (C:\Users\<user>\AppData\Roaming)
// - %LOCALAPPDATA%Low   (C:\Users\<user>\AppData\LocalLow)
// - C:\ProgramData
//
// [Deep scan mode]
// - Scan emulator leftovers (LDPlayer, BlueStacks, Nox, MuMu, MEmu, MSI App Player)
// - Scan orphaned virtual disk files (.vmdk, .vdi, .vhd)
//
// [Safety mechanisms]
// 1. Structured whitelist protection: critical system folders are never scanned
// 2. Confidence tiers: only high-confidence entries are checked by default
// 3. ProgramData penalty: not marked as leftover directly, only the score is lowered
// 4. Size threshold: ignore folders smaller than 1MB
// ============================================================================

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime};
use walkdir::WalkDir;
use winreg::enums::*;
use winreg::RegKey;

use super::leftover_whitelist;

// ============================================================================
// Install history persistence (for detecting leftover folders that were "installed before but now uninstalled")
// Use the unified data directory module (crate::data_dir) to get the storage path
// ============================================================================

/// Install-history cache file name
const INSTALL_HISTORY_FILE: &str = "install_history.json";

/// Load the set of historical install folder names from the persistent cache
fn load_install_history() -> HashSet<String> {
    let path = crate::data_dir::get_data_dir().join(INSTALL_HISTORY_FILE);
    match fs::read_to_string(&path) {
        Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
        Err(_) => HashSet::new(),
    }
}

/// Persist the currently known install folder names (merged with history)
fn save_install_history(folders: &HashSet<String>) {
    let dir = crate::data_dir::get_data_dir();
    if let Err(e) = fs::create_dir_all(&dir) {
        log::warn!("无法创建数据目录 {}: {}", dir.display(), e);
        return;
    }
    let path = dir.join(INSTALL_HISTORY_FILE);
    let json = serde_json::to_string_pretty(folders).unwrap_or_default();
    if let Err(e) = fs::write(&path, &json) {
        log::warn!("无法保存安装历史缓存 {}. {}", path.display(), e);
    }
}

/// Build a path from the unified data directory (for use by other internal modules)
#[allow(dead_code)]
fn get_data_sub_path(relative: &str) -> PathBuf {
    crate::data_dir::get_data_dir().join(relative)
}

// ============================================================================
// Precompiled regexes (package-name format / version-number format)
// ============================================================================

/// Package-name format: com.example.app, org.apache.commons, etc.
static RE_PACKAGE_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9]*(\.[a-z][a-z0-9]*){2,}$").unwrap());

/// Pure version-number format: 1.2.3.4, v2.0, etc.
static RE_VERSION_FOLDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^v?\d+(\.\d+){1,3}$").unwrap());

/// Updater directories: names containing updater / update, or ending directly with Updater.
///
/// Matches `kimi-desktop-updater`, `comfyui-desktop-2-updater`, `QuarkCloudDriveUpdater`,
/// `@genie\workbuddy-desktop-updater` and similar forms.
///
/// These folders hold "downloaded/unpacked new versions" and are no longer needed after the program updates. Worst case if deleted:
/// the next update simply downloads them again - this is the only category here where we can tell the user exactly "what the consequence is",
/// so it is worth marking separately (`DetectionCategory::UpdaterCache`) instead of lumping it into "high confidence".
static RE_UPDATER_DIR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(^|[-_. ])(updater|update)([-_. ]|$)|updater$").unwrap());

/// The app's own cache/log directories.
///
/// Matches `AviraWebView2Cache`, `SquirrelTemp`, `TSVNCache`, `PLogs`, `xxxCrashRpt` and similar.
///
/// ⚠️ Plain `Cache` / `Temp` / `CrashDumps` do not reach here - they are already excluded in `build_whitelist_rules()`;
/// that is the junk-cleanup module's territory, and reporting on both sides would make users think "it wasn't deleted".
/// This rule only covers "app directories whose names carry cache characteristics".
static RE_CACHE_DIR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"cache|caches|crashpad|crashrpt|crashdumps|crashreports|logs?$|temp$|tmp$").unwrap()
});

// ============================================================================
// Data model
// ============================================================================

/// Uninstall leftover scan result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeftoverScanResult {
    /// List of leftover folders found
    pub leftovers: Vec<LeftoverEntry>,
    /// Total size (bytes)
    pub total_size: u64,
    /// Scan duration (milliseconds)
    pub scan_duration_ms: u64,
    /// Number of top-level directories actually checked (including those blocked by whitelist / size threshold)
    ///
    /// When the result is empty, give the user evidence that "the module really ran"; otherwise it always returns 0 entries,
    /// and the user cannot tell "there really are no leftovers" from "the scan is broken".
    pub scanned_directories: usize,
    /// Number of entries that entered scoring but were dropped for being below the threshold
    pub below_threshold: usize,
    /// Highest confidence among all scored directories (0.0~1.0)
    pub highest_confidence: f32,
    /// Current decision threshold (0.0~1.0); the frontend reads the same source as the data to avoid hardcoding 0.40 in two places
    pub confidence_threshold: f32,
}

/// A single leftover entry (frontend-compatible + new confidence fields)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeftoverEntry {
    /// Folder path
    pub path: String,
    /// Folder size (bytes)
    pub size: u64,
    /// Possible software name (inferred from the folder name)
    pub app_name: String,
    /// Source type
    pub source: LeftoverSource,
    /// Last modified time (Unix timestamp)
    pub last_modified: i64,
    /// Number of files contained
    pub file_count: u32,
    /// Whether it is an emulator leftover
    pub is_emulator: bool,
    /// Whether it is a virtual disk file
    pub is_virtual_disk: bool,
    /// Leftover type description (for UI display)
    pub leftover_type: LeftoverType,
    /// Confidence score (0.0 ~ 1.0); higher means more likely a leftover
    pub confidence: f32,
    /// Detection category
    pub detection_category: DetectionCategory,
    /// List of scoring reasons (Chinese, for UI hover tooltips)
    pub reasons: Vec<String>,
}

/// Leftover type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LeftoverType {
    /// Normal app leftover
    Normal,
    /// Emulator leftover (LDPlayer, BlueStacks, Nox, etc.)
    Emulator,
    /// Virtual disk file (.vmdk, .vdi, .vhd)
    VirtualDisk,
    /// Registry-orphan leftover
    RegistryOrphan,
    /// Orphaned executable sitting directly in the AppData / ProgramData root
    LooseFile,
}

/// Leftover source type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LeftoverSource {
    /// AppData\Local
    LocalAppData,
    /// AppData\Roaming
    RoamingAppData,
    /// AppData\LocalLow
    LocalLowAppData,
    /// ProgramData
    ProgramData,
    /// Virtual disk file (standalone file)
    VirtualDiskFile,
}

impl LeftoverSource {
    #[allow(dead_code)]
    pub fn display_name(&self) -> &'static str {
        match self {
            LeftoverSource::LocalAppData => "本地应用数据",
            LeftoverSource::RoamingAppData => "漫游应用数据",
            LeftoverSource::LocalLowAppData => "LocalLow数据",
            LeftoverSource::ProgramData => "程序数据",
            LeftoverSource::VirtualDiskFile => "虚拟磁盘文件",
        }
    }
}

/// Detection category (confidence tiers)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DetectionCategory {
    /// High-confidence leftover (score >= 0.75)
    HighConfidenceLeftover,
    /// Suspicious (0.30 <= score < 0.75)
    Suspicious,
    /// Possibly app data still in use
    LikelyAppData,
    /// System shared directory
    SystemShared,
    /// Old version directory downloaded/unpacked by an updater.
    ///
    /// A separate category instead of merging into "high confidence": the two are not equally trustworthy.
    /// High confidence means "probably a leftover, but I can't guarantee it"; this category has **clear consequences** -
    /// after deletion the updater will just re-download next time, with no effect on already installed programs.
    /// Users seeing this label do not have to guess item by item.
    UpdaterCache,
    /// The app's own cache/log directories (`AviraWebView2Cache`, `SquirrelTemp`, `PLogs`).
    ///
    /// The difference from `UpdaterCache` is the cost: that one means "re-download hundreds of MB on next update",
    /// this one means "regenerated on next program run", usually unnoticeable. So they are labeled separately,
    /// letting users choose based on "whether I'm willing to wait for it to rebuild".
    CacheDir,
}

// ============================================================================
// Structured whitelist system
// Global contains matching is forbidden; only exact/prefix/wildcard patterns are allowed
// ============================================================================

/// Whitelist rule type
enum WhitelistRule {
    /// Exact match (case-insensitive)
    Exact(String),
    /// Prefix match (case-insensitive)
    Prefix(String),
    /// Wildcard pattern match (simplified: only * prefix/suffix supported)
    Pattern(String),
}

impl WhitelistRule {
    /// Check whether a folder name matches the rule
    fn matches(&self, folder_name_lower: &str) -> bool {
        match self {
            WhitelistRule::Exact(s) => folder_name_lower == s,
            WhitelistRule::Prefix(s) => folder_name_lower.starts_with(s),
            WhitelistRule::Pattern(p) => {
                // Simplified wildcard: only "prefix*" and "*suffix" patterns are supported
                if let Some(prefix) = p.strip_suffix('*') {
                    folder_name_lower.starts_with(prefix)
                } else if let Some(suffix) = p.strip_prefix('*') {
                    folder_name_lower.ends_with(suffix)
                } else {
                    folder_name_lower == p
                }
            }
        }
    }
}

/// Build the list of structured whitelist rules
fn build_whitelist_rules() -> Vec<WhitelistRule> {
    vec![
        // ==================== Windows system core (exact match) ====================
        WhitelistRule::Exact("microsoft".into()),
        WhitelistRule::Exact("windows".into()),
        WhitelistRule::Exact("packages".into()),
        WhitelistRule::Exact("windowsapps".into()),
        WhitelistRule::Exact("connecteddevicesplatform".into()),
        WhitelistRule::Exact("comms".into()),
        WhitelistRule::Exact("d3dscache".into()),
        WhitelistRule::Exact("diagnostics".into()),
        WhitelistRule::Exact("publishers".into()),
        WhitelistRule::Exact("temp".into()),
        WhitelistRule::Exact("temporary internet files".into()),
        // ==================== Hardware drivers (exact + prefix) ====================
        WhitelistRule::Exact("nvidia".into()),
        WhitelistRule::Exact("nvidia corporation".into()),
        WhitelistRule::Exact("amd".into()),
        WhitelistRule::Exact("intel".into()),
        WhitelistRule::Exact("realtek".into()),
        WhitelistRule::Exact("asus".into()),
        WhitelistRule::Exact("msi".into()),
        WhitelistRule::Exact("gigabyte".into()),
        WhitelistRule::Exact("logitech".into()),
        WhitelistRule::Exact("razer".into()),
        WhitelistRule::Exact("corsair".into()),
        WhitelistRule::Exact("steelseries".into()),
        // ==================== Runtimes and frameworks (exact match) ====================
        WhitelistRule::Exact(".net".into()),
        WhitelistRule::Exact("dotnet".into()),
        WhitelistRule::Exact("java".into()),
        WhitelistRule::Exact("python".into()),
        WhitelistRule::Prefix("python3".into()),
        WhitelistRule::Exact("node".into()),
        WhitelistRule::Exact("nodejs".into()),
        WhitelistRule::Exact("npm".into()),
        WhitelistRule::Exact("yarn".into()),
        WhitelistRule::Exact("rust".into()),
        WhitelistRule::Exact("cargo".into()),
        WhitelistRule::Exact("go".into()),
        WhitelistRule::Exact("golang".into()),
        // ==================== Dev tools (exact + prefix) ====================
        WhitelistRule::Exact("vscode".into()),
        WhitelistRule::Exact("visual studio".into()),
        WhitelistRule::Prefix("visual studio".into()),
        WhitelistRule::Exact("jetbrains".into()),
        WhitelistRule::Exact("git".into()),
        WhitelistRule::Exact("github".into()),
        WhitelistRule::Exact("docker".into()),
        WhitelistRule::Exact("wsl".into()),
        WhitelistRule::Exact("tauri".into()),
        WhitelistRule::Exact("electron".into()),
        WhitelistRule::Exact("flutter".into()),
        WhitelistRule::Exact("android".into()),
        WhitelistRule::Exact("gradle".into()),
        WhitelistRule::Exact("maven".into()),
        WhitelistRule::Exact("composer".into()),
        WhitelistRule::Exact("pip".into()),
        WhitelistRule::Exact("conda".into()),
        WhitelistRule::Exact("anaconda".into()),
        WhitelistRule::Exact("miniconda".into()),
        WhitelistRule::Exact("virtualenv".into()),
        WhitelistRule::Exact("pnpm".into()),
        WhitelistRule::Exact("bun".into()),
        WhitelistRule::Exact("deno".into()),
        // ==================== System services (exact match) ====================
        WhitelistRule::Exact("application data".into()),
        WhitelistRule::Exact("local settings".into()),
        WhitelistRule::Exact("history".into()),
        WhitelistRule::Exact("cookies".into()),
        WhitelistRule::Exact("cache".into()),
        WhitelistRule::Exact("caches".into()),
        WhitelistRule::Exact("logs".into()),
        WhitelistRule::Exact("crash reports".into()),
        WhitelistRule::Exact("crashdumps".into()),
        // Package manager caches belong to "junk cleanup".
        //
        // These two directories are "system/tool-level rebuildable data", not an individual app's own cache:
        // junk cleanup matches them exactly by path segment (`deep_junk::cache_names`) and scans more thoroughly than here.
        // Reporting on both sides means the user deletes in A, still sees the same entry in B, and thinks it wasn't deleted.
        WhitelistRule::Exact("npm-cache".into()),
        WhitelistRule::Exact("pnpm-cache".into()),
        // ==================== Common apps (exact match) ====================
        WhitelistRule::Exact("google".into()),
        WhitelistRule::Exact("chrome".into()),
        WhitelistRule::Exact("edge".into()),
        WhitelistRule::Exact("firefox".into()),
        WhitelistRule::Exact("mozilla".into()),
        WhitelistRule::Exact("opera".into()),
        WhitelistRule::Prefix("opera".into()),
        WhitelistRule::Exact("brave".into()),
        WhitelistRule::Exact("vivaldi".into()),
        WhitelistRule::Exact("wechat".into()),
        WhitelistRule::Exact("tencent".into()),
        WhitelistRule::Exact("qq".into()),
        WhitelistRule::Exact("discord".into()),
        WhitelistRule::Exact("slack".into()),
        WhitelistRule::Exact("zoom".into()),
        WhitelistRule::Exact("teams".into()),
        WhitelistRule::Exact("skype".into()),
        WhitelistRule::Exact("telegram".into()),
        WhitelistRule::Exact("telegram desktop".into()),
        WhitelistRule::Exact("whatsapp".into()),
        WhitelistRule::Exact("steam".into()),
        WhitelistRule::Exact("epic games".into()),
        WhitelistRule::Exact("ubisoft".into()),
        WhitelistRule::Exact("origin".into()),
        WhitelistRule::Exact("ea".into()),
        WhitelistRule::Exact("blizzard".into()),
        WhitelistRule::Exact("battle.net".into()),
        WhitelistRule::Exact("riot games".into()),
        WhitelistRule::Exact("adobe".into()),
        WhitelistRule::Exact("autodesk".into()),
        WhitelistRule::Exact("office".into()),
        WhitelistRule::Exact("onedrive".into()),
        WhitelistRule::Exact("dropbox".into()),
        WhitelistRule::Exact("icloud".into()),
        WhitelistRule::Exact("spotify".into()),
        WhitelistRule::Exact("vlc".into()),
        WhitelistRule::Exact("potplayer".into()),
        WhitelistRule::Exact("7-zip".into()),
        WhitelistRule::Exact("winrar".into()),
        WhitelistRule::Exact("bandizip".into()),
        // ==================== Video editing software (exact match) ====================
        WhitelistRule::Exact("jianyingpro".into()),
        WhitelistRule::Prefix("jianyingpro".into()),
        WhitelistRule::Exact("jianying".into()),
        WhitelistRule::Prefix("jianying".into()),
        WhitelistRule::Exact("capcut".into()),
        WhitelistRule::Prefix("capcut".into()),
        // ==================== Hidden folders (wildcard) ====================
        WhitelistRule::Pattern(".*".into()),
        // Video / creative tools
        WhitelistRule::Exact("davinci resolve".into()),
        WhitelistRule::Exact("blackmagic design".into()),
        WhitelistRule::Exact("obs-studio".into()),
        WhitelistRule::Exact("obs studio".into()),
        WhitelistRule::Exact("figma".into()),
        WhitelistRule::Exact("comfyui".into()),
    ]
}

// ============================================================================
// Known shared vendor directories (negative signal)
// These directories are likely shared by multiple apps and should not be marked as leftovers
// ============================================================================

/// Known shared vendor/system directory names (exact match, lowercase)
/// Deduct -0.5 when matched
const KNOWN_SHARED_VENDORS: &[&str] = &[
    "adobe",
    "microsoft",
    "google",
    "apple",
    "intel",
    "nvidia",
    "nvidia corporation",
    "amd",
    "realtek",
    "qualcomm",
    "broadcom",
    "dell",
    "hp",
    "lenovo",
    "asus",
    "msi",
    "gigabyte",
    "logitech",
    "razer",
    "corsair",
    "steelseries",
    "mozilla",
];

/// Generic directory names (negative signal, lowercase)
/// These names are too generic to be leftovers of a specific uninstalled app
/// Deduct -0.4 when matched
const GENERIC_FOLDER_NAMES: &[&str] = &[
    "cache",
    "caches",
    "logs",
    "log",
    "temp",
    "tmp",
    "data",
    "config",
    "settings",
    "preferences",
    "backup",
    "backups",
    "crash reports",
    "crashdumps",
    "diagnostics",
    "telemetry",
    "update",
    "updates",
    "downloads",
    "icons",
    "thumbnails",
];

// ============================================================================
// Emulator signature library
// ============================================================================

/// Emulator signature info
struct EmulatorSignature {
    /// Emulator name
    name: &'static str,
    /// Folder-name keywords (exact or prefix match, lowercase)
    folder_keywords: &'static [&'static str],
    /// Registry vendor-name keywords
    #[allow(dead_code)]
    registry_keywords: &'static [&'static str],
}

/// Known emulator signature library
const EMULATOR_SIGNATURES: &[EmulatorSignature] = &[
    // LDPlayer emulator (LDPlayer)
    EmulatorSignature {
        name: "雷电模拟器",
        folder_keywords: &["ldplayer", "leidian", "dnplayer", "changzhi"],
        registry_keywords: &["ldplayer", "changzhi", "xuanzhi"],
    },
    // BlueStacks emulator (BlueStacks)
    EmulatorSignature {
        name: "蓝叠模拟器",
        folder_keywords: &["bluestacks", "bluestacks_nxt", "bstk"],
        registry_keywords: &["bluestacks", "bluestack systems"],
    },
    // Nox emulator (Nox)
    EmulatorSignature {
        name: "夜神模拟器",
        folder_keywords: &["nox", "noxplayer", "bignox", "yeshen"],
        registry_keywords: &["nox", "bignox", "duodian"],
    },
    // MuMu emulator (NetEase)
    EmulatorSignature {
        name: "MuMu模拟器",
        folder_keywords: &["mumu", "nemu", "mumuemulator", "nemubox"],
        registry_keywords: &["mumu", "netease", "nemu"],
    },
    // MEmu emulator (Xiaoyao)
    EmulatorSignature {
        name: "MEmu模拟器",
        folder_keywords: &["memu", "microvirt", "xyaz"],
        registry_keywords: &["memu", "microvirt"],
    },
    // MSI App Player
    EmulatorSignature {
        name: "MSI App Player",
        folder_keywords: &["msi app player", "msiappplayer"],
        registry_keywords: &["msi app player"],
    },
    // Tencent Game Assistant
    EmulatorSignature {
        name: "腾讯手游助手",
        folder_keywords: &["txgameassistant", "gameloop", "tgp", "androidemulator"],
        registry_keywords: &["tencent", "gameloop"],
    },
];

/// Virtual disk file extensions
const VIRTUAL_DISK_EXTENSIONS: &[&str] = &[".vmdk", ".vdi", ".vhd", ".vhdx"];

/// Maximum recursion depth for filesystem probing
const FS_PROBE_MAX_DEPTH: usize = 4;

/// Executable file extensions (positive signal for filesystem probing)
const EXECUTABLE_EXTENSIONS: &[&str] = &["exe", "dll", "sys", "msi"];

/// Minimum size for orphaned root files.
///
/// Executables below this size are almost always placeholders (installer stubs);
/// deleting them saves no space and may break some still-used tool's self-check.
const LOOSE_FILE_MIN_SIZE: u64 = 64 * 1024;

/// Directory names to skip entirely when looking for virtual disks.
///
/// Virtual disks only appear in `LocalState` / `wsl` subdirectories under WSL / Docker / emulator locations,
/// while `node_modules`, `Temp`, and various `*Cache` cannot contain them - yet they make up most of the tree's directory count.
/// Measured before the change: one pass over the three AppData roots took 5.9 s with 0 hits, almost all of it spent in these directories.
const DISK_SCAN_SKIP_DIRS: &[&str] = &[
    "node_modules",
    "temp",
    "tmp",
    "cache",
    "caches",
    "code cache",
    "gpucache",
    "shadercache",
    "graphitedawncache",
    "logs",
    "log",
    "crashpad",
    "crashdumps",
    ".git",
    "site-packages",
    "__pycache__",
    "dist",
    "build",
    "obj",
    // ⚠️ Do NOT add `packages`: `Local\Packages\<distribution>\LocalState\ext4.vhdx` is exactly where WSL disks live;
    // skipping it would make this feature completely useless for WSL users.
];

/// Directory names where virtual disks **may** live. From the third level down, only these are entered.
///
/// The first two levels are app names (`Docker`, `Packages\<distribution>`); from the third level on it is no longer "who",
/// but "which subdirectory" - only a few names occur at that level. Directories not on the list
/// (`AC\Temp` under UWP packages, various `Settings`, `LocalCache`) cannot contain disks,
/// but they contribute the vast majority of the directory count.
const DISK_SCAN_DIR_HINTS: &[&str] = &[
    "localstate",
    "wsl",
    "data",
    "disks",
    "disk",
    "distro",
    "distros",
    "vm-data",
    "machines",
    "virtual machines",
    "vmware",
    "virtualbox vms",
    "docker",
];

/// High-confidence threshold: only at this score does it count as "very likely a leftover".
const HIGH_CONFIDENCE_SCORE: f32 = 0.75;

/// Minimum output threshold, also the lower bound of the "Suspicious" tier.
///
/// ⚠️ **These two numbers must be equal**; change one and you must change the other:
/// - threshold > bucket lower bound -> reaches the bucket (Suspicious) but is filtered out, wasted work;
/// - threshold < bucket lower bound -> enters results but the label says "possibly app data still in use", self-contradictory.
///
/// Set to 0.30 rather than 0.40 because the common combination `+0.20 has executable` + `+0.10 long unchanged`
/// equals exactly 0.30: leftover directories with an exe (updaters, portable apps) mostly only reach this tier.
/// 0.40 would make all such entries disappear, and they are exactly the ones users understand and dare to delete.
const SUSPICIOUS_SCORE: f32 = 0.30;

/// Entries under ProgramData below this score are classed as "system shared" rather than "suspicious".
///
/// A little activity in a system directory is not enough to call it a leftover; this branch was unreachable at threshold 0.40,
/// and only after lowering to 0.30 does the `[0.30, 0.40)` range land here - expected behavior.
const SYSTEM_SHARED_SCORE_LIMIT: f32 = 0.40;

// ============================================================================
// Installed-app map (path ownership inference)
// ============================================================================

/// Info about a single installed app
#[derive(Debug, Clone)]
struct InstalledAppInfo {
    /// Display name in the registry (original case)
    display_name: String,
    /// Install path (if any)
    install_location: Option<String>,
    /// Folder names inferred from the install path (lowercase)
    inferred_folder_names: Vec<String>,
}

/// Installed-app map
struct InstalledAppMap {
    /// All installed app info
    apps: Vec<InstalledAppInfo>,
    /// Fast lookup set: all known folder names (lowercase) -> list of corresponding app indices
    folder_to_app: HashMap<String, Vec<usize>>,
    /// Exact folder-name set (only from the last/second-to-last directory of InstallLocation)
    known_folders: HashSet<String>,
    /// Normalized DisplayName set (lowercase, version numbers and special characters removed)
    display_names: HashSet<String>,
    /// Suspected leftover candidates: folder names seen in historical InstallLocation but not found in the current registry
    leftover_candidates: HashSet<String>,
    /// Tokens of installed app names -> app indices.
    ///
    /// Why needed: directory names and DisplayNames are rarely character-for-character identical.
    /// A user with Playwright installed has a `ms-playwright` directory; one with VS Code has `Code`.
    /// Exact matching all fails, so "tools still in use" and "really uninstalled" land in the same tier,
    /// leaving users no basis to decide which to delete (20+ entries at the 0.30 tier all with the same score).
    /// After splitting into tokens, `ms-playwright` can match `playwright`.
    token_to_app: HashMap<String, Vec<usize>>,
    /// Tokens of uninstalled apps -> original names seen in history.
    ///
    /// Reverse evidence: a directory token matching an app that "was installed before but is no longer in the registry"
    /// is the very definition of "uninstall leftover", far more reliable than the two weak signals "has exe + long unchanged".
    uninstalled_tokens: HashMap<String, String>,
}

/// Normalize DisplayName: lowercase, remove version numbers, parenthetical content, and extra spaces
fn normalize_display_name(name: &str) -> String {
    let lower = name.to_lowercase();
    // Remove parenthetical content (e.g. "Foo App (x64)" -> "foo app")
    let no_parens = lower.replace(|c: char| c == '(' || c == ')', " ");
    // Remove version-number patterns (e.g. "v1.2.3", "1.0.0")
    let cleaned: String = no_parens
        .split_whitespace()
        .filter(|tok| !RE_VERSION_FOLDER.is_match(tok))
        .collect::<Vec<_>>()
        .join(" ");
    cleaned.trim().to_string()
}

/// Common parent directories not allowed as parent names in InstalledAppMap
const EXCLUDED_PARENT_DIRS: &[&str] = &[
    "program files",
    "program files (x86)",
    "programdata",
    "users",
];

/// Tokens excluded from matching after splitting directory names.
///
/// These words appear in almost every software's directory name (`xxx-desktop`, `xxx-updater`);
/// using them to decide "which app's directory this is" only creates noise:
/// if any Electron app is installed, every directory containing `desktop` is treated as its data.
const TOKEN_STOPWORDS: &[&str] = &[
    "app",
    "apps",
    "application",
    "applications",
    "desktop",
    "client",
    "service",
    "services",
    "updater",
    "update",
    "updates",
    "setup",
    "installer",
    "install",
    "uninstall",
    "helper",
    "launcher",
    "runtime",
    "editor",
    "viewer",
    "player",
    "manager",
    "studio",
    "suite",
    "tools",
    "tool",
    "utils",
    "utility",
    "common",
    "shared",
    "system",
    "program",
    "programs",
    "files",
    "folder",
    "cache",
    "data",
    "config",
    "settings",
    "user",
    "current",
    "version",
    "bin",
    "lib",
    "node",
    "electron",
    "chromium",
    "portable",
    "windows",
    "x64",
    "x86",
    "win32",
    "win64",
    "preview",
    "beta",
    "alpha",
    "nightly",
];

/// Split a directory name into tokens usable for comparison.
///
/// `ms-playwright` -> [`playwright`] (`ms` is too short)
/// `kimi-desktop-updater` -> [`kimi`] (the last two are stopwords)
/// `ollama app` → [`ollama`]
///
/// Keep only tokens of length >= 4 that are not stopwords and not purely numeric.
/// The threshold is 4 because two- or three-letter tokens like `ms`, `ai`, `vr` almost inevitably collide.
fn name_tokens(name_lower: &str) -> impl Iterator<Item = &str> {
    name_lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| token.len() >= 4)
        .filter(|token| !TOKEN_STOPWORDS.contains(token))
        .filter(|token| !token.chars().all(|c| c.is_ascii_digit()))
}

/// Build the "token -> app index" index from the installed app list.
///
/// Extracted separately so tests can specify "which apps are installed" themselves, without depending on this machine's registry.
fn build_token_index(apps: &[InstalledAppInfo]) -> HashMap<String, Vec<usize>> {
    let mut token_to_app: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, app) in apps.iter().enumerate() {
        let normalized = normalize_display_name(&app.display_name);
        for token in name_tokens(&normalized) {
            token_to_app.entry(token.to_string()).or_default().push(idx);
        }
    }
    token_to_app
}

/// Whether to descend into this directory during virtual disk search.
///
/// Extracted as a function so it can be unit-tested: the `WalkDir::filter_entry` closure cannot get a constructible `DirEntry`,
/// and if this rule is wrong, WSL users' `ext4.vhdx` will never be found again - must be pinned down.
///
/// Rules:
/// - matches `DISK_SCAN_SKIP_DIRS` -> never enter;
/// - first two levels (app names) -> always enter;
/// - from the third level -> only enter `DISK_SCAN_DIR_HINTS` (like `LocalState` / `wsl\data`).
fn should_descend_for_disks(name_lower: &str, depth: usize) -> bool {
    if DISK_SCAN_SKIP_DIRS.contains(&name_lower) {
        return false;
    }
    if depth >= 3 {
        return DISK_SCAN_DIR_HINTS.contains(&name_lower);
    }
    true
}

/// Infer folder names from InstallLocation. Kept pure so tests can use fixed fixtures instead of reading the local registry.
///
/// Take at most two levels: the final directory and its parent, excluding common parents such as Program Files.
/// **Do not split DisplayName into words** -- short tokens from the display name collide with unrelated directories and were the main source of false positives.
fn infer_folder_names(install_location: Option<&str>) -> Vec<String> {
    let Some(location) = install_location else {
        return Vec::new();
    };
    let location_path = Path::new(location);
    let mut inferred = Vec::new();

    // a. Final directory name: the app's own folder, most reliable
    if let Some(folder) = location_path.file_name() {
        let name = folder.to_string_lossy().to_lowercase();
        if !name.is_empty() {
            inferred.push(name);
        }
    }

    // b. Parent directory: matches the "vendor\\product" layout. Common parents must be excluded,
    //    otherwise every app under Program Files would also register "program files" as a folder name.
    if let Some(parent) = location_path.parent() {
        if let Some(vendor) = parent.file_name() {
            let vendor_name = vendor.to_string_lossy().to_lowercase();
            if !vendor_name.is_empty() && !EXCLUDED_PARENT_DIRS.contains(&vendor_name.as_str()) {
                inferred.push(vendor_name);
            }
        }
    }

    inferred.sort();
    inferred.dedup();
    inferred
}

impl InstalledAppMap {
    /// Build the installed-app map from the registry
    ///
    /// known_folders keeps only two sources:
    ///   a. the last directory name of the InstallLocation path (lowercase)
    ///   b. the second-to-last directory name, excluding common parent directories
    /// DisplayName tokens are no longer split, avoiding misjudgments from short-token collisions
    fn build() -> Self {
        let mut apps = Vec::new();
        let mut folder_to_app: HashMap<String, Vec<usize>> = HashMap::new();
        let mut known_folders = HashSet::new();
        let mut display_names = HashSet::new();

        // [Safety note] Only reads the registry; no write operations are performed
        let reg_paths = [
            (
                HKEY_LOCAL_MACHINE,
                r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
            ),
            (
                HKEY_LOCAL_MACHINE,
                r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
            ),
            (
                HKEY_CURRENT_USER,
                r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
            ),
        ];

        for (hkey, path) in reg_paths {
            if let Ok(key) = RegKey::predef(hkey).open_subkey_with_flags(path, KEY_READ) {
                for subkey_name in key.enum_keys().filter_map(|k| k.ok()) {
                    if let Ok(subkey) = key.open_subkey_with_flags(&subkey_name, KEY_READ) {
                        let display_name: String =
                            subkey.get_value("DisplayName").unwrap_or_default();
                        if display_name.is_empty() {
                            continue;
                        }

                        let install_location: Option<String> = subkey
                            .get_value::<String, _>("InstallLocation")
                            .ok()
                            .filter(|s| !s.is_empty());

                        // Normalize DisplayName and add to the set
                        let normalized = normalize_display_name(&display_name);
                        if !normalized.is_empty() {
                            display_names.insert(normalized);
                        }

                        // Infer folder names only from InstallLocation, do not split DisplayName tokens
                        let inferred = infer_folder_names(install_location.as_deref());

                        let app_idx = apps.len();
                        for name in &inferred {
                            folder_to_app.entry(name.clone()).or_default().push(app_idx);
                            known_folders.insert(name.clone());
                        }

                        apps.push(InstalledAppInfo {
                            display_name,
                            install_location,
                            inferred_folder_names: inferred,
                        });
                    }
                }
            }
        }

        // Load historical install folders and compute suspected leftover candidates
        // leftover_candidates = folders that appeared in InstallLocation before but are no longer in the current registry
        let mut historical = load_install_history();
        let leftover_candidates: HashSet<String> =
            historical.difference(&known_folders).cloned().collect();

        // Currently installed apps: DisplayName tokens -> app indices
        let token_to_app = build_token_index(&apps);

        // Uninstalled apps: tokens of historical names -> original historical names.
        // When a name contains a generic word (Doubao's InstallLocation was once recorded as `application`),
        // all split tokens are stopwords and naturally cannot enter this map - one piece of junk history won't misjudge a whole set of directories.
        let mut uninstalled_tokens: HashMap<String, String> = HashMap::new();
        for candidate in &leftover_candidates {
            for token in name_tokens(candidate) {
                uninstalled_tokens
                    .entry(token.to_string())
                    .or_insert_with(|| candidate.clone());
            }
        }

        // Merge currently known folders into history and persist
        historical.extend(known_folders.iter().cloned());
        save_install_history(&historical);

        log::info!(
            "已安装应用映射构建完成: {} 个应用, {} 个已知文件夹名, {} 个 DisplayName, {} 个历史残留候选, {} 个已卸载词元",
            apps.len(),
            known_folders.len(),
            display_names.len(),
            leftover_candidates.len(),
            uninstalled_tokens.len()
        );

        InstalledAppMap {
            apps,
            folder_to_app,
            known_folders,
            display_names,
            leftover_candidates,
            token_to_app,
            uninstalled_tokens,
        }
    }

    /// Check whether a folder name exactly matches an installed app's InstallLocation last directory
    fn has_exact_owner(&self, folder_name_lower: &str) -> bool {
        self.known_folders.contains(folder_name_lower)
    }

    /// Find the installed app corresponding to a folder name (exact InstallLocation match, returns app name)
    fn find_owner(&self, folder_name_lower: &str) -> Option<&str> {
        if let Some(indices) = self.folder_to_app.get(folder_name_lower) {
            if let Some(&idx) = indices.first() {
                return Some(&self.apps[idx].display_name);
            }
        }
        None
    }

    /// Check whether a folder name matches a known app's normalized DisplayName
    fn matches_display_name(&self, folder_name_lower: &str) -> bool {
        self.display_names.contains(folder_name_lower)
    }

    /// Whether a directory name belongs to an app **still installed** (token match + substring fallback).
    ///
    /// The difference from `matches_display_name` is "looseness": that one requires the whole name to match character-for-character,
    /// here a single matching token is enough. Directory `ms-playwright` vs `Playwright` is the kind of
    /// naming difference only token recognition can catch - otherwise "tools still in use" would be recommended to users as leftovers to delete.
    fn installed_app_for_name(&self, folder_name_lower: &str) -> Option<&str> {
        if let Some(owner) = name_tokens(folder_name_lower).find_map(|token| {
            self.token_to_app
                .get(token)
                .and_then(|indices| indices.first())
                .map(|&idx| self.apps[idx].display_name.as_str())
        }) {
            return Some(owner);
        }

        // Substring fallback: real directory names often concatenate two words and cannot be split
        // （`@genieworkbuddy-desktop-updater` → `genieworkbuddy`），
        // or the app name is only part of the directory name (`Comfy` vs `comfyui-desktop-2-updater`).
        // Length threshold 5: substrings of short names like `uv` or `code` almost inevitably hit something else.
        self.token_to_app
            .iter()
            .filter(|(token, _)| token.len() >= 5 && folder_name_lower.contains(token.as_str()))
            .max_by_key(|(token, _)| token.len())
            .and_then(|(_, indices)| indices.first())
            .map(|&idx| self.apps[idx].display_name.as_str())
    }

    /// Whether a directory name corresponds to an **already uninstalled** app (token match + substring fallback, returns the name seen in history).
    ///
    /// This is the most direct evidence of an "uninstall leftover": the app was once installed and is now gone from the registry.
    fn uninstalled_app_for_name(&self, folder_name_lower: &str) -> Option<&str> {
        if let Some(name) = name_tokens(folder_name_lower)
            .find_map(|token| self.uninstalled_tokens.get(token).map(|name| name.as_str()))
        {
            return Some(name);
        }

        self.uninstalled_tokens
            .iter()
            .filter(|(token, _)| token.len() >= 5 && folder_name_lower.contains(token.as_str()))
            .max_by_key(|(token, _)| token.len())
            .map(|(_, name)| name.as_str())
    }

    /// Structured path ownership inference: check whether a folder name maps to an installed app's InstallLocation
    fn has_inferred_ownership(&self, folder_name_lower: &str) -> bool {
        self.has_exact_owner(folder_name_lower)
    }

    /// Check whether a folder name is a historical leftover candidate (was in InstallLocation but not in the current registry)
    fn is_leftover_candidate(&self, folder_name_lower: &str) -> bool {
        self.leftover_candidates.contains(folder_name_lower)
    }
}

// ============================================================================
// Scoring engine
// ============================================================================

/// Scoring context (intermediate results for scoring a single directory)
struct ScoringContext {
    score: f32,
    reasons: Vec<String>,
}

impl ScoringContext {
    fn new() -> Self {
        // Baseline 0.0; all scores are driven by positive signals
        Self {
            score: 0.0,
            reasons: Vec::new(),
        }
    }

    fn add(&mut self, delta: f32, reason: String) {
        self.score += delta;
        self.reasons.push(reason);
    }

    /// Clamp the score to [0.0, 1.0]
    fn finalize(&mut self) {
        self.score = self.score.clamp(0.0, 1.0);
    }

    /// Determine the detection category from the final score
    fn category(&self) -> DetectionCategory {
        if self.score >= HIGH_CONFIDENCE_SCORE {
            DetectionCategory::HighConfidenceLeftover
        } else if self.score >= SUSPICIOUS_SCORE {
            DetectionCategory::Suspicious
        } else {
            DetectionCategory::LikelyAppData
        }
    }
}

// ============================================================================
// Filesystem probing
// ============================================================================

/// Filesystem probe result
struct FsProbeResult {
    /// Number of executable files found
    executable_count: u32,
    /// Whether uninstall*.exe was found
    has_uninstaller: bool,
    /// Total file count (within the probe depth)
    file_count: u32,
    /// Total size (bytes)
    total_size: u64,
}

/// Perform a limited-depth filesystem probe on a directory
fn probe_directory(path: &Path, max_depth: usize) -> FsProbeResult {
    let mut result = FsProbeResult {
        executable_count: 0,
        has_uninstaller: false,
        file_count: 0,
        total_size: 0,
    };

    for entry in WalkDir::new(path)
        .max_depth(max_depth)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }

        if let Ok(metadata) = entry.metadata() {
            result.total_size += metadata.len();
            result.file_count += 1;
        }

        let file_name = entry.file_name().to_string_lossy().to_lowercase();

        // Check executable files
        if let Some(ext) = entry.path().extension() {
            let ext_lower = ext.to_string_lossy().to_lowercase();
            if EXECUTABLE_EXTENSIONS.contains(&ext_lower.as_str()) {
                result.executable_count += 1;
            }
        }

        // Check for uninstallers
        if file_name.starts_with("uninstall") || file_name.starts_with("uninst") {
            if file_name.ends_with(".exe") {
                result.has_uninstaller = true;
            }
        }
    }

    result
}

// ============================================================================
// Uninstall leftover scanner
// ============================================================================

/// Uninstall leftover scanner (confidence scoring engine)
pub struct LeftoverScanner {
    /// Installed-app map
    app_map: InstalledAppMap,
    /// Structured whitelist rules
    whitelist: Vec<WhitelistRule>,
    /// Minimum folder size threshold (bytes)
    min_size_threshold: u64,
    /// Minimum days unmodified (for positive scoring)
    min_days_old: u64,
    /// Whether deep scan mode is enabled
    deep_scan: bool,
    /// Minimum output confidence threshold (entries below this score are not output)
    min_confidence_threshold: f32,
}

impl LeftoverScanner {
    /// Create a new scanner instance (full scan enabled by default, including emulator leftovers and virtual disk detection)
    pub fn new() -> Self {
        let app_map = InstalledAppMap::build();
        let whitelist = build_whitelist_rules();
        log::info!(
            "置信度评分引擎初始化: {} 个已安装应用, {} 条白名单规则",
            app_map.apps.len(),
            whitelist.len(),
        );

        LeftoverScanner {
            app_map,
            whitelist,
            min_size_threshold: 1024 * 1024, // 1MB
            min_days_old: 7,
            deep_scan: true,
            // Only output entries with score >= SUSPICIOUS_SCORE (0.30): the common combination
            // "has executable +0.20" + "long unchanged +0.10" lands right on this line
            min_confidence_threshold: SUSPICIOUS_SCORE,
        }
    }

    /// Compatible with the old interface; arguments ignored, full scan always enabled
    pub fn with_deep_scan(_deep_scan: bool) -> Self {
        Self::new()
    }

    /// Run the uninstall leftover scan
    pub fn scan(&self) -> LeftoverScanResult {
        // The user whitelist is read once at the start of a scan to avoid repeated disk IO during directory enumeration.
        let user_whitelist = leftover_whitelist::list_entries().unwrap_or_else(|error| {
            log::warn!(
                "读取卸载残留用户白名单失败，将按空白名单继续扫描: {}",
                error
            );
            Vec::new()
        });
        self.scan_with_paths(self.get_scan_paths(), &user_whitelist)
    }

    /// Scan with given root paths; tests can verify rules with temp directories without touching real user data.
    fn scan_with_paths(
        &self,
        scan_paths: Vec<(PathBuf, LeftoverSource)>,
        user_whitelist: &[leftover_whitelist::LeftoverWhitelistEntry],
    ) -> LeftoverScanResult {
        let start_time = std::time::Instant::now();
        let mut leftovers = Vec::new();
        let mut total_size = 0u64;
        // Diagnostic counters for empty results: let users know how many directories were scanned and how far the top score was off
        let mut scanned_directories = 0usize;
        let mut below_threshold = 0usize;
        let mut highest_confidence = 0f32;

        for (base_path, source) in &scan_paths {
            if !base_path.exists() {
                continue;
            }

            log::info!("扫描残留目录: {:?}", base_path);

            // Only scan first-level subdirectories
            if let Ok(entries) = fs::read_dir(base_path) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();

                    // Only process directories
                    if !path.is_dir() {
                        continue;
                    }

                    scanned_directories += 1;

                    // Get the folder name
                    let folder_name = match path.file_name() {
                        Some(name) => name.to_string_lossy().to_string(),
                        None => continue,
                    };

                    // Whitelist check (hard exclusion)
                    // The user whitelist takes priority over scoring logic, ensuring false-positive paths never reappear in results.
                    if leftover_whitelist::contains_path(user_whitelist, &path)
                        || self.is_whitelisted(&folder_name)
                    {
                        continue;
                    }

                    let folder_lower = folder_name.to_lowercase();

                    // Pre-filter: package-name format folder (com.xxx.yyy) -> skip directly
                    if RE_PACKAGE_NAME.is_match(&folder_lower) {
                        continue;
                    }
                    // Pre-filter: pure version-number folder (1.2.3.4, v2.0) -> skip directly
                    if RE_VERSION_FOLDER.is_match(&folder_lower) {
                        continue;
                    }

                    // Emulator detection (high-confidence short circuit)
                    let emulator_match = self.detect_emulator(&folder_name);

                    // Filesystem probing (limited depth)
                    let probe = probe_directory(&path, FS_PROBE_MAX_DEPTH);

                    // Empty shell directory: not a single file.
                    // Real uninstall leftovers often look exactly like this - the uninstaller cleared the contents, leaving only the directory,
                    // but it never passes the 1MB size gate, so recognize it first and let historical evidence decide the score.
                    let is_empty_shell = probe.file_count == 0;

                    // Size threshold filtering (empty shells excluded: whether they are leftovers is decided by history, not size)
                    let threshold = if emulator_match.is_some() {
                        100 * 1024 // Lower the threshold to 100KB for emulator leftovers
                    } else {
                        self.min_size_threshold
                    };
                    if !is_empty_shell && probe.total_size < threshold {
                        continue;
                    }

                    // Get last modified time
                    let last_modified = Self::get_last_modified(&path);

                    // Updater / cache directories: the name alone tells what they are, regardless of install or modification state
                    let is_updater_dir = RE_UPDATER_DIR.is_match(&folder_lower);
                    let is_cache_dir = !is_updater_dir && RE_CACHE_DIR.is_match(&folder_lower);

                    // ============ Scoring (baseline 0.0) ============
                    let mut ctx = ScoringContext::new();

                    if let Some(emu_name) = &emulator_match {
                        // [5] Emulator hit -> straight 0.90, skip other signals
                        ctx.score = 0.90;
                        ctx.reasons.push(format!("匹配已知模拟器: {}", emu_name));
                    } else {
                        // ---- Positive signals ----

                        // +0.35 Found uninstall*.exe / uninst*.exe in the folder
                        if probe.has_uninstaller {
                            ctx.add(0.35, "包含卸载程序残留 (uninstall*.exe)".into());
                        }

                        // +0.45 Updater directory.
                        // This scores the "consequence", not the "likelihood": after the program updates,
                        // these unpacked old versions are useless; deleting them only makes the updater re-download next time.
                        if is_updater_dir {
                            ctx.add(
                                0.45,
                                "更新器下载目录（删除后下次更新会重新下载，不影响已安装的程序）"
                                    .into(),
                            );
                        }

                        // +0.35 App's own cache/log directory: deleting it is regenerated on next program run
                        if is_cache_dir {
                            ctx.add(0.35, "缓存或日志目录（删除后程序会自行重新生成）".into());
                        }

                        // +0.25 Folder name matches a historical InstallLocation and the app is no longer in the registry
                        // (the history cache records all past InstallLocation folders;
                        //   if a folder appeared in history but is not in the current registry, the app is uninstalled)
                        if self.app_map.is_leftover_candidate(&folder_lower) {
                            ctx.add(
                                0.25,
                                format!("匹配已知安装路径但应用已卸载: {}", folder_name),
                            );
                        } else if let Some(uninstalled) =
                            self.app_map.uninstalled_app_for_name(&folder_lower)
                        {
                            // Token-level historical hit: directory names and install records are rarely character-for-character identical
                            // （`Comfy Desktop` vs `comfyui-desktop-2-updater`），
                            // Exact matching basically fails; this is the path where historical evidence really takes effect.
                            ctx.add(
                                0.20,
                                format!(
                                    "目录名与该应用的历史安装记录吻合（已卸载）: {}",
                                    uninstalled
                                ),
                            );
                        }

                        // +0.10 Empty shell directory: only together with the history hit above does it form "an empty directory left after uninstall"
                        if is_empty_shell {
                            ctx.add(0.10, "目录已空（只剩目录本身）".into());
                        }

                        // +0.20 Contains .exe or .dll files
                        if probe.executable_count > 0 {
                            ctx.add(
                                0.20,
                                format!("包含 {} 个可执行文件", probe.executable_count),
                            );
                        }

                        // Modification time (computed once)
                        let days_old = Self::get_days_since_modified(&path);

                        // +0.10 Not modified for more than min_days_old days
                        if days_old > self.min_days_old {
                            ctx.add(0.10, format!("已 {} 天未修改", days_old));
                        }

                        // ---- Negative signals ----

                        // -0.45 Folder name exactly matches a currently installed app's DisplayName.
                        // DisplayName comes from the current registry and means the software still has an install record; using it as a protective signal reduces the chance of harming data of software in use.
                        if self.app_map.matches_display_name(&folder_lower) {
                            ctx.add(
                                -0.45,
                                format!("匹配当前已安装应用 DisplayName: {}", folder_name),
                            );
                        } else if let Some(owner) =
                            self.app_map.installed_app_for_name(&folder_lower)
                        {
                            // -0.35 Token match to an app still installed.
                            // What exact matching misses is the most common form: `ms-playwright` / `Code` /
                            // `comfyui-desktop-2-updater` - they never match DisplayName;
                            // without a penalty they would appear in results as 0.30 items and be treated by users as "deletable".
                            ctx.add(-0.35, format!("目录名与仍在安装的应用吻合: {}", owner));
                        }

                        // -0.60 Folder name exactly matches an installed app's InstallLocation last directory
                        if let Some(owner) = self.app_map.find_owner(&folder_lower) {
                            ctx.add(-0.60, format!("映射到已安装应用: {}", owner));
                        }

                        // -0.40 Generic directory name
                        if GENERIC_FOLDER_NAMES.contains(&folder_lower.as_str()) {
                            ctx.add(-0.40, format!("通用目录名: {}", folder_name));
                        }

                        // -0.30 Located under ProgramData
                        if *source == LeftoverSource::ProgramData {
                            ctx.add(-0.30, "位于 ProgramData（系统共享目录）".into());
                        }

                        // -0.20 Modified within 7 days.
                        //
                        // Empty shell directories are excluded: their modification time almost inevitably equals "the moment contents were deleted",
                        // using it as evidence of "recently still in use" is backwards - it actually means the uninstall just happened.
                        if days_old < 7 && !is_empty_shell {
                            ctx.add(-0.20, format!("最近 {} 天内有修改", days_old));
                        }

                        // -0.15 Package-name format folder (pre-filter already skips most; kept here as a negative-signal fallback)
                        // (already skipped in the pre-filter stage, not repeated here)

                        // -0.15 Pure version-number folder
                        // (already skipped in the pre-filter stage, not repeated here)

                        // -0.50 Known shared vendor directory
                        if KNOWN_SHARED_VENDORS.contains(&folder_lower.as_str()) {
                            ctx.add(-0.50, format!("已知共享厂商目录: {}", folder_name));
                        }
                    }

                    ctx.finalize();

                    // Record diagnostics: what the top score is and how many entries are stuck below the threshold
                    highest_confidence = highest_confidence.max(ctx.score);

                    // Filter low-score entries (score < SUSPICIOUS_SCORE is not output)
                    if ctx.score < self.min_confidence_threshold {
                        below_threshold += 1;
                        continue;
                    }

                    let detection_category = if emulator_match.is_some() {
                        DetectionCategory::HighConfidenceLeftover
                    } else if is_updater_dir {
                        // More specific than "high confidence": for this category even the consequence of deletion can be stated clearly
                        DetectionCategory::UpdaterCache
                    } else if is_cache_dir {
                        DetectionCategory::CacheDir
                    } else if *source == LeftoverSource::ProgramData
                        && ctx.score < SYSTEM_SHARED_SCORE_LIMIT
                    {
                        DetectionCategory::SystemShared
                    } else {
                        ctx.category()
                    };

                    let leftover_type = if emulator_match.is_some() {
                        LeftoverType::Emulator
                    } else {
                        LeftoverType::Normal
                    };

                    leftovers.push(LeftoverEntry {
                        path: path.to_string_lossy().to_string(),
                        size: probe.total_size,
                        app_name: folder_name,
                        source: source.clone(),
                        last_modified,
                        file_count: probe.file_count,
                        is_emulator: emulator_match.is_some(),
                        is_virtual_disk: false,
                        leftover_type,
                        confidence: ctx.score,
                        detection_category,
                        reasons: ctx.reasons,
                    });

                    total_size += probe.total_size;
                }
            }
        }

        // [Deep scan] Scan for virtual disk files
        if self.deep_scan {
            log::info!("执行深度扫描: 搜索孤立虚拟磁盘文件...");
            let virtual_disks = self.scan_virtual_disk_files(&scan_paths, user_whitelist);
            for entry in virtual_disks {
                total_size += entry.size;
                leftovers.push(entry);
            }
        }

        // Orphaned executables in the root directory (like `Roaming\ollama app.exe`)
        for entry in self.scan_loose_files(&scan_paths, user_whitelist) {
            total_size += entry.size;
            leftovers.push(entry);
        }

        // Sort by confidence descending (ties broken by size descending)
        leftovers.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.size.cmp(&a.size))
        });

        let scan_duration_ms = start_time.elapsed().as_millis() as u64;
        log::info!(
            "卸载残留扫描完成: 发现 {} 个条目 (高置信度 {}, 可疑 {}), 总大小 {} 字节, 耗时 {}ms",
            leftovers.len(),
            leftovers
                .iter()
                .filter(|l| l.detection_category == DetectionCategory::HighConfidenceLeftover)
                .count(),
            leftovers
                .iter()
                .filter(|l| l.detection_category == DetectionCategory::Suspicious)
                .count(),
            total_size,
            scan_duration_ms
        );

        LeftoverScanResult {
            leftovers,
            total_size,
            scan_duration_ms,
            scanned_directories,
            below_threshold,
            highest_confidence,
            confidence_threshold: self.min_confidence_threshold,
        }
    }

    // ========================================================================
    // Private methods
    // ========================================================================

    /// Whitelist check (structured rule matching)
    fn is_whitelisted(&self, folder_name: &str) -> bool {
        let name_lower = folder_name.to_lowercase();
        self.whitelist.iter().any(|rule| rule.matches(&name_lower))
    }

    /// Check whether any ancestor directory of a file path is in the whitelist
    ///
    /// Starting from the file's parent directory, walk upward (up to base_dir),
    /// returning true if any level's directory name matches the whitelist.
    /// Used in scan_virtual_disk_files to prevent deep-nested virtual disks like WSL2 (\wsl\<GUID>\ext4.vhdx)
    /// from being misjudged - the immediate parent is a GUID, but the ancestor "wsl" is on the list.
    fn is_path_in_whitelist(&self, file_path: &Path, base_dir: &Path) -> bool {
        // Start from the file's parent directory, checking upward level by level
        let mut current = match file_path.parent() {
            Some(p) => p.to_path_buf(),
            None => return false,
        };

        while current.starts_with(base_dir) && current != base_dir {
            if let Some(name) = current.file_name() {
                let name_lower = name.to_string_lossy().to_lowercase();
                if self.is_whitelisted(&name_lower) {
                    return true;
                }
            }
            // Continue to the parent level
            current = match current.parent() {
                Some(p) => p.to_path_buf(),
                None => break,
            };
        }
        false
    }

    /// Emulator detection (exact/prefix matching, not global contains)
    /// Returns the matched emulator name (if any)
    fn detect_emulator(&self, folder_name: &str) -> Option<String> {
        let name_lower = folder_name.to_lowercase();

        for sig in EMULATOR_SIGNATURES {
            for keyword in sig.folder_keywords {
                // Use exact or prefix matching instead of contains
                if name_lower == *keyword || name_lower.starts_with(keyword) {
                    log::debug!("检测到模拟器残留: {} (匹配 {})", folder_name, sig.name);
                    return Some(sig.name.to_string());
                }
            }
        }

        None
    }

    /// [Deep scan] Scan for orphaned virtual disk files
    /// Scan **orphaned files** directly under the AppData / ProgramData roots.
    ///
    /// Uninstall leftovers are not only directories. Installers often drop a launcher directly in the `AppData\Roaming` root,
    /// and on uninstall only delete the items they registered - `Roaming\ollama app.exe` is exactly this:
    /// a single executable lying alone in the root, while the main flow only traverses directories (`if !path.is_dir() continue`)
    /// and never sees it.
    ///
    /// Only executables are collected: other files in the root are config or cache markers (`obs-virtualcam.txt`,
    /// `viap.json`, `IconCache.db`), whose deletion is either harmless or system-managed; reporting them is just noise.
    ///
    /// The base score is 0.25 rather than 0.30: an exe in the root is suspicious by itself, but one weak signal should not convict alone.
    /// To pass the 0.30 threshold, a second signal is required - "long unchanged" (+0.10) or "matches an uninstalled app" (+0.25).
    fn scan_loose_files(
        &self,
        scan_paths: &[(PathBuf, LeftoverSource)],
        user_whitelist: &[leftover_whitelist::LeftoverWhitelistEntry],
    ) -> Vec<LeftoverEntry> {
        let mut results = Vec::new();

        for (base_path, source) in scan_paths {
            let Ok(entries) = fs::read_dir(base_path) else {
                continue;
            };

            for entry in entries.filter_map(|e| e.ok()) {
                // Files only: directories are handled by the main flow under leftover rules
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if !file_type.is_file() {
                    continue;
                }

                let path = entry.path();
                let Some(extension) = path.extension() else {
                    continue;
                };
                if !EXECUTABLE_EXTENSIONS
                    .contains(&extension.to_string_lossy().to_lowercase().as_str())
                {
                    continue;
                }

                if leftover_whitelist::contains_path(user_whitelist, &path) {
                    continue;
                }

                let Some(file_name) = path.file_name().map(|n| n.to_string_lossy().to_string())
                else {
                    continue;
                };
                if self.is_whitelisted(&file_name) {
                    continue;
                }

                let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
                // A few-hundred-byte exe is usually just a placeholder marker; deleting or not is meaningless
                if size < LOOSE_FILE_MIN_SIZE {
                    continue;
                }

                let name_lower = file_name.to_lowercase();
                let days_old = Self::get_days_since_modified(&path);

                let mut ctx = ScoringContext::new();
                ctx.add(
                    0.25,
                    "直接位于 AppData / ProgramData 根目录下的可执行文件".into(),
                );

                if let Some(owner) = self.app_map.installed_app_for_name(&name_lower) {
                    // The same-named app is still installed: this is probably its own launcher, must not be touched
                    ctx.add(-0.45, format!("文件名与仍在安装的应用吻合: {}", owner));
                } else if let Some(uninstalled) = self.app_map.uninstalled_app_for_name(&name_lower)
                {
                    ctx.add(
                        0.25,
                        format!(
                            "文件名与该应用的历史安装记录吻合（已卸载）: {}",
                            uninstalled
                        ),
                    );
                }

                if days_old > self.min_days_old {
                    ctx.add(0.10, format!("已 {} 天未修改", days_old));
                }

                // ProgramData is a system shared directory; the same evidence is not enough to prove anything
                if *source == LeftoverSource::ProgramData {
                    ctx.add(-0.20, "位于 ProgramData（系统共享目录）".into());
                }

                ctx.finalize();
                if ctx.score < self.min_confidence_threshold {
                    continue;
                }

                log::info!(
                    "发现根目录下的孤立文件: {} ({} KB)",
                    path.display(),
                    size / 1024
                );

                results.push(LeftoverEntry {
                    path: path.to_string_lossy().to_string(),
                    size,
                    app_name: file_name,
                    source: source.clone(),
                    last_modified: Self::get_last_modified(&path),
                    file_count: 1,
                    is_emulator: false,
                    is_virtual_disk: false,
                    leftover_type: LeftoverType::LooseFile,
                    confidence: ctx.score,
                    detection_category: ctx.category(),
                    reasons: ctx.reasons,
                });
            }
        }

        results
    }

    fn scan_virtual_disk_files(
        &self,
        scan_paths: &[(PathBuf, LeftoverSource)],
        user_whitelist: &[leftover_whitelist::LeftoverWhitelistEntry],
    ) -> Vec<LeftoverEntry> {
        let mut results = Vec::new();

        // Share the same scan roots as the main flow: this used to define its own three-element array,
        // and once get_scan_paths() added or removed a location the two would disagree (directories found, disks not).
        // Parameterizing also lets tests verify pruning rules with temp directories without touching real AppData.
        for (base_dir, _source) in scan_paths {
            if !base_dir.exists() {
                continue;
            }

            // Recursively search for virtual disk files.
            //
            // Depth 4 is enough: WSL's `Local\Packages\<distribution>\LocalState\ext4.vhdx` and
            // Docker's `Local\Docker\wsl\data\ext4.vhdx` both land at this level.
            // It was 5; the extra level earned no hits, just traversed one more directory level.
            //
            // Pruning is the key: a full-tree pass takes 5.9 s, and the directory count is almost entirely from node_modules,
            // Temp, and package caches - virtual disks cannot be under them.
            //
            // Two pruning rules:
            //   ① Exclude obviously fragmented directories by name;
            //   ② From the third level, only enter directories in `DISK_SCAN_DIR_HINTS` -
            //      the first two levels are app names; from the third level only `LocalState` / `wsl\data` and a few such names remain.
            for entry in WalkDir::new(base_dir)
                .max_depth(6)
                .into_iter()
                .filter_entry(|entry| {
                    if !entry.file_type().is_dir() {
                        return true;
                    }
                    let name = entry.file_name().to_string_lossy().to_lowercase();
                    should_descend_for_disks(&name, entry.depth())
                })
                .filter_map(|e| e.ok())
            {
                let path = entry.path();

                // Only process files
                if !path.is_file() {
                    continue;
                }

                // The user whitelist can protect the virtual disk file itself or any of its ancestor directories.
                if leftover_whitelist::contains_path(user_whitelist, path) {
                    continue;
                }

                // Check the extension
                let ext = path
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
                    .unwrap_or_default();

                if !VIRTUAL_DISK_EXTENSIONS.contains(&ext.as_str()) {
                    continue;
                }

                // Get the file size
                let size = path.metadata().map(|m| m.len()).unwrap_or(0);

                // Virtual disk files are usually large; ignore anything under 100MB
                if size < 100 * 1024 * 1024 {
                    continue;
                }

                // Check whether any ancestor directory is in the whitelist
                // In WSL2-like cases: the file is at \wsl\<GUID>\ext4.vhdx and the immediate parent is a GUID,
                // but the ancestor "wsl" is on the whitelist, so it should be skipped
                if self.is_path_in_whitelist(path, base_dir) {
                    log::info!("虚拟磁盘文件路径命中白名单，跳过: {}", path.display());
                    continue;
                }

                // Check whether the parent directory corresponds to an installed app
                let parent_folder = path
                    .parent()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().to_lowercase())
                    .unwrap_or_default();

                if self.app_map.has_inferred_ownership(&parent_folder) {
                    continue;
                }

                let file_name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();

                let last_modified = Self::get_last_modified(path);

                log::info!(
                    "发现孤立虚拟磁盘文件: {} ({} MB)",
                    path.display(),
                    size / 1024 / 1024
                );

                results.push(LeftoverEntry {
                    path: path.to_string_lossy().to_string(),
                    size,
                    app_name: file_name,
                    source: LeftoverSource::VirtualDiskFile,
                    last_modified,
                    file_count: 1,
                    is_emulator: false,
                    is_virtual_disk: true,
                    leftover_type: LeftoverType::VirtualDisk,
                    confidence: 0.85,
                    detection_category: DetectionCategory::HighConfidenceLeftover,
                    reasons: vec!["孤立虚拟磁盘文件，未关联已安装应用".into()],
                });
            }
        }

        results
    }

    /// Get the list of paths to scan
    fn get_scan_paths(&self) -> Vec<(PathBuf, LeftoverSource)> {
        let mut paths = Vec::new();

        // AppData\Local
        if let Some(local_app_data) = dirs::data_local_dir() {
            paths.push((local_app_data.clone(), LeftoverSource::LocalAppData));

            // AppData\LocalLow (common location for emulator leftovers)
            if let Some(parent) = local_app_data.parent() {
                let local_low = parent.join("LocalLow");
                if local_low.exists() {
                    paths.push((local_low, LeftoverSource::LocalLowAppData));
                }
            }
        }

        // AppData\Roaming
        if let Some(roaming_app_data) = dirs::data_dir() {
            paths.push((roaming_app_data, LeftoverSource::RoamingAppData));
        }

        // ProgramData
        let program_data = PathBuf::from(r"C:\ProgramData");
        if program_data.exists() {
            paths.push((program_data, LeftoverSource::ProgramData));
        }

        paths
    }

    /// Get a folder's last modified time (Unix timestamp)
    fn get_last_modified(path: &Path) -> i64 {
        if let Ok(metadata) = fs::metadata(path) {
            if let Ok(modified) = metadata.modified() {
                if let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH) {
                    return duration.as_secs() as i64;
                }
            }
        }
        0
    }

    /// Get the number of days since last modification
    fn get_days_since_modified(path: &Path) -> u64 {
        if let Ok(metadata) = fs::metadata(path) {
            if let Ok(modified) = metadata.modified() {
                let age = SystemTime::now()
                    .duration_since(modified)
                    .unwrap_or(Duration::ZERO);
                return age.as_secs() / (24 * 60 * 60);
            }
        }
        0
    }
}

impl Default for LeftoverScanner {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Uninstall leftover deletion operations
// ============================================================================

/// Uninstall leftover deletion result
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LeftoverDeleteResult {
    /// Number of folders successfully deleted
    pub deleted_count: u32,
    /// Space freed (bytes)
    pub deleted_size: u64,
    /// Paths that failed to delete (including those skipped for containing executables; frontend indexes into `errors` by position)
    pub failed_paths: Vec<String>,
    /// List of error messages
    pub errors: Vec<String>,
    /// Paths skipped for containing executables
    pub skipped_executables: Vec<String>,
}

/// Shallow executable scan; returns a warning message if any are found
///
/// [Note]
/// Quickly scan the shallow layers (depth=3) of the target folder; if .exe/.dll/.sys files are found,
/// the directory may contain portable software or system components still in use.
/// Returning Some(message) means executables were found and deletion should be skipped.
fn has_executables_shallow(path: &Path) -> Option<String> {
    const EXE_EXTENSIONS: &[&str] = &["exe", "dll", "sys"];
    let max_depth = 3;
    let max_found = 5;
    let mut found = 0u32;
    let mut example = String::new();

    for entry in WalkDir::new(path)
        .max_depth(max_depth)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        if let Some(ext) = entry.path().extension() {
            let ext_lower = ext.to_string_lossy().to_lowercase();
            if EXE_EXTENSIONS.contains(&ext_lower.as_str()) {
                found += 1;
                if example.is_empty() {
                    example = entry.file_name().to_string_lossy().to_string();
                }
                if found >= max_found {
                    break;
                }
            }
        }
    }

    if found > 0 {
        if found == 1 {
            Some(format!(
                "文件夹包含可执行文件 (如 {})，已跳过，请使用深度清理手动确认",
                example
            ))
        } else {
            Some(format!(
                "文件夹包含 {} 个可执行文件 (如 {})，已跳过，请使用深度清理手动确认",
                found, example
            ))
        }
    } else {
        None
    }
}

/// Delete uninstall leftover folders (with safety pre-checks)
///
/// [Note]
/// For each path, in order: path-scope check -> classify (directory / file) -> executable scan -> delete.
/// **Directories** containing executables are not deleted; they are marked as skipped_executables
/// and the user is guided to handle them via deep cleanup (which includes manual review).
pub fn delete_folders(paths: Vec<String>) -> LeftoverDeleteResult {
    let whitelist_entries = match leftover_whitelist::list_entries() {
        Ok(entries) => entries,
        Err(error) => {
            // If the whitelist cannot be read, refuse to delete: better to ask the user to fix the config than let protection rules fail.
            return LeftoverDeleteResult {
                deleted_count: 0,
                deleted_size: 0,
                failed_paths: paths.clone(),
                errors: paths
                    .iter()
                    .map(|path| format!("无法读取用户白名单，已拒绝删除 {}: {}", path, error))
                    .collect(),
                skipped_executables: Vec::new(),
            };
        }
    };
    delete_folders_with_whitelist(paths, &whitelist_entries)
}

/// Allowed deletion roots, aligned with the scan scope of `get_scan_paths()`.
///
/// If the two disagree, "found but cannot delete" occurs (a judgment falls outside the scan scope).
fn deletion_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(local) = dirs::data_local_dir() {
        roots.push(local.clone());
        // LocalLow is at the same level as Local; emulator leftovers often land here
        if let Some(parent) = local.parent() {
            let local_low = parent.join("LocalLow");
            if local_low.exists() {
                roots.push(local_low);
            }
        }
    }
    if let Some(roaming) = dirs::data_dir() {
        roots.push(roaming);
    }
    let program_data = PathBuf::from(r"C:\ProgramData");
    if program_data.exists() {
        roots.push(program_data);
    }
    roots
}

/// Shared by normal deletion and tests, ensuring even stale scan results cannot bypass user protection rules.
fn delete_folders_with_whitelist(
    paths: Vec<String>,
    whitelist_entries: &[leftover_whitelist::LeftoverWhitelistEntry],
) -> LeftoverDeleteResult {
    delete_folders_in(paths, whitelist_entries, &deletion_roots())
}

fn delete_folders_in(
    paths: Vec<String>,
    whitelist_entries: &[leftover_whitelist::LeftoverWhitelistEntry],
    roots: &[PathBuf],
) -> LeftoverDeleteResult {
    let mut deleted_count = 0u32;
    let mut deleted_size = 0u64;
    let mut failed_paths = Vec::new();
    let mut errors = Vec::new();
    let mut skipped_executables = Vec::new();

    for path in paths {
        let path_buf = PathBuf::from(&path);

        // Re-check before deleting so a whitelist update cannot still be bypassed by old scan results.
        if leftover_whitelist::contains_path(whitelist_entries, &path_buf) {
            failed_paths.push(path.clone());
            errors.push(format!("路径受用户白名单保护: {}", path));
            continue;
        }

        // Safety check 1: path is within the allowed scope
        if !is_safe_leftover_path_under(&path_buf, roots) {
            failed_paths.push(path.clone());
            errors.push(format!("路径不在允许的目录内: {}", path));
            continue;
        }

        // Safety check 2: determine type. Orphaned virtual disks (like `ext4.vhdx`) are **files**,
        // and would inevitably report "invalid directory name" through `remove_dir_all`; what the user sees is
        // "tens of GB of leftovers cannot be deleted", so split them out here first.
        // junctions / directory symlinks do not count as is_file and still take the directory branch - `remove_dir_all`
        // deletes only the link itself and does not follow into the target.
        let metadata = match fs::symlink_metadata(&path_buf) {
            Ok(metadata) => metadata,
            Err(error) => {
                failed_paths.push(path.clone());
                errors.push(format!("无法读取 {}: {}", path, error));
                continue;
            }
        };

        if metadata.is_file() {
            let file_size = metadata.len();
            match fs::remove_file(&path_buf) {
                Ok(()) => {
                    deleted_count += 1;
                    deleted_size += file_size;
                }
                Err(error) => {
                    failed_paths.push(path.clone());
                    errors.push(format!("删除失败 {}: {}", path, error));
                }
            }
            continue;
        }

        // Safety check 3: shallow executable scan (only meaningful for directories)
        if let Some(msg) = has_executables_shallow(&path_buf) {
            skipped_executables.push(path.clone());
            // A skip also counts as "not deleted" and goes into failed_paths as well:
            // the frontend indexes into errors by the position in failed_paths;
            // pushing one fewer here would shift the error messages of all later entries.
            failed_paths.push(path.clone());
            errors.push(format!("{}: {}", path, msg));
            continue;
        }

        // Compute folder size before deleting
        let folder_size = calculate_dir_size(&path_buf);

        match fs::remove_dir_all(&path_buf) {
            Ok(_) => {
                deleted_count += 1;
                deleted_size += folder_size;
            }
            Err(e) => {
                failed_paths.push(path.clone());
                errors.push(format!("删除失败 {}: {}", path, e));
            }
        }
    }

    LeftoverDeleteResult {
        deleted_count,
        deleted_size,
        failed_paths,
        errors,
        skipped_executables,
    }
}

/// Recursively compute directory size
fn calculate_dir_size(path: &std::path::Path) -> u64 {
    let mut size = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            // `file_type()` does not follow symlinks / junctions; using `is_dir()` would follow them:
            // one, it would count capacity from other drives; two, a loop pointing upward would recurse infinitely.
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                size += calculate_dir_size(&entry.path());
            } else if file_type.is_file() {
                if let Ok(metadata) = entry.metadata() {
                    size += metadata.len();
                }
            }
        }
    }
    size
}

/// Check whether a path is within the allowed deletion scope (root injectable for testing).
///
/// [Safety note]
/// All three constraints are required:
/// 1. **Reject `.` / `..` components** - `remove_dir_all` acts on the resolved real path,
///    while prefix matching is done on the literal. `...\Local\..\..\..\Windows` would pass validation
///    as "under AppData\Local" while actually deleting a different directory.
/// 2. **Compare by components, not string prefix** - string `starts_with` would allow
///    `AppData\LocalBackup` (its first 19 characters are the same as `AppData\Local`).
/// 3. **Directories must be direct children of the root** - the scanner only produces first-level directories;
///    deeper directories should not be wholesale deleted by this module; files (orphaned virtual disks) may go deeper,
///    as their real location is inside `Packages\<package name>\LocalState\`.
fn is_safe_leftover_path_under(path: &Path, roots: &[PathBuf]) -> bool {
    if crate::path_guard::has_relative_components(path) {
        return false;
    }

    let path_depth = path.components().count();
    let is_file = fs::symlink_metadata(path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false);

    roots.iter().any(|root| {
        // Strictly below the root: the root itself cannot be treated as a leftover and deleted
        if path_depth <= root.components().count() {
            return false;
        }
        if !starts_with_ignore_case(path, root) {
            return false;
        }
        if is_file {
            return true;
        }
        path.parent()
            .map(|parent| components_eq_ignore_case(parent, root))
            .unwrap_or(false)
    })
}

/// Compare paths by components, case-insensitively (Windows paths are case-insensitive).
fn starts_with_ignore_case(path: &Path, root: &Path) -> bool {
    let mut path_components = path.components();
    for root_component in root.components() {
        let Some(path_component) = path_components.next() else {
            return false;
        };
        if !path_component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&root_component.as_os_str().to_string_lossy())
        {
            return false;
        }
    }
    true
}

/// Whether two paths are fully equivalent (by components, case-insensitive).
fn components_eq_ignore_case(left: &Path, right: &Path) -> bool {
    let mut left_components = left.components();
    let mut right_components = right.components();
    loop {
        match (left_components.next(), right_components.next()) {
            (None, None) => return true,
            (Some(left_component), Some(right_component)) => {
                if !left_component
                    .as_os_str()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&right_component.as_os_str().to_string_lossy())
                {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_whitelist_entry(path: &Path) -> leftover_whitelist::LeftoverWhitelistEntry {
        leftover_whitelist::LeftoverWhitelistEntry {
            path: path.to_string_lossy().to_string(),
            added_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn user_whitelist_excludes_synthetic_leftover_and_blocks_normal_deletion() {
        let root = std::env::temp_dir().join(format!(
            "lightc-leftover-scan-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("读取测试时间失败")
                .as_nanos()
        ));
        let candidate = root.join("leidian_whitelist_fixture");
        fs::create_dir_all(&candidate).expect("创建模拟残留目录失败");
        // Emulator signatures and above-threshold content ensure the fixture passes the real scanner's filters, rather than only testing string filtering.
        fs::write(candidate.join("uninstall.exe"), vec![0u8; 1024 * 1024])
            .expect("创建模拟卸载程序失败");

        let mut scanner = LeftoverScanner::new();
        scanner.deep_scan = false;
        let scan_paths = vec![(root.clone(), LeftoverSource::LocalAppData)];
        let unprotected_result = scanner.scan_with_paths(scan_paths.clone(), &[]);
        assert!(unprotected_result
            .leftovers
            .iter()
            .any(|entry| entry.path == candidate.to_string_lossy()));

        let whitelist = vec![test_whitelist_entry(&candidate)];
        let protected_result = scanner.scan_with_paths(scan_paths, &whitelist);
        assert!(protected_result.leftovers.is_empty());

        let deletion = delete_folders_with_whitelist(
            vec![candidate.to_string_lossy().to_string()],
            &whitelist,
        );
        assert_eq!(deletion.deleted_count, 0);
        assert_eq!(deletion.failed_paths.len(), 1);
        assert!(candidate.exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_whitelist_exact() {
        let rules = build_whitelist_rules();
        // Exact match
        assert!(rules.iter().any(|r| r.matches("microsoft")));
        assert!(rules.iter().any(|r| r.matches("nvidia corporation")));
        // Hidden-folder wildcard
        assert!(rules.iter().any(|r| r.matches(".vscode")));
        assert!(rules.iter().any(|r| r.matches(".git")));
        // No match for random names
        assert!(!rules.iter().any(|r| r.matches("somerandomapp")));
    }

    #[test]
    fn test_whitelist_no_global_contains() {
        // Ensure the whitelist does not falsely match because of contains
        let rules = build_whitelist_rules();
        // "microsoftedge" should not be matched by the "microsoft" exact rule
        assert!(!rules.iter().any(|r| r.matches("microsoftedge")));
        // "amdgpu_settings" should not be matched by the "amd" exact rule
        assert!(!rules.iter().any(|r| r.matches("amdgpu_settings")));
    }

    #[test]
    fn test_scoring_context() {
        let mut ctx = ScoringContext::new();
        // Baseline 0.0
        assert!((ctx.score - 0.0).abs() < f32::EPSILON);

        ctx.add(0.40, "测试正向信号".into());
        ctx.add(0.35, "卸载程序残留".into());
        ctx.finalize();
        assert!((ctx.score - 0.75).abs() < 0.01);
        assert_eq!(ctx.category(), DetectionCategory::HighConfidenceLeftover);
    }

    #[test]
    fn test_scoring_clamped() {
        let mut ctx = ScoringContext::new();
        ctx.add(1.5, "超大正向".into());
        ctx.finalize();
        assert!((ctx.score - 1.0).abs() < f32::EPSILON);

        let mut ctx2 = ScoringContext::new();
        ctx2.add(-2.0, "超大负向".into());
        ctx2.finalize();
        assert!((ctx2.score - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_detection_category_thresholds() {
        // score >= 0.75 → HighConfidenceLeftover
        let mut ctx = ScoringContext::new();
        ctx.score = 0.75;
        assert_eq!(ctx.category(), DetectionCategory::HighConfidenceLeftover);

        // 0.30 <= score < 0.75 → Suspicious
        ctx.score = 0.50;
        assert_eq!(ctx.category(), DetectionCategory::Suspicious);

        // The threshold itself is the lower bound of Suspicious: 0.30 must land in "suspicious",
        // otherwise if the threshold is changed but bucketing is not, entries appear in results labeled "possibly app data still in use"
        ctx.score = SUSPICIOUS_SCORE;
        assert_eq!(ctx.category(), DetectionCategory::Suspicious);

        // Below threshold -> LikelyAppData (normally already filtered; purely defensive)
        ctx.score = SUSPICIOUS_SCORE - 0.01;
        assert_eq!(ctx.category(), DetectionCategory::LikelyAppData);
    }

    #[test]
    fn test_generic_folder_names() {
        assert!(GENERIC_FOLDER_NAMES.contains(&"cache"));
        assert!(GENERIC_FOLDER_NAMES.contains(&"logs"));
        assert!(GENERIC_FOLDER_NAMES.contains(&"temp"));
        assert!(!GENERIC_FOLDER_NAMES.contains(&"foobar"));
    }

    #[test]
    fn test_known_shared_vendors() {
        assert!(KNOWN_SHARED_VENDORS.contains(&"adobe"));
        assert!(KNOWN_SHARED_VENDORS.contains(&"microsoft"));
        assert!(!KNOWN_SHARED_VENDORS.contains(&"somerandomvendor"));
    }

    // ===== New tests =====

    #[test]
    fn test_score_starts_at_zero() {
        // Verify ScoringContext::new() starts at 0.0
        let ctx = ScoringContext::new();
        assert!((ctx.score - 0.0).abs() < f32::EPSILON, "基线分应为 0.0");
    }

    #[test]
    fn test_package_name_filtered() {
        // Verify package-name format folders are skipped by the pre-filter
        assert!(
            RE_PACKAGE_NAME.is_match("com.example.app"),
            "com.example.app 应匹配包名格式"
        );
        assert!(
            RE_PACKAGE_NAME.is_match("org.apache.commons.lang"),
            "org.apache.commons.lang 应匹配"
        );
        assert!(
            !RE_PACKAGE_NAME.is_match("steamapp"),
            "steamapp 不应匹配包名格式"
        );
        assert!(
            !RE_PACKAGE_NAME.is_match("com.x"),
            "com.x 只有两段，不应匹配"
        );
    }

    #[test]
    fn test_version_folder_filtered() {
        // Verify pure version-number folders are skipped by the pre-filter
        assert!(
            RE_VERSION_FOLDER.is_match("1.2.3.4"),
            "1.2.3.4 应匹配版本号格式"
        );
        assert!(RE_VERSION_FOLDER.is_match("v2.0"), "v2.0 应匹配版本号格式");
        assert!(
            RE_VERSION_FOLDER.is_match("10.0.1"),
            "10.0.1 应匹配版本号格式"
        );
        assert!(
            !RE_VERSION_FOLDER.is_match("v2"),
            "v2 没有点号分隔，不应匹配"
        );
        assert!(
            !RE_VERSION_FOLDER.is_match("foobar"),
            "foobar 不应匹配版本号格式"
        );
    }

    #[test]
    fn test_high_confidence_threshold() {
        // Verify threshold >= 0.75 is classified as HighConfidenceLeftover
        let mut ctx = ScoringContext::new();
        ctx.score = 0.75;
        assert_eq!(
            ctx.category(),
            DetectionCategory::HighConfidenceLeftover,
            "score = 0.75 应为 HighConfidenceLeftover"
        );

        ctx.score = 0.74;
        assert_eq!(
            ctx.category(),
            DetectionCategory::Suspicious,
            "score = 0.74 应为 Suspicious"
        );

        ctx.score = SUSPICIOUS_SCORE;
        assert_eq!(
            ctx.category(),
            DetectionCategory::Suspicious,
            "score = 0.30 应为 Suspicious（门槛与分档下限是同一个数）"
        );

        ctx.score = SUSPICIOUS_SCORE - 0.01;
        assert_eq!(
            ctx.category(),
            DetectionCategory::LikelyAppData,
            "score = 0.29 应为 LikelyAppData"
        );
    }

    #[test]
    fn infer_folder_names_uses_install_location_only() {
        // Use fixed inputs instead of the local registry: the old test depended on installed software,
        // so its result varied by machine while a clean CI runner hid the problem (audit M14).
        // These four structural invariants are what actually need to hold.
        //
        // 1) Normal "vendor\\product" layout: take both levels
        assert_eq!(
            infer_folder_names(Some(r"C:\Program Files\SomeVendor\SomeApp")),
            vec!["someapp".to_string(), "somevendor".to_string()]
        );

        // 2) Common parents are not folder names, otherwise every Program Files app records "program files"
        assert_eq!(
            infer_folder_names(Some(r"C:\Program Files\SomeApp")),
            vec!["someapp".to_string()]
        );
        assert_eq!(
            infer_folder_names(Some(r"C:\ProgramData\SomeApp")),
            vec!["someapp".to_string()]
        );

        // 3) All-digit directory names are legal and must be kept as-is.
        //    The old test treated them as tokenization debris and therefore produced false
        //    positives on any machine with a real `...\\18` directory.
        assert_eq!(
            infer_folder_names(Some(r"D:\Tools\18")),
            vec!["18".to_string(), "tools".to_string()]
        );

        // 4) Missing or empty paths produce no folder names
        assert!(infer_folder_names(None).is_empty());
        assert!(infer_folder_names(Some("")).is_empty());
    }

    // ========================================================================
    // v2.4.3 fix: whitelist covers WSL2 virtual disks & JianyingPro workspace
    // ========================================================================

    #[test]
    fn test_whitelist_wsl_and_jianying() {
        let rules = build_whitelist_rules();

        // === WSL2 scenario ===
        // Microsoft Store version of WSL (Ubuntu and other distributions)
        let store_path = Path::new(
            r"C:\Users\test_user\AppData\Local\Packages\CanonicalGroupLimited.Ubuntu_79rhkp1fndgsc\LocalState\ext4.vhdx",
        );
        // Non-Store WSL
        let legacy_wsl_path = Path::new(
            r"C:\Users\test_user\AppData\Local\wsl\a1b2c3d4-e5f6-7890-abcd-ef1234567890\ext4.vhdx",
        );

        // === JianyingPro scenario ===
        let jianying_pro_path =
            Path::new(r"C:\Users\test_user\AppData\Local\JianyingPro\User Data\Projects");
        let jianying_drafts_path =
            Path::new(r"C:\Users\test_user\AppData\Local\JianyingPro Drafts\xxx");
        let capcut_path = Path::new(r"C:\Users\test_user\AppData\Roaming\CapCut\User Data");

        // === Real leftover scenario ===
        let real_leftover_path =
            Path::new(r"C:\Users\test_user\AppData\Local\SomeRandomUninstalledApp\cache");

        // Verify: the whitelist contains key directory names
        assert!(rules.iter().any(|r| r.matches("wsl")), "wsl 应在白名单中");
        assert!(
            rules.iter().any(|r| r.matches("packages")),
            "packages 应在白名单中"
        );
        assert!(
            rules.iter().any(|r| r.matches("jianyingpro")),
            "jianyingpro 应在白名单中"
        );
        assert!(
            rules.iter().any(|r| r.matches("jianyingpro drafts")),
            "jianyingpro drafts 应在白名单中（Prefix 匹配）"
        );
        assert!(
            rules.iter().any(|r| r.matches("jianying")),
            "jianying 应在白名单中"
        );
        assert!(
            rules.iter().any(|r| r.matches("capcut")),
            "capcut 应在白名单中"
        );

        // Verify: is_path_in_whitelist correctly identifies protected paths
        let scanner = LeftoverScanner::new();
        // The mock path base directory must share the test path's prefix for strip_prefix to succeed
        let mock_local = PathBuf::from(r"C:\Users\test_user\AppData\Local");
        let mock_roaming = PathBuf::from(r"C:\Users\test_user\AppData\Roaming");

        // Store version of WSL: Packages is on the whitelist -> should be skipped
        assert!(
            scanner.is_path_in_whitelist(store_path, &mock_local),
            "Microsoft Store WSL 路径应命中白名单 (Packages)"
        );
        // Non-Store WSL: wsl is on the whitelist -> should be skipped
        assert!(
            scanner.is_path_in_whitelist(legacy_wsl_path, &mock_local),
            "传统 WSL 路径应命中白名单 (wsl)"
        );
        // JianyingPro
        assert!(
            scanner.is_path_in_whitelist(jianying_pro_path, &mock_local),
            "剪映专业版路径应命中白名单 (JianyingPro)"
        );
        // JianyingPro drafts directory
        assert!(
            scanner.is_path_in_whitelist(jianying_drafts_path, &mock_local),
            "剪映草稿目录应命中白名单 (JianyingPro Drafts)"
        );
        // CapCut international version
        assert!(
            scanner.is_path_in_whitelist(capcut_path, &mock_roaming),
            "CapCut 路径应命中白名单 (CapCut)"
        );
        // Real leftover: SomeRandomUninstalledApp is not on the whitelist -> should not be skipped
        assert!(
            !scanner.is_path_in_whitelist(real_leftover_path, &mock_local),
            "真实残留路径不应命中白名单"
        );
    }

    /// Temp directory for tests (with process id and timestamp, so concurrent cases don't clash).
    fn case_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "lightc-leftover-{}-{}-{}",
            tag,
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("读取测试时间失败")
                .as_nanos()
        ))
    }

    /// The three bottoms of path validation: reject `..`, compare by components, allow directories only one level deep.
    ///
    /// The old implementation used string `starts_with`; both `AppData\LocalBackup` and
    /// `AppData\Local\..\..\..\Windows` would pass - while `remove_dir_all`
    /// acts on the resolved real path.
    #[test]
    fn safe_path_check_rejects_traversal_and_prefix_lookalikes() {
        let base = case_dir("safe-path");
        let root = base.join("Local");
        let app_dir = root.join("SomeApp");
        let nested_dir = app_dir.join("Config");
        let virtual_disk = app_dir
            .join("Packages")
            .join("SomePackage")
            .join("LocalState")
            .join("ext4.vhdx");
        fs::create_dir_all(&nested_dir).expect("创建测试目录失败");
        fs::create_dir_all(virtual_disk.parent().expect("虚拟磁盘应有父目录"))
            .expect("创建虚拟磁盘目录失败");
        fs::write(&virtual_disk, vec![0u8; 64]).expect("写入虚拟磁盘夹具失败");

        let roots = vec![root.clone()];

        // First-level subdirectory: allowed (the scanner only produces this level)
        assert!(is_safe_leftover_path_under(&app_dir, &roots));
        // The root itself: not allowed
        assert!(!is_safe_leftover_path_under(&root, &roots));
        // Sibling prefix: string prefix matching would allow it; component comparison must block it
        assert!(!is_safe_leftover_path_under(
            &base.join("LocalBackup").join("x"),
            &roots
        ));
        // `..` traversal: the literal looks under the root, but actually deletes a different directory
        assert!(!is_safe_leftover_path_under(
            &app_dir.join("..").join("..").join("Windows"),
            &roots
        ));
        // Second-level subdirectory: wholesale deletion is outside this module's responsibility
        assert!(!is_safe_leftover_path_under(&nested_dir, &roots));
        // Deep file (orphaned virtual disk): allowed; its real location is inside Packages
        assert!(is_safe_leftover_path_under(&virtual_disk, &roots));

        let _ = fs::remove_dir_all(&base);
    }

    /// An orphaned virtual disk is a **file** and must go through `remove_file`.
    ///
    /// The old implementation always used `remove_dir_all`, which inevitably reports "invalid directory name" for files -
    /// and WSL's `ext4.vhdx` is often tens of GB, the most valuable entry.
    #[test]
    fn virtual_disk_file_is_deleted_as_file() {
        let base = case_dir("virtual-disk");
        let root = base.join("Local");
        let virtual_disk = root
            .join("Packages")
            .join("CanonicalGroupLimited")
            .join("LocalState")
            .join("ext4.vhdx");
        fs::create_dir_all(virtual_disk.parent().expect("虚拟磁盘应有父目录"))
            .expect("创建虚拟磁盘目录失败");
        fs::write(&virtual_disk, vec![0u8; 4096]).expect("写入虚拟磁盘夹具失败");

        let result = delete_folders_in(
            vec![virtual_disk.to_string_lossy().to_string()],
            &[],
            std::slice::from_ref(&root),
        );

        assert_eq!(result.deleted_count, 1, "虚拟磁盘文件应被删除");
        assert_eq!(result.deleted_size, 4096, "大小按文件实际长度记");
        assert!(
            result.errors.is_empty(),
            "不应产生错误: {:?}",
            result.errors
        );
        assert!(!virtual_disk.exists(), "文件应已不存在");

        let _ = fs::remove_dir_all(&base);
    }

    /// `errors` and `failed_paths` must align entry by entry.
    ///
    /// The frontend uses `failed_paths.indexOf(path)` to fetch the error message from `errors`;
    /// the old implementation pushed paths skipped "because of executables" only into errors, not failed_paths,
    /// so from the first skipped entry on, every later entry's error message was attributed to the wrong one.
    #[test]
    fn skipped_executables_stay_index_aligned_with_errors() {
        let base = case_dir("aligned");
        let root = base.join("Local");
        let skipped = root.join("LegacyApp");
        let plain = root.join("PlainLeftover");
        let missing = root.join("GoneApp");
        fs::create_dir_all(&skipped).expect("创建跳过用例目录失败");
        fs::create_dir_all(&plain).expect("创建普通用例目录失败");
        fs::write(skipped.join("uninstall.exe"), vec![0u8; 2048]).expect("写入卸载程序失败");
        fs::write(plain.join("cache.bin"), vec![0u8; 1024]).expect("写入普通残留失败");

        let result = delete_folders_in(
            vec![
                skipped.to_string_lossy().to_string(),
                plain.to_string_lossy().to_string(),
                missing.to_string_lossy().to_string(),
            ],
            &[],
            std::slice::from_ref(&root),
        );

        assert_eq!(result.deleted_count, 1, "只有普通残留会被删掉");
        assert_eq!(result.skipped_executables.len(), 1);
        assert_eq!(result.failed_paths.len(), 2, "跳过项与不存在项都算没删掉");
        assert_eq!(result.errors.len(), result.failed_paths.len(), "逐条对齐");
        assert_eq!(
            result.failed_paths[0],
            skipped.to_string_lossy().to_string()
        );
        assert!(
            result.errors[0].starts_with(skipped.to_string_lossy().as_ref()),
            "第一条错误必须属于被跳过的目录，实际: {}",
            result.errors[0]
        );
        assert!(
            result.errors[1].contains("无法读取"),
            "第二条错误必须属于不存在的目录，实际: {}",
            result.errors[1]
        );
        assert!(skipped.exists(), "含可执行文件的目录不能被删");
        assert!(!plain.exists(), "普通残留应已删除");

        let _ = fs::remove_dir_all(&base);
    }

    /// Empty results must carry a "why", otherwise users cannot tell "really no leftovers" from "scan broken".
    ///
    /// This pins the three diagnostic fields: how many directories were checked, how many entries are below the threshold, and the top score.
    #[test]
    fn empty_result_reports_scan_diagnostics() {
        let base = case_dir("diagnostics");
        let root = base.join("Local");
        let candidate = root.join("RecentNoEvidenceApp");
        fs::create_dir_all(&candidate).expect("创建测试目录失败");
        fs::write(candidate.join("data.bin"), vec![0u8; 2 * 1024 * 1024])
            .expect("写入测试文件失败");

        let mut scanner = LeftoverScanner::new();
        scanner.deep_scan = false;
        let result =
            scanner.scan_with_paths(vec![(root.clone(), LeftoverSource::LocalAppData)], &[]);

        assert!(result.leftovers.is_empty(), "没有正向证据不该出结果");
        assert_eq!(result.scanned_directories, 1, "必须报告检查过几个目录");
        assert_eq!(result.below_threshold, 1, "必须报告有多少条卡在门槛下");
        assert!(
            (result.confidence_threshold - scanner.min_confidence_threshold).abs() < f32::EPSILON,
            "门槛要和扫描器同源，前端不另写死"
        );
        assert!(
            result.highest_confidence <= 0.05,
            "今天刚改过、没有任何残留证据，分数应接近 0，实际 {}",
            result.highest_confidence
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// Construct a scanner with fully controlled "what is installed, what was uninstalled".
    ///
    /// The real registry is unpredictable: this machine has Playwright installed, another machine may not.
    /// Scoring rules must be verifiable independently of the machine, otherwise you can only go in circles at the "it's like this on my machine" level.
    fn scanner_with_apps(installed: &[&str], uninstalled: &[&str]) -> LeftoverScanner {
        let apps: Vec<InstalledAppInfo> = installed
            .iter()
            .map(|name| InstalledAppInfo {
                display_name: (*name).to_string(),
                install_location: None,
                inferred_folder_names: Vec::new(),
            })
            .collect();

        let mut uninstalled_tokens: HashMap<String, String> = HashMap::new();
        for name in uninstalled {
            let lower = name.to_lowercase();
            for token in name_tokens(&lower) {
                uninstalled_tokens
                    .entry(token.to_string())
                    .or_insert_with(|| lower.clone());
            }
        }

        LeftoverScanner {
            app_map: InstalledAppMap {
                token_to_app: build_token_index(&apps),
                apps,
                folder_to_app: HashMap::new(),
                known_folders: HashSet::new(),
                display_names: HashSet::new(),
                leftover_candidates: HashSet::new(),
                uninstalled_tokens,
            },
            // The built-in whitelist would block real directory names like `code` and `nvidia`; tests only care about scoring, so passing empty is most controllable
            whitelist: Vec::new(),
            min_size_threshold: 1024 * 1024,
            min_days_old: 7,
            deep_scan: false,
            min_confidence_threshold: SUSPICIOUS_SCORE,
        }
    }

    /// The only purpose of token splitting: let "directory name" and "app name" match, without being led astray by generic words.
    #[test]
    fn name_tokens_keep_only_meaningful_words() {
        // Written as a function with an explicit lifetime rather than a closure: a closure cannot infer that the returned reference points to the input,
        // and the compiler reports "lifetime may not live long enough"
        fn tokens(name: &str) -> Vec<&str> {
            name_tokens(name).collect()
        }

        assert_eq!(tokens("ms-playwright"), vec!["playwright"]);
        assert_eq!(tokens("kimi-desktop-updater"), vec!["kimi"]);
        assert_eq!(tokens("ollama app"), vec!["ollama"]);
        assert_eq!(tokens("comfyui-desktop-2-updater"), vec!["comfyui"]);

        // Generic words / too short / purely numeric do not participate in matching - otherwise if any Electron app is installed,
        // every directory containing desktop would be treated as its data
        assert!(tokens("application").is_empty());
        assert!(tokens("desktop").is_empty());
        assert!(tokens("app").is_empty());
        assert!(tokens("2022").is_empty());
    }

    /// For the same directory name, an app still installed is negative evidence; only uninstalled is positive.
    #[test]
    fn token_match_tells_installed_apart_from_uninstalled() {
        let installed = scanner_with_apps(&["ms-playwright"], &[]);
        assert_eq!(
            installed.app_map.installed_app_for_name("ms-playwright"),
            Some("ms-playwright"),
            "装了 Playwright 时，ms-playwright 目录必须被认为是它在用"
        );
        assert_eq!(
            installed
                .app_map
                .installed_app_for_name("kimi-desktop-updater"),
            None
        );

        let uninstalled = scanner_with_apps(&[], &["Kimi"]);
        assert_eq!(
            uninstalled
                .app_map
                .uninstalled_app_for_name("kimi-desktop-updater"),
            Some("kimi"),
            "Kimi 卸载后，kimi-* 目录应能对上历史安装记录"
        );
        assert_eq!(
            uninstalled.app_map.uninstalled_app_for_name("desktop-app"),
            None,
            "全是停用词的名字不能被判成历史命中"
        );
    }

    /// Empty shell directories: directories left after being emptied are only worth output if they "appeared in history".
    ///
    /// This pins the main form of real uninstall leftovers (after Doubao uninstall, Local\Doubao has 0 bytes),
    /// which previously could not even enter scoring - the size threshold filtered it out before scoring.
    #[test]
    fn empty_shell_needs_history_evidence() {
        let base = case_dir("empty-shell");
        let root = base.join("Local");
        let ghost = root.join("GhostApp");
        let stranger = root.join("StrangerApp");
        fs::create_dir_all(&ghost).expect("创建空壳目录失败");
        fs::create_dir_all(&stranger).expect("创建空壳目录失败");

        let scanner = scanner_with_apps(&[], &["GhostApp"]);
        let result =
            scanner.scan_with_paths(vec![(root.clone(), LeftoverSource::LocalAppData)], &[]);

        let paths: Vec<&str> = result
            .leftovers
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        assert_eq!(
            paths.len(),
            1,
            "只有能对上历史的空目录才该输出，实际: {:?}",
            paths
        );
        assert!(paths[0].ends_with("GhostApp"));
        assert!(
            result.leftovers[0]
                .reasons
                .iter()
                .any(|r| r.contains("已空")),
            "理由里要说明它是空目录"
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// Orphaned root files: only executables are recognized, and too-small ones are not reported.
    ///
    /// `Roaming\ollama app.exe` is a typical leftover of this kind: the installer drops the launcher in the root,
    /// and it was not deleted on uninstall. The main flow only traverses directories and never sees it.
    #[test]
    fn loose_file_needs_executable_type_and_real_size() {
        let base = case_dir("loose");
        let root = base.join("Roaming");
        fs::create_dir_all(root.join("SomeDir")).expect("创建目录失败");
        fs::write(root.join("stray.exe"), vec![0u8; 128 * 1024]).expect("写入 exe 失败");
        fs::write(root.join("tiny.exe"), vec![0u8; 1024]).expect("写入小 exe 失败");
        fs::write(root.join("settings.json"), vec![0u8; 128 * 1024]).expect("写入配置文件失败");

        // Make stray.exe match a "uninstalled" history, otherwise "there's an exe in the root" alone is not enough to convict
        let scanner = scanner_with_apps(&[], &["stray"]);
        let entries =
            scanner.scan_loose_files(&[(root.clone(), LeftoverSource::RoamingAppData)], &[]);

        let names: Vec<&str> = entries
            .iter()
            .map(|entry| entry.app_name.as_str())
            .collect();
        assert_eq!(names, vec!["stray.exe"], "只有够大的 exe 该被报出来");
        assert_eq!(entries[0].leftover_type, LeftoverType::LooseFile);
        assert_eq!(entries[0].file_count, 1, "文件条目按 1 个文件计");
        assert_eq!(entries[0].size, 128 * 1024);

        let _ = fs::remove_dir_all(&base);
    }

    /// When the same-named app is still installed, a root exe must not be treated as a leftover.
    #[test]
    fn loose_file_ignored_while_its_app_is_still_installed() {
        let base = case_dir("loose-installed");
        let root = base.join("Roaming");
        fs::create_dir_all(&root).expect("创建目录失败");
        fs::write(root.join("ollama app.exe"), vec![0u8; 256 * 1024]).expect("写入 exe 失败");

        let scanner = scanner_with_apps(&["Ollama"], &[]);
        let entries =
            scanner.scan_loose_files(&[(root.clone(), LeftoverSource::RoamingAppData)], &[]);

        assert!(
            entries.is_empty(),
            "Ollama 还装着时，ollama app.exe 可能是它自己在用，不能报: {:?}",
            entries
                .iter()
                .map(|e| e.app_name.as_str())
                .collect::<Vec<_>>()
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// Pruning rules for virtual disk search.
    ///
    /// Getting this wrong produces no error, only "WSL users' ext4.vhdx is never found" -
    /// so both ends must be pinned: noisy directories must be pruned, and real WSL / Docker paths must be kept.
    #[test]
    fn disk_scan_prunes_noise_but_keeps_wsl_and_docker() {
        // The first two levels are app names; always enter
        assert!(should_descend_for_disks("packages", 1));
        assert!(should_descend_for_disks("docker", 1));
        assert!(should_descend_for_disks("ubuntu", 2));

        // From the third level, only known containers: WSL's LocalState, Docker's wsl/data
        assert!(
            should_descend_for_disks("localstate", 3),
            "WSL 的 ext4.vhdx 就在 LocalState 下，剪掉等于功能失效"
        );
        assert!(
            should_descend_for_disks("data", 3),
            "Docker 的磁盘在 wsl/data 下"
        );
        assert!(!should_descend_for_disks("ac", 3), "UWP 包里的噪音目录");
        assert!(!should_descend_for_disks("settings", 3));

        // Fragmented directories are never entered
        assert!(!should_descend_for_disks("node_modules", 1));
        assert!(!should_descend_for_disks("cache", 2));

        // This guards against later "casually" adding packages to the pruning list
        assert!(
            !DISK_SCAN_SKIP_DIRS.contains(&"packages"),
            "Local/Packages 正是 WSL 磁盘所在，任何情况下都不能整体跳过"
        );
    }
}
