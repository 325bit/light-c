// ============================================================================
// Dedicated social app scanner - smart path tracing and deep file-type classification
// ============================================================================
//
// Supported social apps:
// - WeChat: registry + documents directory + full-disk search to locate the data directory; also compatible with two generations of directory structures: legacy 3.x (WeChat Files)
//   and new 4.x (xwechat_files / db_storage + msg)
// - QQ/NTQQ: locate the nt_data directory and nt_qq\global, identify message databases;
//   install paths are inferred from common install directories in addition to the registry, covering custom install locations
// - DingTalk: locate the storage and cache directories
// - Lark/Feishu: scan LarkShell, locate sdk_storage and file_storage
// - WeCom (WXWork): identify cache and file directories
// - Telegram: identify cache directories
//
// ============================================================================
// Risk level description
// ============================================================================
//
// CRITICAL (danger) - chat record database, must not be deleted
//   Identification rules:
//   1. Path contains: Msg/Database, Msg/Multi, nt_msg, nt_db, db_storage, tdata
//   2. File extensions: .db, .db-wal, .db-shm, .sqlite, .sqlite-wal, .sqlite-shm
//   3. Effect: is_deletable = false, frontend disables the checkbox
//
// MEDIUM (warning) - received files, clean with caution
//   Identification rules:
//   1. Path contains: FileStorage/File, FileRecv, MsgAttach, file_storage
//   2. Effect: remind the user that it may contain important documents
//
// LOW (safe) - encrypted images/videos, recommended for cleanup
//   Identification rules:
//   1. .dat files under WeChat FileStorage/Image (encrypted images)
//   2. Media files in the Image, Video, Pic, Ptt directories
//   3. Effect: recommended for cleanup; can be re-downloaded after deletion
//
// NONE (no risk) - temporary cache and logs, safe to clean
//   Identification rules:
//   1. Path contains: Cache, Temp, Log, WebView, Thumb, sdk_storage
//   2. Effect: safe to clean, does not affect normal use
//
// ============================================================================

use log::info;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Maximum recursion depth for a single scan path.
///
/// Consistent with the original `walkdir` `max_depth(16)`: deep WeChat directories
/// (`msg/attach/<hash>/<date>/…`) easily exceed 10 levels; lowering it would miss files.
const SOCIAL_SCAN_MAX_DEPTH: usize = 16;

/// Fork subdirectories to rayon for the first few levels.
///
/// Measured (105k files / 15k directories on this machine): with only 1 level forked, if a single subdirectory dominates the
/// cost there is no benefit at all - QQ's `Image` has 4388 directories, of which `Group2` alone has 1618,
/// forking 1 level gives only 1.0x; forking 3 levels reaches 1.8x-3.7x.
const SOCIAL_SCAN_FORK_DEPTH: usize = 3;

/// Maximum number of slowest scan paths listed in the timing overlay.
///
/// Use 5: fits on one screen and is enough to determine "which app's directory is slowing things down".
const SOCIAL_SLOW_PATH_LIMIT: usize = 5;

// ============================================================================
// Enum definitions
// ============================================================================

/// File risk level
/// Used to mark file safety and guide the user on whether a file can be deleted
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    /// Danger - chat record database, must not be deleted
    /// Characteristics: .db, .db-wal, .db-shm files in the Msg/Database/nt_msg directory
    Critical,

    /// Warning - received files, clean with caution
    /// Characteristics: files in the File, FileRecv, MsgAttach directories
    Medium,

    /// Safe - encrypted images/videos, recommended for cleanup (local backup)
    /// Characteristics: .dat files in the Image directory, video cache
    Low,

    /// No risk - temporary cache and logs, safe to clean
    /// Characteristics: Cache, Temp, Log, WebView, Thumb directories
    None,
}

impl RiskLevel {
    /// Get the Chinese description of the risk level
    pub fn description(&self) -> &'static str {
        match self {
            RiskLevel::Critical => "危险（聊天记录）",
            RiskLevel::Medium => "谨慎清理",
            RiskLevel::Low => "建议清理",
            RiskLevel::None => "安全清理",
        }
    }

    /// Get the hint text for the risk level
    pub fn tooltip(&self) -> &'static str {
        match self {
            RiskLevel::Critical => "此文件为聊天记录数据库，删除后将永久丢失聊天记录，强烈建议保留",
            RiskLevel::Medium => "此文件可能包含重要文档或附件，请确认后再删除",
            RiskLevel::Low => "此文件为图片/视频缓存，删除后可通过重新下载恢复",
            RiskLevel::None => "此文件为临时缓存，可安全删除",
        }
    }

    /// Whether the user is allowed to check it for deletion
    pub fn is_deletable(&self) -> bool {
        !matches!(self, RiskLevel::Critical)
    }
}

/// File category
/// Classify by directory characteristics and file extensions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileCategory {
    /// Chat record database
    /// Contains .db, .db-wal, .db-shm files
    ChatDatabase,

    /// Image and video cache
    /// Contains encrypted .dat files and common image/video formats
    ImageVideo,

    /// Transferred files
    /// Various files received by the user
    FileTransfer,

    /// Temporary cache
    /// Includes Cache, Temp, Log, WebView, Thumb, and other directories
    TempCache,

    /// Moments/feed cache
    /// Includes Sns, Moments, and other directories
    MomentsCache,
}

impl FileCategory {
    /// Get the Chinese name of the category
    pub fn display_name(&self) -> &'static str {
        match self {
            FileCategory::ChatDatabase => "聊天记录",
            FileCategory::ImageVideo => "图片视频",
            FileCategory::FileTransfer => "传输文件",
            FileCategory::TempCache => "临时缓存",
            FileCategory::MomentsCache => "动态缓存",
        }
    }

    /// Get the description of the category
    pub fn description(&self) -> &'static str {
        match self {
            FileCategory::ChatDatabase => "聊天记录数据库文件，删除后无法恢复",
            FileCategory::ImageVideo => "聊天中收发的图片和视频文件",
            FileCategory::FileTransfer => "通过聊天传输的各类文件",
            FileCategory::TempCache => "应用运行产生的临时缓存",
            FileCategory::MomentsCache => "朋友圈、空间动态等缓存数据",
        }
    }

    /// Get the unique category identifier (for frontend/backend key matching)
    pub fn id(&self) -> &'static str {
        match self {
            FileCategory::ChatDatabase => "chatdatabase",
            FileCategory::ImageVideo => "imagevideo",
            FileCategory::FileTransfer => "filetransfer",
            FileCategory::TempCache => "tempcache",
            FileCategory::MomentsCache => "momentscache",
        }
    }

    /// Get the default risk level for this category
    pub fn default_risk_level(&self) -> RiskLevel {
        match self {
            FileCategory::ChatDatabase => RiskLevel::Critical,
            FileCategory::ImageVideo => RiskLevel::Low,
            FileCategory::FileTransfer => RiskLevel::Medium,
            FileCategory::TempCache => RiskLevel::None,
            FileCategory::MomentsCache => RiskLevel::None,
        }
    }
}

// ============================================================================
// Data structures
// ============================================================================

/// File last modified time (Unix seconds).
///
/// If unreadable (locked, permissions, clock before 1970), always return 0 - such entries are placed at the bottom in the UI when
/// sorting by "modified time", so they never mix with normal data.
fn modified_secs(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// Social app file entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialFileEntry {
    /// Full file path
    pub path: String,
    /// File size (bytes)
    pub size: u64,
    /// Name of the owning app
    pub app_name: String,
    /// File category
    pub category: FileCategory,
    /// Risk level
    pub risk_level: RiskLevel,
    /// Whether it can be deleted (forced to false for Critical level)
    pub deletable: bool,
    /// Last modified time (Unix seconds). Used by "sort by modified time" in the detail modal;
    /// older cached data lacks this field and is treated as 0 on deserialization (sorted last).
    #[serde(default)]
    pub modified_time: i64,
}

/// Social app category statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialCategoryStats {
    /// Category ID
    pub id: String,
    /// Category name
    pub name: String,
    /// Category description
    pub description: String,
    /// Number of files
    pub file_count: usize,
    /// Total size (bytes)
    pub total_size: u64,
    /// Number of deletable files
    pub deletable_count: usize,
    /// Size of deletable files
    pub deletable_size: u64,
    /// File list
    pub files: Vec<SocialFileEntry>,
}

/// Social app scan result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialScanResult {
    /// Statistics by category
    pub categories: Vec<SocialCategoryStats>,
    /// Total number of files
    pub total_files: usize,
    /// Total size
    pub total_size: u64,
    /// Number of deletable files
    pub deletable_files: usize,
    /// Size of deletable files
    pub deletable_size: u64,
    /// List of detected social apps
    pub detected_apps: Vec<String>,
    /// Phase timing details (for performance troubleshooting; collapsed by default in the frontend "stopwatch" overlay and hidden until clicked)
    pub phase_durations: Vec<SocialPhaseDuration>,
    /// The slowest scan paths (up to 5), used to answer "why this user's scan is slow"
    pub slow_paths: Vec<SocialSlowPath>,
}

/// Duration of a single scan phase
///
/// Take a timestamp once at each phase boundary, not inside the traversal loop - a dozen or so `Instant::now()` calls
/// are negligible compared with the whole scan (hundreds of milliseconds).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialPhaseDuration {
    /// Phase identifier: detect (path detection) / categorize (traverse and classify) / total
    pub stage: String,
    /// Phase duration in milliseconds
    pub duration_ms: u64,
}

/// Duration details for a single scan path
///
/// Also give the directory count: this module's cost mainly comes from `readdir` calls; when directories far outnumber files,
/// (e.g., QQ's `Image`: 4388 directories but only 1803 files), the reason for slowness is "listing directories"
/// rather than "too many files"; this line alone tells you, without asking the user to install tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialSlowPath {
    /// Scan path
    pub path: String,
    /// Owning app
    pub app_name: String,
    /// Duration of this path in milliseconds
    pub duration_ms: u64,
    /// Number of files under this path
    pub file_count: usize,
    /// Number of directories under this path (including itself)
    pub directory_count: usize,
}

/// Output of one subtree traversal: file entries + number of directories walked
///
/// The directory count is returned separately because almost all of this module's time is spent on `readdir`
/// (see `docs/dev/scan-performance-optimization.md` §6.1): troubleshooting "why the scan is slow" must distinguish
/// "too many files" from "too many directories" - their per-unit costs differ by two orders of magnitude.
struct DirectoryWalk {
    /// File entries collected under this subtree
    entries: Vec<SocialFileEntry>,
    /// Number of directories under this subtree (including the subtree root itself)
    directory_count: usize,
}

impl DirectoryWalk {
    /// Empty output: used when the depth limit is exceeded or a directory cannot be read; not counted in directory statistics
    fn empty() -> Self {
        Self {
            entries: Vec::new(),
            directory_count: 0,
        }
    }
}

/// Social app path information
#[derive(Debug, Clone)]
pub struct SocialAppPath {
    /// App name
    pub app_name: String,
    /// Path
    pub path: PathBuf,
    /// File category
    pub category: FileCategory,
    /// Whether it is a user-customized path (obtained from the registry)
    #[allow(dead_code)]
    pub is_custom_path: bool,
}

/// Registry path resolution result
/// Used to handle custom path configuration for apps such as WeChat
#[derive(Debug, Clone)]
pub(super) enum RegistryPathResult {
    /// Use the system documents directory (registry value is "MyDocument:")
    MyDocument,
    /// Absolute path (e.g., "E:\data\xwechat_files")
    AbsolutePath(String),
}

// ============================================================================
// Social app scanner
// ============================================================================

/// Social app scanner
/// Responsible for smart path tracing and deep file-type classification
pub struct SocialScanner {
    /// User profile directory
    pub(super) user_profile: String,
    /// LocalAppData directory
    pub(super) local_appdata: String,
    /// Roaming AppData directory
    pub(super) appdata: String,
    /// Documents directory (may be on a non-system drive)
    pub(super) documents_dir: String,
    /// Default documents directory
    pub(super) default_documents: String,
    /// All available drives (for full-disk search)
    pub(super) available_drives: Vec<String>,
    /// Per-drive directory cache: directory path -> list of its direct subdirectories.
    ///
    /// WeChat's and QQ's fallback searches each call read_dir on **the same set of directories**; the second pass is pure duplicate IO:
    /// Measured: most of the 2.3s detection phase is spent in these two traversals.
    /// The cache is valid only for the lifetime of the scanner instance; every rescan creates a new instance -
    /// this does not affect "realtime" behavior, it just avoids re-reading the disk within the same scan.
    pub(super) dir_cache:
        std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, Vec<std::path::PathBuf>>>,
}

impl SocialScanner {
    /// Create a new scanner instance
    pub fn new() -> Self {
        let user_profile =
            std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\Users\\Default".to_string());
        let local_appdata = std::env::var("LOCALAPPDATA")
            .unwrap_or_else(|_| format!("{}\\AppData\\Local", user_profile));
        let appdata = std::env::var("APPDATA")
            .unwrap_or_else(|_| format!("{}\\AppData\\Roaming", user_profile));

        // Get the real documents directory (may be on a non-system drive such as D:)
        let documents_dir = dirs::document_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| format!("{}\\Documents", user_profile));

        let default_documents = format!("{}\\Documents", user_profile);

        // Get all available drives (for full-disk search fallback)
        // M13 fix: single drive enumeration source (crate::sys_drive); non-Windows keeps its POSIX fallback
        #[cfg(target_os = "windows")]
        let available_drives = crate::sys_drive::existing_drive_roots();
        #[cfg(not(target_os = "windows"))]
        let available_drives = vec!["/".to_string()];

        info!("SocialScanner 初始化:");
        info!("  用户目录: {}", user_profile);
        info!("  文档目录: {}", documents_dir);
        info!("  LocalAppData: {}", local_appdata);
        info!("  可用盘符: {:?}", available_drives);

        Self {
            user_profile,
            local_appdata,
            appdata,
            documents_dir,
            default_documents,
            available_drives,
            dir_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Read the **direct subdirectories** of a directory, hitting the disk only once within the same scan.
    ///
    /// See the `dir_cache` notes: the fallback search ranges of WeChat and QQ overlap completely,
    /// the second search hits the cache and produces virtually no IO.
    pub(super) fn read_subdirectories_cached(
        &self,
        dir: &std::path::Path,
    ) -> Vec<std::path::PathBuf> {
        if let Ok(cache) = self.dir_cache.lock() {
            if let Some(hit) = cache.get(dir) {
                return hit.clone();
            }
        }

        let subdirectories: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.path())
                    .filter(|path| path.is_dir())
                    .collect()
            })
            .unwrap_or_default();

        if let Ok(mut cache) = self.dir_cache.lock() {
            cache.insert(dir.to_path_buf(), subdirectories.clone());
        }
        subdirectories
    }

    // ========================================================================
    // System utility methods
    // ========================================================================

    #[cfg(not(target_os = "windows"))]
    fn get_available_drives() -> Vec<String> {
        vec!["/".to_string()]
    }

    /// Perform the scan
    pub fn scan(&self) -> SocialScanResult {
        // Three time points are enough to locate issues: detection (independent of data volume), traversal (related to directory count), and total
        let scan_started = Instant::now();
        let mut all_paths = Vec::new();
        let mut detected_apps = Vec::new();

        // 1. WeChat path detection (including registry tracing)
        if let Some(paths) = self.detect_wechat_paths() {
            if !paths.is_empty() {
                detected_apps.push("微信".to_string());
                all_paths.extend(paths);
            }
        }

        // 2. QQ/NTQQ path detection
        if let Some(paths) = self.detect_qq_paths() {
            if !paths.is_empty() {
                detected_apps.push("QQ".to_string());
                all_paths.extend(paths);
            }
        }

        if let Some(paths) = self.detect_ntqq_paths() {
            if !paths.is_empty() {
                if !detected_apps.contains(&"NTQQ".to_string()) {
                    detected_apps.push("NTQQ".to_string());
                }
                all_paths.extend(paths);
            }
        }

        // 3. DingTalk path detection
        if let Some(paths) = self.detect_dingtalk_paths() {
            if !paths.is_empty() {
                detected_apps.push("钉钉".to_string());
                all_paths.extend(paths);
            }
        }

        // 4. Lark/Feishu path detection
        if let Some(paths) = self.detect_feishu_paths() {
            if !paths.is_empty() {
                detected_apps.push("飞书".to_string());
                all_paths.extend(paths);
            }
        }

        // 5. WeCom path detection
        if let Some(paths) = self.detect_wxwork_paths() {
            if !paths.is_empty() {
                detected_apps.push("企业微信".to_string());
                all_paths.extend(paths);
            }
        }

        // 6. Telegram path detection
        if let Some(paths) = self.detect_telegram_paths() {
            if !paths.is_empty() {
                detected_apps.push("Telegram".to_string());
                all_paths.extend(paths);
            }
        }

        // LINE and WhatsApp use separate adapters to avoid mixing overseas client path rules into domestic app logic.
        if let Some(paths) = self.detect_line_paths() {
            if !paths.is_empty() {
                detected_apps.push("LINE".to_string());
                all_paths.extend(paths);
            }
        }

        if let Some(paths) = self.detect_whatsapp_paths() {
            if !paths.is_empty() {
                detected_apps.push("WhatsApp".to_string());
                all_paths.extend(paths);
            }
        }

        info!(
            "共检测到 {} 个社交软件，{} 个扫描路径",
            detected_apps.len(),
            all_paths.len()
        );

        // Path detection ends here: this part is registry + shallow full-disk search, independent of chat data size
        let detect_duration = scan_started.elapsed();

        // Perform file scanning and classification
        let walk_started = Instant::now();
        let (categories, slow_paths) = self.scan_and_classify(&all_paths);
        let categorize_duration = walk_started.elapsed();

        // Aggregate statistics
        let total_files: usize = categories.iter().map(|c| c.file_count).sum();
        let total_size: u64 = categories.iter().map(|c| c.total_size).sum();
        let deletable_files: usize = categories.iter().map(|c| c.deletable_count).sum();
        let deletable_size: u64 = categories.iter().map(|c| c.deletable_size).sum();

        let phase_durations = vec![
            SocialPhaseDuration {
                stage: "detect".to_string(),
                duration_ms: detect_duration.as_millis() as u64,
            },
            SocialPhaseDuration {
                stage: "categorize".to_string(),
                duration_ms: categorize_duration.as_millis() as u64,
            },
            SocialPhaseDuration {
                stage: "total".to_string(),
                duration_ms: scan_started.elapsed().as_millis() as u64,
            },
        ];

        SocialScanResult {
            categories,
            total_files,
            total_size,
            deletable_files,
            deletable_size,
            detected_apps,
            phase_durations,
            slow_paths,
        }
    }

    // ========================================================================
    // File scanning and classification
    // ========================================================================

    /// Scan and classify files
    fn scan_and_classify(
        &self,
        app_paths: &[SocialAppPath],
    ) -> (Vec<SocialCategoryStats>, Vec<SocialSlowPath>) {
        // Initialize category statistics
        let mut category_map: HashMap<FileCategory, SocialCategoryStats> = HashMap::new();

        for cat in &[
            FileCategory::ChatDatabase,
            FileCategory::ImageVideo,
            FileCategory::FileTransfer,
            FileCategory::TempCache,
            FileCategory::MomentsCache,
        ] {
            category_map.insert(
                *cat,
                SocialCategoryStats {
                    id: cat.id().to_string(),
                    name: cat.display_name().to_string(),
                    description: cat.description().to_string(),
                    file_count: 0,
                    total_size: 0,
                    deletable_count: 0,
                    deletable_size: 0,
                    files: Vec::new(),
                },
            );
        }

        // Scan each path; adapters may point to the same file via multiple candidate entries, so deduplicate uniformly.
        //
        // ⚠️ Must **scan deeper paths first**: when the same file is covered by both a parent and a child directory, the one scanned first
        // determines the category. Telegram's tdata (chat records) contains user_data (cache),
        // originally tdata was scanned first by push order, so 1.1GB of cache was merged into chat records and no separate cache item appeared in the UI.
        // After sorting by depth from deep to shallow, child paths get their own category first and parent paths then skip duplicate files.
        let mut ordered_paths: Vec<_> = app_paths.iter().collect();
        ordered_paths.sort_by(|a, b| {
            b.path
                .components()
                .count()
                .cmp(&a.path.components().count())
        });

        let mut scanned_files = HashSet::new();
        // Subtrees already fully traversed. Deeper paths are scanned first; when a parent path is scanned later, skip these subdirectories directly,
        // so the same set of files does not walk the directory tree again only to be all discarded by deduplication.
        // Telegram's tdata contains user_data (cache); measured on this machine, this item wasted 33 ms.
        let mut scanned_roots: Vec<PathBuf> = Vec::new();
        // Record the duration of every path (over a hundred, negligible overhead), then keep only the slowest few
        let mut slow_paths: Vec<SocialSlowPath> = Vec::new();
        for app_path in ordered_paths {
            if !app_path.path.exists() {
                continue;
            }

            let path_started = Instant::now();
            let (file_count, directory_count) = self.scan_directory(
                &app_path.path,
                &app_path.app_name,
                app_path.category,
                &mut category_map,
                &mut scanned_files,
                &scanned_roots,
            );

            slow_paths.push(SocialSlowPath {
                path: app_path.path.to_string_lossy().to_string(),
                app_name: app_path.app_name.clone(),
                duration_ms: path_started.elapsed().as_millis() as u64,
                file_count,
                directory_count,
            });

            scanned_roots.push(app_path.path.clone());
        }

        // Keep only the slowest few: they fit in the overlay and are enough to answer "where the slowness is"
        slow_paths.sort_by(|left, right| right.duration_ms.cmp(&left.duration_ms));
        slow_paths.truncate(SOCIAL_SLOW_PATH_LIMIT);

        // Convert to Vec and sort
        let mut categories: Vec<SocialCategoryStats> = category_map.into_values().collect();

        // Sort by risk level: Critical > Medium > Low > None
        categories.sort_by(|a, b| {
            let risk_a = self.category_to_risk(&a.id);
            let risk_b = self.category_to_risk(&b.id);
            risk_b.cmp(&risk_a)
        });

        (categories, slow_paths)
    }

    /// Scan a directory and add it to categories
    ///
    /// Traversal forks by level internally (see `SOCIAL_SCAN_FORK_DEPTH`); after collection, do cross-path deduplication uniformly.
    /// Deferring deduplication to the end is safe: the same file cannot appear within one scan path, and duplicates across paths
    /// are still resolved by the original rule "the category scanned first wins".
    fn scan_directory(
        &self,
        path: &Path,
        app_name: &str,
        base_category: FileCategory,
        category_map: &mut HashMap<FileCategory, SocialCategoryStats>,
        scanned_files: &mut HashSet<String>,
        scanned_roots: &[PathBuf],
    ) -> (usize, usize) {
        let walked =
            self.collect_directory_entries(path, app_name, base_category, 0, scanned_roots);
        let mut counted_files = 0usize;

        for file_entry in walked.entries {
            let file_key = Self::normalize_path_key(Path::new(&file_entry.path));
            if !scanned_files.insert(file_key) {
                continue;
            }

            let size = file_entry.size;
            let deletable = file_entry.deletable;
            if let Some(stats) = category_map.get_mut(&file_entry.category) {
                stats.file_count += 1;
                stats.total_size += size;

                if deletable {
                    stats.deletable_count += 1;
                    stats.deletable_size += size;
                }

                stats.files.push(file_entry);
                counted_files += 1;
            }
        }

        (counted_files, walked.directory_count)
    }

    /// Collect all file entries under a directory (including subdirectories) and count the directories walked along the way.
    ///
    /// Equivalent to the original `walkdir` traversal: do not follow symlinks, only regular files, max depth 16 levels.
    /// The only difference is order (direct files first, then each subdirectory); the backend never guaranteed file order,
    /// and the frontend displays by category as a whole and filters in the modal itself, so it does not depend on that order.
    ///
    /// Parallelism only applies **within the same subtree**: subdirectories do not overlap, so assigning them to different threads does not change the result set.
    fn collect_directory_entries(
        &self,
        directory: &Path,
        app_name: &str,
        base_category: FileCategory,
        depth: usize,
        scanned_roots: &[PathBuf],
    ) -> DirectoryWalk {
        // The directory itself is already at the depth limit; going deeper would exceed the original max_depth(16)
        if depth >= SOCIAL_SCAN_MAX_DEPTH {
            return DirectoryWalk::empty();
        }

        // Insufficient permissions / directory just deleted: the original implementation also skips silently without aborting the whole scan
        let Ok(read_dir) = std::fs::read_dir(directory) else {
            return DirectoryWalk::empty();
        };

        let mut entries = Vec::new();
        let mut subdirectories = Vec::new();
        for dir_entry in read_dir.filter_map(|item| item.ok()) {
            let Ok(file_type) = dir_entry.file_type() else {
                continue;
            };

            if file_type.is_dir() {
                let child = dir_entry.path();
                // Skip subtrees already fully covered by a deeper scan path (results will be produced by that path)
                let covered = scanned_roots.iter().any(|root| child.starts_with(root));
                if !covered {
                    subdirectories.push(child);
                }
                continue;
            }

            // Only regular files: symlinks, devices, etc. are not counted (consistent with the original walkdir is_file filter)
            if !file_type.is_file() {
                continue;
            }

            // On Windows, read_dir already returns the file size, so metadata makes no extra system call
            if let Ok(metadata) = dir_entry.metadata() {
                entries.push(self.build_file_entry(
                    &dir_entry.path(),
                    metadata.len(),
                    modified_secs(&metadata),
                    app_name,
                    base_category,
                ));
            }
        }

        // Fork only the first few levels: deeper down, fall back to single-threaded to avoid spawning many empty tasks for small directories
        let child_walks: Vec<DirectoryWalk> =
            if depth < SOCIAL_SCAN_FORK_DEPTH && subdirectories.len() > 1 {
                subdirectories
                    .par_iter()
                    .map(|child| {
                        self.collect_directory_entries(
                            child,
                            app_name,
                            base_category,
                            depth + 1,
                            scanned_roots,
                        )
                    })
                    .collect()
            } else {
                subdirectories
                    .iter()
                    .map(|child| {
                        self.collect_directory_entries(
                            child,
                            app_name,
                            base_category,
                            depth + 1,
                            scanned_roots,
                        )
                    })
                    .collect()
            };

        // Count the current directory itself as one (subdirectory counts are added below)
        let mut directory_count = 1usize;
        for child_walk in child_walks {
            entries.extend(child_walk.entries);
            directory_count += child_walk.directory_count;
        }

        DirectoryWalk {
            entries,
            directory_count,
        }
    }

    /// Build a single file entry: classification and risk level still go through `classify_file`; no rule was changed
    fn build_file_entry(
        &self,
        path: &Path,
        size: u64,
        modified_time: i64,
        app_name: &str,
        base_category: FileCategory,
    ) -> SocialFileEntry {
        let (category, risk_level) = self.classify_file(path, base_category);
        SocialFileEntry {
            path: path.to_string_lossy().to_string(),
            size,
            app_name: app_name.to_string(),
            category,
            risk_level,
            deletable: risk_level.is_deletable(),
            modified_time,
        }
    }

    /// Classify by file characteristics and determine the risk level
    ///
    /// # Risk classification rules (in priority order)
    ///
    /// ## 1. CRITICAL (danger) - chat record database, must not be deleted
    ///
    /// **Path pattern matching (high priority):**
    /// - `Msg/Database` - WeChat message database directory
    /// - `Msg/Multi` - WeChat multi-instance message directory
    /// - `nt_msg` - NTQQ message database
    /// - `nt_db` - NTQQ user database
    /// - `tdata` - Telegram data directory
    /// - `Database` - DingTalk database directory
    ///
    /// **File extension matching:**
    /// - `.db`, `.db-wal`, `.db-shm` - SQLite database files
    /// - `.sqlite`, `.sqlite-wal`, `.sqlite-shm` - SQLite variants
    ///
    /// **Effect:** `is_deletable = false`; the frontend must disable the checkbox
    ///
    /// ## 2. MEDIUM (warning) - transferred files, clean with caution
    ///
    /// **Path pattern matching:**
    /// - `FileStorage/File` - WeChat transferred files
    /// - `FileRecv` - QQ received files
    /// - `MsgAttach` - WeChat message attachments
    /// - `file_storage` - Lark file storage
    ///
    /// **Effect:** remind the user that it may contain important documents
    ///
    /// ## 3. LOW (safe) - image/video cache, recommended for cleanup
    ///
    /// **Path pattern matching:**
    /// - `FileStorage/Image` - WeChat images (including encrypted .dat images)
    /// - `Image`, `Video`, `Pic`, `Ptt` - media directories of various apps
    ///
    /// **Effect:** recommended for cleanup; can be re-downloaded after deletion
    ///
    /// ## 4. NONE (no risk) - temporary cache, safe to clean
    ///
    /// **Path pattern matching:**
    /// - `Cache`, `Temp`, `Log`, `WebView`, `Thumb`
    /// - `sdk_storage`, `logs`
    ///
    /// **Effect:** safe to clean, does not affect normal use
    fn classify_file(&self, path: &Path, base_category: FileCategory) -> (FileCategory, RiskLevel) {
        let path_str = path.to_string_lossy().to_lowercase();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        // ================================================================
        // Rule 1: chat-record database (CRITICAL) - highest priority
        // ================================================================

        // Database file extensions
        let db_extensions = [
            "db",
            "db-wal",
            "db-shm",
            "sqlite",
            "sqlite-wal",
            "sqlite-shm",
        ];

        // Database directory patterns (exact path-segment matching)
        // Use more precise path matching to avoid false positives
        let critical_path_patterns = [
            "\\msg\\database", // WeChat: Msg/Database
            "\\msg\\multi",    // WeChat: Msg/Multi
            "/msg/database",   // compatible with forward slashes
            "/msg/multi",
            "\\nt_msg", // NTQQ: nt_msg
            "\\nt_db",  // NTQQ: nt_db
            "/nt_msg",
            "/nt_db",
            "\\tdata\\", // Telegram: tdata
            "/tdata/",
            "\\database\\", // DingTalk: Database
            "/database/",
        ];

        // Telegram's cache directory (user_data) is inside tdata, so its path also contains "\tdata\",
        // and would be misclassified as chat records by the pattern below - counting 1.1GB of cache as "non-deletable",
        // so a separate cache item would never appear in the UI. Exclude it first here.
        let is_telegram_cache =
            path_str.contains("\\tdata\\user_data") || path_str.contains("/tdata/user_data");

        // Check whether it is in a critical database directory
        let in_critical_dir = !is_telegram_cache
            && critical_path_patterns
                .iter()
                .any(|pattern| path_str.contains(pattern));

        // If it is in a critical directory or has a database file extension
        if in_critical_dir {
            return (FileCategory::ChatDatabase, RiskLevel::Critical);
        }

        // Database file extension + base category is a database
        if db_extensions.contains(&ext.as_str()) {
            if base_category == FileCategory::ChatDatabase {
                return (FileCategory::ChatDatabase, RiskLevel::Critical);
            }
            // Even outside a database directory, .db files still need a path check
            let db_related_dirs = ["msg", "database", "nt_msg", "nt_db", "tdata"];
            if db_related_dirs.iter().any(|d| path_str.contains(d)) {
                return (FileCategory::ChatDatabase, RiskLevel::Critical);
            }
        }

        // Base category is a database, so all files are marked Critical
        if base_category == FileCategory::ChatDatabase {
            return (FileCategory::ChatDatabase, RiskLevel::Critical);
        }

        // ================================================================
        // Rule 1.5: document extensions -> transferred files (MEDIUM)
        //
        // Placed before the cache rules because "document" is a stronger signal than "which directory it sits in":
        // Documents received by WeChat 4.x do not always land in msg\file; user-saved files, screenshot-tool exports,
        // and files temporarily written by the chat window can all appear in cache directories such as temp\RWTemp.
        // These are real files; cleaning them up as cache by rule 2 would lose user data.
        //
        // Deliberately exclude txt / log / json / xml: in cache directories they are mostly logs and config,
        // including them would lower the overall "safe to clean" amount, which is counterproductive.
        //
        // Also exclude bundled program resource packs (QQ emoji zips, skins, WebView cores...):
        // they share extensions with user documents, but deleting them only makes the client re-download resources,
        // measured on this machine, this rule alone would pull 130 MB of QQ emoji out of the "cleanable" amount.
        // ================================================================

        const DOCUMENT_EXTENSIONS: [&str; 16] = [
            "doc", "docx", "xls", "xlsx", "ppt", "pptx", "pdf", "csv", "zip", "rar", "7z", "psd",
            "ai", "apk", "exe", "msi",
        ];

        if DOCUMENT_EXTENSIONS.contains(&ext.as_str()) && !Self::is_program_resource_path(&path_str)
        {
            return (FileCategory::FileTransfer, RiskLevel::Medium);
        }

        // ================================================================
        // Rule 2: temporary cache (NONE)
        // Only when base_category itself is TempCache is cache path matching allowed to take effect;
        // if base_category is ImageVideo / FileTransfer / MomentsCache,
        // trust base_category and do not block the file just because the path contains "cache"
        // ================================================================

        let cache_path_patterns = [
            "\\cache\\",
            "/cache/",
            "\\temp\\",
            "/temp/",
            "\\log\\",
            "/log/",
            "\\logs\\",
            "/logs/",
            "\\webview\\",
            "/webview/",
            "\\thumb\\",
            "/thumb/",
            "\\sdk_storage\\",
            "/sdk_storage/",
            "\\customemotion\\",
            "/customemotion/",
        ];

        let is_cache_dir = cache_path_patterns
            .iter()
            .any(|pattern| path_str.contains(pattern));

        // Only classify by cache path when the caller explicitly gave the TempCache category
        if base_category == FileCategory::TempCache {
            return (FileCategory::TempCache, RiskLevel::None);
        }

        // ================================================================
        // Rule 3: Moments/feed cache (NONE)
        // Checked before the media rule because Sns directories often contain image/video thumbnail data that must not be claimed by the generic media rule.
        // ================================================================

        let moments_path_patterns = [
            "\\sns\\",
            "/sns/",
            "\\moments\\",
            "/moments/",
            "\\filestorage\\sns", // WeChat Moments full path
            "/filestorage/sns",
        ];

        let is_moments_dir = moments_path_patterns
            .iter()
            .any(|pattern| path_str.contains(pattern));

        if is_moments_dir || base_category == FileCategory::MomentsCache {
            return (FileCategory::MomentsCache, RiskLevel::None);
        }

        // ================================================================
        // Rule 4: images and videos (LOW)
        // ================================================================

        // Media file extensions
        let image_video_extensions = [
            // Images
            "jpg", "jpeg", "png", "gif", "bmp", "webp", "heic", "heif", "tiff", "ico",
            // Videos
            "mp4", "avi", "mov", "wmv", "flv", "mkv", "webm", "m4v", "3gp", "ts",
            // Audio
            "mp3", "wav", "aac", "flac", "ogg", "wma", "m4a",
            // WeChat-specific formats
            "dat",  // WeChat encrypted images
            "silk", // WeChat voice messages
            "amr",  // voice format
        ];

        let image_video_path_patterns = [
            "\\image\\",
            "/image/",
            "\\video\\",
            "/video/",
            "\\pic\\",
            "/pic/",
            "\\ptt\\",
            "/ptt/",
            "\\audio\\",
            "/audio/",
        ];

        let is_image_video_dir = image_video_path_patterns
            .iter()
            .any(|pattern| path_str.contains(pattern));

        // Special handling for WeChat encrypted images: .dat files under FileStorage/Image
        let is_wechat_encrypted_image = ext == "dat"
            && (path_str.contains("filestorage\\image") || path_str.contains("filestorage/image"));

        if is_wechat_encrypted_image {
            return (FileCategory::ImageVideo, RiskLevel::Low);
        }

        if image_video_extensions.contains(&ext.as_str()) || is_image_video_dir {
            return (FileCategory::ImageVideo, RiskLevel::Low);
        }

        if base_category == FileCategory::ImageVideo {
            return (FileCategory::ImageVideo, RiskLevel::Low);
        }

        // ================================================================
        // Rule 5: transferred files (MEDIUM)
        // ================================================================

        let file_transfer_path_patterns = [
            "\\filestorage\\file\\", // WeChat: FileStorage/File
            "/filestorage/file/",
            "\\filerecv\\", // QQ: FileRecv
            "/filerecv/",
            "\\msgattach\\", // WeChat: MsgAttach
            "/msgattach/",
            "\\file_storage\\", // Lark: file_storage
            "/file_storage/",
            "\\file\\", // generic File directory
            "/file/",
        ];

        let is_file_transfer_dir = file_transfer_path_patterns
            .iter()
            .any(|pattern| path_str.contains(pattern));

        if is_file_transfer_dir || base_category == FileCategory::FileTransfer {
            return (FileCategory::FileTransfer, RiskLevel::Medium);
        }

        // ================================================================
        // Rule 6: fallback cache matching (moved to the end)
        // Reaching here means base_category is not TempCache,
        // but the path does contain cache patterns (e.g., subdirectories under WeCom Cache\File)
        // ================================================================
        if is_cache_dir {
            return (FileCategory::TempCache, RiskLevel::None);
        }

        // ================================================================
        // Default: use the base category's default risk level
        // ================================================================
        (base_category, base_category.default_risk_level())
    }

    /// Whether a path belongs to "bundled program resources" rather than user files.
    ///
    /// Looking only at extensions would count QQ emoji packs, skins, and WebView cores (zip/apk) as "transferred files",
    /// misleading users (into thinking they received files) and moving them out of cleanable items. Use a keyword fallback here:
    /// The client puts such resources in fixed-named directories; extensions are not considered.
    fn is_program_resource_path(path_str: &str) -> bool {
        const RESOURCE_KEYWORDS: [&str; 12] = [
            "\\emoji",
            "\\face",
            "\\expression",
            "\\sticker",
            "\\skin",
            "\\theme",
            "\\lottie",
            "\\webview",
            "\\sdk_storage",
            "\\nt_temp",
            "\\avatar",
            "\\resource",
        ];
        RESOURCE_KEYWORDS
            .iter()
            .any(|keyword| path_str.contains(keyword))
    }

    /// Convert category ID to risk level (for sorting)
    fn category_to_risk(&self, id: &str) -> u8 {
        match id {
            "chatdatabase" => 4,
            "filetransfer" => 3,
            "imagevideo" => 2,
            "momentscache" => 1,
            "tempcache" => 0,
            _ => 0,
        }
    }
}

impl Default for SocialScanner {
    fn default() -> Self {
        Self::new()
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
            "lightc-social-{}-{}-{}",
            name,
            std::process::id(),
            sequence
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建测试目录失败");
        dir
    }

    #[test]
    fn test_risk_level_deletable() {
        assert!(!RiskLevel::Critical.is_deletable());
        assert!(RiskLevel::Medium.is_deletable());
        assert!(RiskLevel::Low.is_deletable());
        assert!(RiskLevel::None.is_deletable());
    }

    #[test]
    fn test_file_category_risk() {
        assert_eq!(
            FileCategory::ChatDatabase.default_risk_level(),
            RiskLevel::Critical
        );
        assert_eq!(
            FileCategory::FileTransfer.default_risk_level(),
            RiskLevel::Medium
        );
        assert_eq!(
            FileCategory::ImageVideo.default_risk_level(),
            RiskLevel::Low
        );
        assert_eq!(
            FileCategory::TempCache.default_risk_level(),
            RiskLevel::None
        );
    }

    /// Document files must be classified as "transferred files" no matter which directory they land in (including cache directories).
    ///
    /// WeChat 4.x puts received images in `msg/attach` and documents in `msg/file`, but user-saved,
    /// exported, and temporarily written files often appear in `temp/RWTemp`; cleaning them as cache would lose user files.
    /// The negative case must hold too: this is the boundary of "not harming the cleanable amount".
    #[test]
    fn classifies_documents_in_cache_dirs_as_file_transfer() {
        let root = fixture("doc-in-cache");
        let cache = root.join("temp/RWTemp/2026-11");
        std::fs::create_dir_all(&cache).expect("创建缓存目录失败");
        for name in ["report.docx", "photo.png", "debug.log"] {
            std::fs::write(cache.join(name), [0u8; 8]).expect("写入测试文件失败");
        }
        // Bundled program resources are also zips, but they are emoji-pack resources downloaded by the client and should not be "transferred files"
        let emoji = root.join("nt_qq/nt_data/Emoji/emoji-resource");
        std::fs::create_dir_all(&emoji).expect("创建表情资源目录失败");
        std::fs::write(emoji.join("emoji_res.zip"), [0u8; 8]).expect("写入测试文件失败");

        let scanner = SocialScanner::new();
        let walked =
            scanner.collect_directory_entries(&root, "微信", FileCategory::TempCache, 0, &[]);
        let find = |name: &str| {
            walked
                .entries
                .iter()
                .find(|entry| entry.path.ends_with(name))
                .unwrap_or_else(|| panic!("{} 未被收集", name))
        };

        assert_eq!(find("report.docx").category, FileCategory::FileTransfer);
        assert_eq!(find("report.docx").risk_level, RiskLevel::Medium);
        // Images in cache directories still count as cache (rule 2 takes priority); only document types are claimed
        assert_eq!(find("photo.png").category, FileCategory::TempCache);
        // Logs stay in cache, otherwise the "safe to clean" amount would be eaten by such files
        assert_eq!(find("debug.log").category, FileCategory::TempCache);
        assert_eq!(find("emoji_res.zip").category, FileCategory::TempCache);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_path_key_normalizes_windows_separators() {
        let first = SocialScanner::normalize_path_key(Path::new(r"C:/Users/Test/Cache/"));
        let second = SocialScanner::normalize_path_key(Path::new(r"c:\users\test\cache"));
        assert_eq!(first, second);
    }

    /// All files must still be collected after parallel forking: multiple levels and subdirectories (beyond the fork threshold), not a single file missed.
    #[test]
    fn collects_every_file_of_nested_directories() {
        let root = fixture("collect-nested");
        for group in 0..3 {
            for account in 0..4 {
                // 4 subdirectories per level to ensure the parallel branch is taken rather than single-threaded
                let dir = root.join(format!("group{}/account{}", group, account));
                std::fs::create_dir_all(&dir).expect("创建子目录失败");
                for index in 0..5 {
                    std::fs::write(dir.join(format!("photo{}.jpg", index)), [0u8; 16])
                        .expect("写入测试文件失败");
                }
            }
        }

        let scanner = SocialScanner::new();
        let walked =
            scanner.collect_directory_entries(&root, "微信", FileCategory::ImageVideo, 0, &[]);

        assert_eq!(walked.entries.len(), 60, "3×4×5 个文件必须全部收集到");
        assert_eq!(
            walked.entries.iter().map(|entry| entry.size).sum::<u64>(),
            60 * 16,
            "文件大小必须与原实现一致"
        );
        assert!(
            walked
                .entries
                .iter()
                .all(|entry| entry.category == FileCategory::ImageVideo
                    && entry.risk_level == RiskLevel::Low),
            "分类仍由 classify_file 决定，jpg 属于图片视频"
        );
        // Directory statistics must reflect the real traversal volume: root + 3 groups + 12 accounts
        assert_eq!(walked.directory_count, 16, "目录数 = 1 + 3 + 12");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Subtrees fully covered by a deeper scan path must be skipped, otherwise the same files are traversed twice.
    #[test]
    fn skips_subdirectory_covered_by_deeper_scan() {
        let root = fixture("skip-covered");
        let covered = root.join("msg/user_data");
        std::fs::create_dir_all(&covered).expect("创建子目录失败");
        std::fs::write(covered.join("cache.bin"), [0u8; 8]).expect("写入测试文件失败");

        let sibling = root.join("msg/other");
        std::fs::create_dir_all(&sibling).expect("创建子目录失败");
        std::fs::write(sibling.join("keep.bin"), [0u8; 4]).expect("写入测试文件失败");

        let scanner = SocialScanner::new();
        let walked = scanner.collect_directory_entries(
            &root,
            "Telegram",
            FileCategory::ChatDatabase,
            0,
            &[covered.clone()],
        );

        assert_eq!(walked.entries.len(), 1, "被覆盖的子目录不应再产出条目");
        assert!(walked.entries[0].path.ends_with("keep.bin"));
        // Three directories: root, msg, msg/other; the skipped user_data is not counted
        assert_eq!(walked.directory_count, 3, "被跳过的子树不应计入目录数");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Depth limit must match the original walkdir max_depth(16): files beyond level 16 are not collected.
    #[test]
    fn stops_collecting_beyond_sixteen_levels() {
        let root = fixture("depth-limit");
        let mut current = root.clone();
        for level in 0..18 {
            current = current.join(format!("level{}", level));
            std::fs::create_dir_all(&current).expect("创建子目录失败");
            std::fs::write(current.join(format!("file{}.bin", level)), [0u8; 1])
                .expect("写入测试文件失败");
        }

        let scanner = SocialScanner::new();
        let walked =
            scanner.collect_directory_entries(&root, "微信", FileCategory::TempCache, 0, &[]);

        // root is level 0, files in level0 are at level 2... level 16 corresponds to files in level14
        assert_eq!(walked.entries.len(), 15, "只收集深度不超过 16 层的文件");

        let _ = std::fs::remove_dir_all(&root);
    }
}
