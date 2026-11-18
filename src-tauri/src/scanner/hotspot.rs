// ============================================================================
// Large-directory analysis module (formerly C-drive hotspot scan)
// Supports two scan modes:
// 1. Default mode: scan only the AppData directory
// 2. Deep scan mode: scan a specified disk (sequential IO + depth limit + huge-directory skip)
// ============================================================================

use serde::{Deserialize, Serialize};
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Instant, SystemTime};
use tauri::Emitter;

// ============================================================================
// Data structure definitions
// ============================================================================

/// Large-directory entry information
/// Records the disk usage and last modified time of a single folder
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotspotEntry {
    /// Full path of the folder
    pub path: String,
    /// Folder name
    pub name: String,
    /// Total size (bytes) - includes all child files
    pub total_size: u64,
    /// File count
    pub file_count: usize,
    /// Last modified time (Unix timestamp, milliseconds)
    /// Takes the latest modification time among all files under the directory
    pub last_modified: i64,
    /// Parent directory type (Local/Roaming/LocalLow/System/Program, etc.)
    pub parent_type: String,
    /// Whether it is a cache directory (contains keywords like cache/tmp/temp/log/download/thumb)
    pub is_cache: bool,
    /// Whether it is a program directory (path contains Local\Programs)
    pub is_program: bool,
    /// Whether it is safe to clean (forced to false in deep scan mode)
    pub is_safe_to_clean: bool,
    /// Whether it is a system-protected directory (blacklisted directory)
    pub is_protected: bool,
    /// List of subdirectories (smart drill-down: when a directory is >5GB and has >1000 files, show the top 3 largest subdirectories)
    #[serde(default)]
    pub children: Vec<HotspotEntry>,
    /// Drill-down depth of the current directory (0 = top-level directory)
    #[serde(default)]
    pub depth: u8,
}

/// Large-directory scan result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotspotScanResult {
    /// List of large directories (sorted by size descending)
    pub entries: Vec<HotspotEntry>,
    /// Total number of folders scanned
    pub total_folders_scanned: usize,
    /// Scan duration (milliseconds)
    pub scan_duration_ms: u64,
    /// Total size of the scan scope (AppData or the selected disk total)
    pub scanned_total_size: u64,
    /// Whether it is deep scan mode
    pub is_full_scan: bool,
}

/// Scan progress information (for real-time display in the frontend)
#[derive(Debug, Clone, Serialize)]
pub struct HotspotScanProgress {
    /// Path of the directory currently being scanned
    pub current_dir: String,
    /// Total number of folders scanned
    pub scanned_dirs: usize,
    /// Number of large directories found (≥100MB)
    pub found_entries: usize,
    /// Total size of the scanned scope (bytes)
    pub total_size: u64,
    /// Total number of first-level directories (for progress percentage)
    pub total_first_level_dirs: usize,
    /// Number of completed first-level directories (progress percentage = completed_roots / total_first_level_dirs)
    /// Unlike scanned_dirs, this includes subdirectories and can exceed 100%
    pub completed_roots: usize,
    /// Current backend: mft / walkdir, used by the frontend to decide whether a fallback was taken
    #[serde(default)]
    pub backend: String,
    /// Current stage: mft / index / metadata / aggregate / result / walkdir
    #[serde(default)]
    pub stage: String,
    /// Text shown for the current stage, so the frontend does not have to guess the bottleneck from current_dir alone
    #[serde(default)]
    pub message: String,
    /// Total scan duration (milliseconds)
    #[serde(default)]
    pub elapsed_ms: u64,
    /// Duration of the current stage (milliseconds)
    #[serde(default)]
    pub stage_elapsed_ms: u64,
    /// Key numbers for the current stage (segment/read/parse, directory count, fallback stat count…), shown directly in the frontend
    #[serde(default)]
    pub detail: String,
}

fn build_hotspot_progress(
    backend: &str,
    stage: &str,
    message: &str,
    processed: usize,
    total_size: u64,
    total_first_level_dirs: usize,
    completed_roots: usize,
    scan_start: &Instant,
    stage_elapsed_ms: u64,
    detail: &str,
) -> HotspotScanProgress {
    HotspotScanProgress {
        current_dir: message.to_string(),
        scanned_dirs: processed,
        found_entries: 0,
        total_size,
        total_first_level_dirs,
        completed_roots,
        backend: backend.to_string(),
        stage: stage.to_string(),
        message: message.to_string(),
        elapsed_ms: scan_start.elapsed().as_millis() as u64,
        stage_elapsed_ms,
        detail: detail.to_string(),
    }
}

// M13 fix: the cancel flag used to be a module-local `static HOTSPOT_SCAN_CANCELLED`;
// it now lives in `crate::scan_cancel` alongside the other three scanners' flags.

// ============================================================================
// Dangerous-directory blacklist configuration
// During deep scans these directories are only counted for size; deletion operations are strictly forbidden
// ============================================================================

/// System-protected directory blacklist (cannot be cleaned during full-disk scans)
/// ProgramData has been removed — Docker/Scoop/VS Code Server under it are major space users that users care about
const PROTECTED_DIRECTORIES: &[&str] = &[
    // Windows core system directories
    "Windows",
    "Windows.old",
    "WinSxS",
    "System32",
    "SysWOW64",
    // System-protected directories
    "System Volume Information",
    "$Recycle.Bin",
    "$WINDOWS.~BT",
    "$WINDOWS.~WS",
    "Recovery",
    "PerfLogs",
    // Program installation directories
    "Program Files",
    "Program Files (x86)",
    // Hardware drivers (no cleaning value)
    "Intel",
    "AMD",
    "NVIDIA",
    // Boot-related
    "Boot",
    "EFI",
];

// ============================================================================
// Smart drill-down configuration
// ============================================================================

/// Minimum directory size to include as a hotspot entry (default 50MB, user-configurable)
const MIN_SIZE_THRESHOLD: u64 = 50 * 1024 * 1024;
/// Absolute lower bound (1MB) to avoid returning too many tiny entries
const SIZE_THRESHOLD_FLOOR: u64 = 1 * 1024 * 1024;
/// Maximum number of subdirectories returned per level
const DRILL_DOWN_TOP_CHILDREN: usize = 3;

/// Directories to skip during scanning (inaccessible or meaningless)
const SKIP_SCAN_DIRECTORIES: &[&str] = &[
    "System Volume Information",
    "$Recycle.Bin",
    "$WINDOWS.~BT",
    "$WINDOWS.~WS",
    "Config.Msi",
    "MSOCache",
    "Recovery",
];

/// System directories under AppData to skip
/// microsoft and packages have been removed — Teams/OneDrive/UWP app data are major space users that users care about
const APPDATA_SKIP_FOLDERS: &[&str] = &[
    "windows",
    "connecteddevicesplatform",
    "comms",
    "history",
    "inetcache",
    "inetcookies",
    "systemcertificates",
];

// ============================================================================
// Huge system directory skip mechanism (Win11 performance killer)
// ============================================================================

/// Huge system directories (file counts explode on Win11; WalkDir is forbidden from traversing deep)
/// These directories only count root-level size and do not recurse into subdirectories
/// SoftwareDistribution has been removed — Windows Update cache can reach several GB and users should see it
const HEAVY_SKIP_DIRS: &[&str] = &[
    "WinSxS",
    "WindowsApps",
    "DriverStore",
    "Installer",
    "Packages",
    "Catroot",
    "assembly",
];

/// Check whether a directory name is a huge system directory (case-insensitive, matches only the last path component)
pub(crate) fn is_heavy_system_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|name| HEAVY_SKIP_DIRS.iter().any(|d| name.eq_ignore_ascii_case(d)))
        .unwrap_or(false)
}

// ============================================================================
// Noise directory configuration (excluded from TopN ranking to avoid polluting results)
// ============================================================================

/// Noise system directories (allowed to be counted and displayed, but excluded from TopN ranking competition)
/// These directories are large but cannot be cleaned by users, e.g. WinSxS, System32
const NOISE_DIRECTORIES: &[&str] = &[
    "WinSxS",
    "System32",
    "SysWOW64",
    "assembly",
    "DriverStore",
    "WindowsApps",
];

/// Check whether a directory name is a noise directory (case-insensitive, matches only the last path component)
fn is_noise_directory(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|name| {
            NOISE_DIRECTORIES
                .iter()
                .any(|d| name.eq_ignore_ascii_case(d))
        })
        .unwrap_or(false)
}

/// Size threshold for triggering hotspot expansion (20GB)
/// Non-protected directories above this value are "expanded" — replaced by their subdirectories in the TopN ranking
const EXPAND_SIZE_THRESHOLD: u64 = 20 * 1024 * 1024 * 1024;

/// Core name check for hidden/system directories (does not call metadata(), avoiding Win11 Defender IO interception)
/// Based only on filename rules: hidden/system directories prefixed with . or $
fn is_hidden_name(name: &str) -> bool {
    if name.starts_with('.') && name != "." && name != ".." {
        return true;
    }
    if name.starts_with('$') {
        return true;
    }
    if name.eq_ignore_ascii_case("System Volume Information") {
        return true;
    }
    false
}

/// Path-based hidden check (used by jwalk's process_read_dir callback, does not rely on DirEntry)
pub(crate) fn is_hidden_by_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| is_hidden_name(n))
        .unwrap_or(false)
}

/// Calculate the hierarchical depth of path relative to root
/// For example, with root=C:\: C:\Users → 1, C:\Users\Evan → 2
fn calculate_relative_depth(root: &Path, path: &Path) -> usize {
    path.strip_prefix(root)
        .map(|p| p.components().count())
        .unwrap_or(0)
}

// ============================================================================
// Large-directory scan engine
// ============================================================================

/// Large-directory scan engine
/// Supports two scan modes: AppData scan and full-disk deep scan
pub struct HotspotScanner {
    /// Whether it is deep scan mode
    full_scan: bool,
    /// Maximum number of entries to return
    top_n: usize,
    /// Maximum display depth (scan root is Level 0, default 3, range 2-5)
    max_display_depth: usize,
    /// Scan depth (controls WalkDir/jwalk max_depth, independent of display depth)
    /// Fast mode is fixed at 6 levels, enough to cover AppData/Local/App/Cache depth
    scan_depth: u8,
    /// Minimum display size threshold (bytes, default 50MB)
    size_threshold: u64,
    /// Whether to ignore system-protected directories during deep scans (default true)
    /// When disabled, subdirectories of Windows/Program Files/ProgramData etc. are also included in results
    ignore_system_dirs: bool,
    /// Target drive letter for deep scans; AppData mode still scans the current user directory and is unaffected by this field.
    drive_letter: char,
}

impl HotspotScanner {
    /// Create a new scanner instance
    ///
    /// # Arguments
    /// - `full_scan`: whether to enable full-disk deep scanning
    /// - `top_n`: maximum number of entries to return
    pub fn new(full_scan: bool, top_n: usize) -> Self {
        Self {
            full_scan,
            top_n,
            max_display_depth: 3,
            scan_depth: 6, // Fast mode fixed at 6 levels, covering AppData/Local/App/Cache depth
            size_threshold: MIN_SIZE_THRESHOLD,
            ignore_system_dirs: true, // ignore system directories by default, preserving existing behavior
            drive_letter: 'C',
        }
    }

    /// Set the maximum display depth (can be linked with the settings panel)
    pub fn with_display_depth(mut self, depth: usize) -> Self {
        self.max_display_depth = depth.clamp(2, 4); // max 4 levels, beyond that there is no practical difference
        self
    }

    /// Set the size threshold
    pub fn with_size_threshold(mut self, threshold: u64) -> Self {
        self.size_threshold = threshold.max(SIZE_THRESHOLD_FLOOR);
        self
    }

    /// Set whether to ignore system-protected directories (only effective for deep scans)
    /// When disabled, can discover abnormally large files hidden under Windows/Program Files/ProgramData
    pub fn with_ignore_system_dirs(mut self, ignore: bool) -> Self {
        self.ignore_system_dirs = ignore;
        self
    }

    /// Set the target drive letter for deep scans, avoiding always scanning the system drive in multi-drive mode.
    pub fn with_drive_letter(mut self, drive_letter: char) -> Self {
        self.drive_letter = drive_letter.to_ascii_uppercase();
        self
    }

    /// Perform a scan (with real-time progress notifications)
    /// The frontend displays the progress bar by listening for the `hotspot-scan:progress` event
    pub fn scan_with_ui(&self, app_handle: &tauri::AppHandle) -> Result<HotspotScanResult, String> {
        if self.full_scan {
            self.scan_full_disk(Some(app_handle))
        } else {
            self.scan_appdata()
        }
    }

    // ========================================================================
    // AppData scan (default mode)
    // ========================================================================

    /// Scan the AppData directory (default mode)
    /// Uses a single jwalk traversal + ancestor aggregation, replacing the original per-subdirectory WalkDir loop
    fn scan_appdata(&self) -> Result<HotspotScanResult, String> {
        let start_time = std::time::Instant::now();

        let appdata_path = Self::get_appdata_path()?;

        let max_depth = self.scan_depth; // scan depth (fixed at 6), independent of display depth
                                         // Modified time is never collected: the only mode that wanted it (Accurate) had no
                                         // constructor left after the dead "accuracy mode" API was removed (M11).
        let track_modified = false;
        let cancel_flag = AtomicBool::new(false); // AppData scans do not support cancellation

        // Single jwalk traversal of AppData, aggregating ancestors at all levels
        let (root_stats, ancestor_map) = aggregate_ancestor_stats(
            &appdata_path,
            max_depth,
            track_modified,
            &cancel_flag,
            true, /* AppData scan with no system directories */
        );

        let scanned_total_size = root_stats.total_size;

        // Filter matching entries from ancestor_map
        let candidate_pairs: Vec<(PathBuf, FolderStats, usize)> = ancestor_map
            .into_iter()
            .filter(|(path, stats)| {
                if stats.total_size < self.size_threshold {
                    return false;
                }
                if Self::should_skip_appdata_folder(path) {
                    return false;
                }
                let depth = calculate_relative_depth(&appdata_path, path);
                // Include one extra level of child nodes so entries at the last display depth can still show subdirectories
                depth > 0 && depth <= self.max_display_depth + 1
            })
            .map(|(path, stats)| {
                let depth = calculate_relative_depth(&appdata_path, &path);
                (path, stats, depth)
            })
            .collect();

        let total_folders_scanned = candidate_pairs.len();

        // Keep only depth=1 entries as top-level results and recursively build subtrees
        let mut top_entries: Vec<HotspotEntry> = Vec::new();
        for (dir_path, dir_stats, depth) in candidate_pairs.iter().filter(|(_, _, d)| *d == 1) {
            let mut entry = Self::build_entry(dir_path, dir_stats, *depth as u8, false);
            // Give one extra level of depth so leaf entries can also build child nodes
            entry.children = Self::build_children_from_pairs(
                dir_path,
                2,
                self.max_display_depth + 1,
                &candidate_pairs,
            );
            top_entries.push(entry);
        }

        // Sort by size descending
        top_entries.sort_by(|a, b| b.total_size.cmp(&a.total_size));

        let entries: Vec<HotspotEntry> = top_entries.into_iter().take(self.top_n).collect();
        let scan_duration_ms = start_time.elapsed().as_millis() as u64;

        Ok(HotspotScanResult {
            entries,
            total_folders_scanned,
            scan_duration_ms,
            scanned_total_size,
            is_full_scan: false,
        })
    }

    // ========================================================================
    // Full-disk deep scan (single traversal + ancestor aggregation)
    // ========================================================================

    /// Scan a specified disk in full (sequential IO + depth limit + ancestor aggregation + huge-directory skip)
    ///
    /// v2.4.5 adds a direct MFT read engine: with admin + NTFS, full-disk scans complete in seconds; non-admin automatically falls back to jwalk
    fn scan_full_disk(
        &self,
        app_handle: Option<&tauri::AppHandle>,
    ) -> Result<HotspotScanResult, String> {
        use crate::scanner::hotspot_engine::engine_selector::{self, HotspotBackend};

        let start_time = std::time::Instant::now();

        let scan_root = PathBuf::from(format!("{}:\\", self.drive_letter));
        let first_level_dirs: Vec<PathBuf> = match std::fs::read_dir(&scan_root) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .filter(|p| !Self::should_skip_scan(p))
                .collect(),
            Err(e) => return Err(format!("无法读取 {} 盘根目录: {}", self.drive_letter, e)),
        };

        let total_first_level = first_level_dirs.len();
        let max_depth = self.scan_depth;
        // See scan_appdata: modified time is never collected after the dead
        // "accuracy mode" API was removed (M11).
        let track_modified = false;

        let total_scanned = AtomicUsize::new(0);
        let total_size = AtomicU64::new(0);
        let completed_roots = AtomicUsize::new(0);

        let mut all_entries: Vec<HotspotEntry> = Vec::new();
        let mut ancestor_cache: HashMap<PathBuf, FolderStats> = HashMap::new();
        let cancel_flag = crate::scan_cancel::flag(crate::scan_cancel::ScanKind::Hotspot);

        // ========================================================================
        // Try the direct MFT read engine (admin + NTFS)
        // ========================================================================
        let backend = engine_selector::detect_backend(self.drive_letter);

        match backend {
            HotspotBackend::Mft => {
                log::info!("[全盘扫描] 使用 MFT 直读引擎扫描 {} 盘", self.drive_letter);
                if let Some(app) = app_handle {
                    let _ = app.emit(
                        "hotspot-scan:progress",
                        build_hotspot_progress(
                            "mft",
                            "mft",
                            "MFT 直读引擎已就绪，正在读取主文件表...",
                            0,
                            0,
                            total_first_level,
                            0,
                            &start_time,
                            0,
                            "",
                        ),
                    );
                }

                let (mft_map, actual_backend) = engine_selector::scan_full_drive(
                    self.drive_letter,
                    &scan_root,
                    max_depth,
                    track_modified,
                    cancel_flag,
                    |progress| {
                        // MFT progress → frontend event
                        if let Some(app) = app_handle {
                            let _ = app.emit(
                                "hotspot-scan:progress",
                                build_hotspot_progress(
                                    "mft",
                                    progress.stage,
                                    &progress.message,
                                    progress.processed,
                                    0,
                                    total_first_level,
                                    (progress.processed / 50000)
                                        .min(total_first_level.saturating_sub(1)),
                                    &start_time,
                                    progress.stage_elapsed_ms,
                                    &progress.detail,
                                ),
                            );
                        }
                    },
                );

                ancestor_cache = mft_map;

                log::info!(
                    "[全盘扫描] {} 引擎完成，{} 个目录，开始构建结果...",
                    actual_backend.label(),
                    ancestor_cache.len()
                );

                if let Some(root_stats) = ancestor_cache.get(&scan_root) {
                    // Each directory after MFT aggregation already includes all descendants; do not sum list item sizes again, or ancestor directories would be double-counted.
                    total_size.store(root_stats.total_size, Ordering::Relaxed);
                }

                // Build all_entries from ancestor_cache (same logic as the Walkdir path)
                for (path, stats) in &ancestor_cache {
                    if stats.total_size >= self.size_threshold {
                        let depth = calculate_relative_depth(&scan_root, path);
                        if depth > 0 && depth <= self.max_display_depth {
                            total_scanned.fetch_add(1, Ordering::Relaxed);
                            all_entries.push(Self::build_entry(path, stats, depth as u8, true));
                        }
                    }
                }

                // Add root entries for first-level directories
                for dir in &first_level_dirs {
                    if let Some(root_stats) = ancestor_cache.get(dir) {
                        if root_stats.total_size >= self.size_threshold {
                            let dir_depth = calculate_relative_depth(&scan_root, dir);
                            all_entries.push(Self::build_entry(
                                dir,
                                root_stats,
                                dir_depth as u8,
                                true,
                            ));
                        }
                    }
                }
            }

            // ========================================================================
            // Fallback: jwalk traversal directory by directory
            // ========================================================================
            HotspotBackend::Walkdir => {
                log::info!(
                    "[全盘扫描] 使用 jwalk 常规遍历扫描 {} 盘",
                    self.drive_letter
                );

                // Scan each first-level directory sequentially (IO-intensive task, avoid SSD random IO storms)
                for dir in &first_level_dirs {
                    if cancel_flag.load(Ordering::SeqCst) {
                        break;
                    }

                    let is_protected_root = Self::is_protected_directory(dir);
                    let dir_depth = calculate_relative_depth(&scan_root, dir);

                    let (root_stats, ancestor_map) = aggregate_ancestor_stats(
                        dir,
                        max_depth,
                        track_modified,
                        cancel_flag,
                        self.ignore_system_dirs,
                    );

                    // Merge into the global ancestor cache
                    ancestor_cache.insert(dir.clone(), root_stats);
                    for (path, stats) in &ancestor_map {
                        ancestor_cache
                            .entry(path.clone())
                            .and_modify(|existing| {
                                if stats.total_size > existing.total_size {
                                    *existing = *stats;
                                }
                            })
                            .or_insert(*stats);
                    }

                    // 1. Add root directory entries
                    if root_stats.file_count > 0 {
                        total_scanned.fetch_add(1, Ordering::Relaxed);
                        total_size.fetch_add(root_stats.total_size, Ordering::Relaxed);
                        if root_stats.total_size >= self.size_threshold {
                            all_entries.push(Self::build_entry(
                                dir,
                                &root_stats,
                                dir_depth as u8,
                                true,
                            ));
                        }
                    }

                    // 2. Extract matching ancestor entries from ancestor_map
                    let hide_ancestors = is_protected_root && self.ignore_system_dirs;
                    if !hide_ancestors {
                        for (path, stats) in &ancestor_map {
                            if stats.total_size >= self.size_threshold {
                                let depth = calculate_relative_depth(&scan_root, path);
                                if depth <= dir_depth + self.max_display_depth {
                                    total_scanned.fetch_add(1, Ordering::Relaxed);
                                    all_entries.push(Self::build_entry(
                                        path,
                                        stats,
                                        depth as u8,
                                        true,
                                    ));
                                }
                            }
                        }
                    }

                    completed_roots.fetch_add(1, Ordering::Relaxed);

                    if let Some(app) = app_handle {
                        let completed = completed_roots.load(Ordering::Relaxed);
                        let message = format!(
                            "正在常规遍历 {} ({}/{})",
                            dir.to_string_lossy(),
                            completed,
                            total_first_level
                        );
                        let _ = app.emit(
                            "hotspot-scan:progress",
                            build_hotspot_progress(
                                "walkdir",
                                "walkdir",
                                &message,
                                total_scanned.load(Ordering::Relaxed),
                                total_size.load(Ordering::Relaxed),
                                total_first_level,
                                completed,
                                &start_time,
                                0,
                                "",
                            ),
                        );
                    }
                }
            }
        }

        // Hotspot expansion + tree construction (shared by both engines, logic unchanged)

        // Check whether the scan was cancelled
        if cancel_flag.load(Ordering::SeqCst) {
            log::info!("大目录扫描被用户取消");
            if let Some(app) = app_handle {
                let _ = app.emit("hotspot-scan:cancelled", ());
            }
            all_entries.sort_by(|a, b| b.total_size.cmp(&a.total_size));
            let partial: Vec<HotspotEntry> = all_entries.into_iter().take(self.top_n).collect();
            return Ok(HotspotScanResult {
                entries: partial,
                total_folders_scanned: total_scanned.load(Ordering::Relaxed),
                scan_duration_ms: start_time.elapsed().as_millis() as u64,
                scanned_total_size: total_size.load(Ordering::Relaxed),
                is_full_scan: true,
            });
        }

        let result_stage_start = Instant::now();
        if let Some(app) = app_handle {
            let _ = app.emit(
                "hotspot-scan:progress",
                build_hotspot_progress(
                    match backend {
                        HotspotBackend::Mft => "mft",
                        HotspotBackend::Walkdir => "walkdir",
                    },
                    "result",
                    "正在生成热点目录列表",
                    total_scanned.load(Ordering::Relaxed),
                    total_size.load(Ordering::Relaxed),
                    total_first_level,
                    completed_roots.load(Ordering::Relaxed),
                    &start_time,
                    0,
                    "",
                ),
            );
        }

        // First build a parent-child index from the aggregation cache; subsequent hotspot expansion and tree results only query memory.
        // This keeps the "display depth" setting effective and avoids repeated read_dir calls in the result stage that trigger disk IO.
        let child_index = build_child_index(
            &ancestor_cache,
            &scan_root,
            self.size_threshold,
            self.ignore_system_dirs,
        );

        // Hotspot expansion: recursively expand container directories (Windows/Program Files, etc.),
        // giving the ranking over to cleanable hotspots users actually care about (WeChat/npm/Docker, etc.)
        // Whether noise directories (WinSxS/System32, etc.) are filtered is determined by the system-directory setting
        let mut entries = flatten_hotspots(
            all_entries,
            &child_index,
            self.top_n,
            true,
            self.ignore_system_dirs,
        );

        // Fill in the result tree according to the user's display depth setting. The depth here is the visible level count "from the current hotspot entry",
        // consistent with the frontend treeDepth logic: when set to 4, the top hotspot + 3 levels of subdirectories are all returned.
        for entry in &mut entries {
            let entry_path = PathBuf::from(&entry.path);
            let child_max_depth = entry
                .depth
                .saturating_add(self.max_display_depth.saturating_sub(1) as u8);
            entry.children =
                build_tree_children_from_index(&entry_path, child_max_depth, &child_index, true);
        }

        let scan_duration_ms = start_time.elapsed().as_millis() as u64;

        if let Some(app) = app_handle {
            let mut final_progress = build_hotspot_progress(
                match backend {
                    HotspotBackend::Mft => "mft",
                    HotspotBackend::Walkdir => "walkdir",
                },
                "result",
                "扫描完成",
                total_scanned.load(Ordering::Relaxed),
                total_size.load(Ordering::Relaxed),
                total_first_level,
                completed_roots.load(Ordering::Relaxed),
                &start_time,
                result_stage_start.elapsed().as_millis() as u64,
                "",
            );
            final_progress.found_entries = entries.len();
            let _ = app.emit("hotspot-scan:progress", &final_progress);
        }

        Ok(HotspotScanResult {
            entries,
            total_folders_scanned: total_scanned.load(Ordering::Relaxed),
            scan_duration_ms,
            scanned_total_size: total_size.load(Ordering::Relaxed),
            is_full_scan: true,
        })
    }

    // ========================================================================
    // Helper methods
    // ========================================================================

    /// Get the AppData path
    fn get_appdata_path() -> Result<PathBuf, String> {
        if let Ok(roaming) = std::env::var("APPDATA") {
            let roaming_path = PathBuf::from(&roaming);
            if let Some(parent) = roaming_path.parent() {
                return Ok(parent.to_path_buf());
            }
        }

        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let local_path = PathBuf::from(&local);
            if let Some(parent) = local_path.parent() {
                return Ok(parent.to_path_buf());
            }
        }

        Err("无法获取 AppData 路径".to_string())
    }

    /// Determine whether a folder under AppData should be skipped
    fn should_skip_appdata_folder(path: &Path) -> bool {
        let folder_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        APPDATA_SKIP_FOLDERS.contains(&folder_name.as_str())
    }

    /// Determine whether a directory should be skipped during scanning (inaccessible or meaningless)
    fn should_skip_scan(path: &Path) -> bool {
        let folder_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        // Check whether it is in the skip list
        for skip_dir in SKIP_SCAN_DIRECTORIES {
            if folder_name.eq_ignore_ascii_case(skip_dir) {
                return true;
            }
        }

        // Skip system directories starting with $
        if folder_name.starts_with('$') {
            return true;
        }

        false
    }

    /// Determine whether it is a system-protected directory (blacklist)
    fn is_protected_directory(path: &Path) -> bool {
        let path_str = path.to_string_lossy().to_lowercase();

        // Extract the folder name from the end of the path (avoid an extra String allocation)
        let folder_name = path_str.rsplit('\\').next().unwrap_or("");

        // Check whether it is in the protection list
        for protected in PROTECTED_DIRECTORIES {
            if folder_name.eq_ignore_ascii_case(protected) {
                return true;
            }
        }

        // Windows Update cache: visible but not allowed to be cleaned automatically
        if folder_name.eq_ignore_ascii_case("softwaredistribution") {
            return true;
        }

        // Check whether the path contains a Windows system directory
        if path_str.contains("\\windows\\") || path_str.ends_with("\\windows") {
            return true;
        }

        // Check whether it is a Program Files subdirectory
        if path_str.contains("\\program files\\") || path_str.contains("\\program files (x86)\\") {
            return true;
        }

        false
    }

    /// Determine whether it is a cache directory
    fn is_cache_directory(path: &str, folder_name: &str) -> bool {
        let path_lower = path.to_lowercase();
        let name_lower = folder_name.to_lowercase();

        let cache_keywords = [
            "cache",
            "caches",
            "tmp",
            "temp",
            "log",
            "logs",
            "download",
            "downloads",
            "thumb",
            "thumbnails",
            "crashdump",
            "crashreport",
            "backup",
        ];

        for keyword in &cache_keywords {
            if name_lower.contains(keyword) {
                return true;
            }
        }

        if path_lower.contains("\\temp\\") || path_lower.ends_with("\\temp") {
            return true;
        }

        false
    }

    /// Determine whether it is a program directory
    fn is_program_directory(path: &str) -> bool {
        let path_lower = path.to_lowercase();
        path_lower.contains("\\programs\\")
            || path_lower.contains("\\program files\\")
            || path_lower.contains("\\program files (x86)\\")
    }

    /// Get the parent directory type
    fn get_parent_type(path: &str) -> String {
        let path_lower = path.to_lowercase();

        if path_lower.contains("\\appdata\\local\\") {
            "Local".to_string()
        } else if path_lower.contains("\\appdata\\roaming\\") {
            "Roaming".to_string()
        } else if path_lower.contains("\\appdata\\locallow\\") {
            "LocalLow".to_string()
        } else if path_lower.contains("\\program files (x86)\\") {
            "Program Files (x86)".to_string()
        } else if path_lower.contains("\\program files\\") {
            "Program Files".to_string()
        } else if path_lower.contains("\\windows\\") {
            "Windows".to_string()
        } else if path_lower.contains("\\users\\") {
            "Users".to_string()
        } else {
            "System".to_string()
        }
    }

    /// Convert SystemTime to a Unix timestamp (milliseconds)
    pub(crate) fn system_time_to_millis(time: SystemTime) -> i64 {
        match time.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(duration) => duration.as_millis() as i64,
            Err(_) => 0,
        }
    }

    /// Core builder: uniformly construct directory paths and statistics into a HotspotEntry
    /// Eliminates duplicated code in scan_appdata / scan_full_disk / drill_down / scan_path_direct
    fn build_entry(
        path: &Path,
        stats: &FolderStats,
        depth: u8,
        is_full_scan: bool,
    ) -> HotspotEntry {
        let path_str = path.to_string_lossy().to_string();
        let folder_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let is_cache = Self::is_cache_directory(&path_str, &folder_name);
        let is_program = Self::is_program_directory(&path_str);
        let parent_type = Self::get_parent_type(&path_str);
        let is_protected = Self::is_protected_directory(path);

        HotspotEntry {
            path: path_str,
            name: folder_name,
            total_size: stats.total_size,
            file_count: stats.file_count,
            last_modified: stats.last_modified,
            parent_type,
            is_cache,
            is_program,
            is_safe_to_clean: !is_full_scan && is_cache && !is_program && !is_protected,
            is_protected,
            children: Vec::new(),
            depth,
        }
    }

    /// Recursively build a subdirectory tree from the candidate list
    ///
    /// Builds parent-child relationships based on path prefix matching in candidate_pairs, with no extra IO.
    /// A child entry whose path's parent() equals parent_path is considered a parent-child relationship.
    fn build_children_from_pairs(
        parent_path: &Path,
        current_depth: usize,
        max_depth: usize,
        candidate_pairs: &[(PathBuf, FolderStats, usize)],
    ) -> Vec<HotspotEntry> {
        if current_depth > max_depth {
            return Vec::new();
        }

        // Filter direct subdirectories: path.parent() == parent_path and depth == current_depth
        let mut children: Vec<HotspotEntry> = candidate_pairs
            .iter()
            .filter(|(p, _, d)| *d == current_depth && p.parent() == Some(parent_path))
            .map(|(path, stats, depth)| {
                let mut child = Self::build_entry(path, stats, *depth as u8, false);
                child.children = Self::build_children_from_pairs(
                    path,
                    current_depth + 1,
                    max_depth,
                    candidate_pairs,
                );
                child
            })
            .collect();

        children.sort_by(|a, b| b.total_size.cmp(&a.total_size));
        children.into_iter().take(DRILL_DOWN_TOP_CHILDREN).collect()
    }
}

// ============================================================================
// Ancestor aggregation → moved to hotspot_engine/fallback_scanner.rs
// ============================================================================
use crate::scanner::hotspot_engine::fallback_scanner::aggregate_ancestor_stats;

#[derive(Debug, Clone)]
struct IndexedChild {
    path: PathBuf,
    stats: FolderStats,
    depth: u8,
}

type ChildIndex = HashMap<PathBuf, Vec<IndexedChild>>;

/// Convert the aggregated directory cache into a parent-child index; subsequent result generation only queries memory and no longer recursively reads disk directories.
fn build_child_index(
    cache: &HashMap<PathBuf, FolderStats>,
    root: &Path,
    min_size: u64,
    ignore_system_dirs: bool,
) -> ChildIndex {
    let mut child_index: ChildIndex = HashMap::with_capacity(cache.len());

    for (path, stats) in cache {
        let Some(parent) = path.parent() else {
            continue;
        };
        if parent == path || stats.total_size < min_size {
            continue;
        }

        let skip_heavy = ignore_system_dirs && is_heavy_system_dir(path);
        // When the user disables system-directory filtering, directories like WinSxS/System32 must genuinely participate in the ranking,
        // otherwise results will clearly disagree with the setting semantics and full-disk views like WizTree.
        let skip_noise = ignore_system_dirs && is_noise_directory(path);
        if skip_noise || skip_heavy || HotspotScanner::should_skip_scan(path) {
            continue;
        }

        let depth = calculate_relative_depth(root, path).min(u8::MAX as usize) as u8;
        child_index
            .entry(parent.to_path_buf())
            .or_default()
            .push(IndexedChild {
                path: path.clone(),
                stats: *stats,
                depth,
            });
    }

    for children in child_index.values_mut() {
        children.sort_by(|a, b| b.stats.total_size.cmp(&a.stats.total_size));
    }

    child_index
}

/// Build tree child nodes for the results page from the parent-child index; depth strictly follows the frontend "display depth" setting.
fn build_tree_children_from_index(
    parent: &Path,
    max_depth: u8,
    child_index: &ChildIndex,
    is_full_scan: bool,
) -> Vec<HotspotEntry> {
    let Some(children) = child_index.get(parent) else {
        return Vec::new();
    };

    children
        .iter()
        .filter(|child| child.depth <= max_depth)
        .take(DRILL_DOWN_TOP_CHILDREN)
        .map(|child| {
            let mut entry =
                HotspotScanner::build_entry(&child.path, &child.stats, child.depth, is_full_scan);
            if child.depth < max_depth {
                entry.children = build_tree_children_from_index(
                    &child.path,
                    max_depth,
                    child_index,
                    is_full_scan,
                );
            }
            entry
        })
        .collect()
}

// ============================================================================
// Hotspot expansion mechanism (Hotspot Expansion)
// Core idea: prevent container directories such as Windows / Program Files from dominating the TopN,
// and give the ranking over to cleanable hotspot directories users actually care about through recursive expansion.
// ============================================================================

/// Determine whether a directory should be expanded (replaced by its subdirectories in the TopN ranking)
///
/// Trigger conditions (any one is enough to expand):
/// - System-protected directory (Windows, etc.) — the user cannot operate on it
/// - Total size > 20GB — too large, need to look inside
/// - depth <= 1 — a direct child of C:\, too general
///
/// Conditions for not expanding (treated as a terminal hotspot):
/// - Non-protected + below the threshold + sufficient depth → this is the directory the user is looking for
fn should_expand_directory(entry: &HotspotEntry, ignore_system_dirs: bool) -> bool {
    if entry.is_protected {
        // When system-directory filtering is disabled, protected directories themselves are the disk-usage results the user wants to see;
        // must return false first, to avoid being forcibly expanded again by the 20GB threshold or first-level directory rule.
        return ignore_system_dirs;
    }
    if entry.total_size > EXPAND_SIZE_THRESHOLD {
        return true;
    }
    if entry.depth <= 1 {
        return true;
    }
    false
}

/// Find the direct subdirectories of a directory from the parent-child index.
/// The index has already applied size, system-directory and noise-directory filtering, avoiding another read_dir during hotspot expansion.
fn find_meaningful_children(
    dir: &Path,
    child_index: &ChildIndex,
    is_full_scan: bool,
) -> Vec<HotspotEntry> {
    child_index
        .get(dir)
        .map(|children| {
            children
                .iter()
                .map(|child| {
                    HotspotScanner::build_entry(
                        &child.path,
                        &child.stats,
                        child.depth,
                        is_full_scan,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// BinaryHeap wrapper: orders HotspotEntry by total_size descending
/// Max-heap rule: larger total_size comes first
struct SizeOrd(HotspotEntry);

impl PartialEq for SizeOrd {
    fn eq(&self, other: &Self) -> bool {
        self.0.total_size == other.0.total_size
    }
}
impl Eq for SizeOrd {}

impl Ord for SizeOrd {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_size.cmp(&other.0.total_size)
    }
}
impl PartialOrd for SizeOrd {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Hotspot flattening: recursively expand container directories and extract the real user hotspots
///
/// Algorithm:
/// 1. Put all_entries into a BinaryHeap candidate queue in descending size order (O(log n) insert/pop)
/// 2. Pop the largest candidate each time
/// 3. If it should be expanded: find subdirectories from the cache and add them to the candidate queue to keep competing;
///    if not: add it to the results as a terminal hotspot
/// 4. Until results reach top_n or the candidate queue is exhausted
///
/// Noise directories (WinSxS/System32, etc.) are only filtered when system-directory filtering is enabled
fn flatten_hotspots(
    all_entries: Vec<HotspotEntry>,
    child_index: &ChildIndex,
    top_n: usize,
    is_full_scan: bool,
    ignore_system_dirs: bool,
) -> Vec<HotspotEntry> {
    // Keep noise directories when system-directory filtering is off, to align semantics with full-disk rankings like WizTree.
    let mut candidates: BinaryHeap<SizeOrd> = all_entries
        .into_iter()
        .filter(|e| !ignore_system_dirs || !is_noise_directory(&PathBuf::from(&e.path)))
        .map(SizeOrd)
        .collect();

    let mut result: Vec<HotspotEntry> = Vec::new();
    // Prevent the same directory from being added to the results more than once
    let mut seen: HashSet<PathBuf> = HashSet::new();

    while !candidates.is_empty() && result.len() < top_n {
        let entry = candidates.pop().unwrap().0;

        // When system-directory filtering is off, results should be closer to WizTree's full-disk usage view,
        // so protected directories themselves are allowed onto the ranking and are no longer unconditionally expanded into deeper subdirectories.
        if should_expand_directory(&entry, ignore_system_dirs) {
            let dir_path = PathBuf::from(&entry.path);
            let children = find_meaningful_children(&dir_path, child_index, is_full_scan);

            if children.is_empty() {
                if seen.insert(dir_path) {
                    result.push(entry);
                }
            } else {
                for child in children {
                    let child_path = PathBuf::from(&child.path);
                    if !seen.contains(&child_path) {
                        candidates.push(SizeOrd(child));
                    }
                }
            }
        } else {
            if seen.insert(PathBuf::from(&entry.path)) {
                result.push(entry);
            }
        }
    }

    result
}

impl HotspotScanner {
    /// Path drill-down scan (UI click to drill down)
    ///
    /// A single ancestor-aggregation scan returns size statistics for multiple subdirectory levels under the target path.
    /// Does not trigger multiple recursive WalkDir runs; target latency <1s.
    pub fn scan_path_direct(path: &str) -> Result<HotspotScanResult, String> {
        let start_time = std::time::Instant::now();
        let target = PathBuf::from(path);

        if !target.exists() {
            return Err(format!("路径不存在: {}", path));
        }
        if !target.is_dir() {
            return Err(format!("路径不是文件夹: {}", path));
        }

        // Single WalkDir + ancestor aggregation (max_depth=4, Fast mode)
        let cancel_flag = AtomicBool::new(false);
        let (_root_stats, ancestor_map) = aggregate_ancestor_stats(
            &target,
            4,     // max_depth
            false, // track_modified (fast mode)
            &cancel_flag,
            true, // path drill-down: no system directories in this context
        );

        let mut entries: Vec<HotspotEntry> = ancestor_map
            .into_iter()
            .map(|(child_path, stats)| {
                let depth = calculate_relative_depth(&target, &child_path);
                Self::build_entry(&child_path, &stats, depth as u8, false)
            })
            .collect();

        // Sort by size descending
        entries.sort_by(|a, b| b.total_size.cmp(&a.total_size));

        let total_folders_scanned = entries.len();
        let scanned_total_size = entries.iter().map(|e| e.total_size).sum();
        let scan_duration_ms = start_time.elapsed().as_millis() as u64;

        Ok(HotspotScanResult {
            entries,
            total_folders_scanned,
            scan_duration_ms,
            scanned_total_size,
            is_full_scan: false,
        })
    }
}

/// Folder statistics (for internal use; pub(crate) so the hotspot_engine submodule can reference it)
#[derive(Debug, Clone, Copy)]
pub(crate) struct FolderStats {
    pub(crate) total_size: u64,
    pub(crate) file_count: usize,
    pub(crate) last_modified: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_appdata_path() {
        let result = HotspotScanner::get_appdata_path();
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.exists());
    }

    #[test]
    fn test_is_protected_directory() {
        let windows_path = PathBuf::from("C:\\Windows");
        assert!(HotspotScanner::is_protected_directory(&windows_path));

        let program_files = PathBuf::from("C:\\Program Files");
        assert!(HotspotScanner::is_protected_directory(&program_files));
    }
}
