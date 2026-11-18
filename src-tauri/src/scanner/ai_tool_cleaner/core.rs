//! Core logic for scanning and deletion.

use std::path::{Component, Path, PathBuf};
use walkdir::WalkDir;

use super::tools::build_specs;
use super::types::{
    AiCacheDeleteFailure, AiCacheDeleteResult, AiCacheEntry, AiCacheFile, AiCacheFileList,
    AiToolGroup, AiToolScanResult,
};

/// Subdirectory names allowed for cleanup. **Only these names count**; anything not on the list is left untouched.
///
/// Casing follows the names actually created on Windows: Electron-based apps use `Cache`/`GPUCache`,
/// some CLI tools use lowercase `cache`, so both are listed.
const CLEANABLE_DIR_NAMES: &[&str] = &[
    "Cache",
    "cache",
    "GPUCache",
    "GrShaderCache",
    "GPUPersistentCache",
    "DawnGraphiteCache",
    "DawnWebGPUCache",
    "Code Cache",
    "logs",
    "Crashpad",
    // The following three are cache directories shared by VS Code-based clients (Cursor / Trae / Devin / Windsurf),
    // deleting them only causes regeneration at next startup, without affecting installed extensions or settings:
    // - CachedData             V8 compilation snapshot, largest (105MB on Devin)
    // - CachedExtensionVSIXs   extension package cache, redownloaded when updating extensions
    // - CachedProfilesData     config read cache
    "CachedData",
    "CachedExtensionVSIXs",
    "CachedProfilesData",
    // Windsurf's downloaded and unpacked extension artifacts, distinct from the CachedExtensionVSIXs above (raw install packages)
    "CachedExtensions",
    // Other Chromium caches: shader compilation results and Service Worker script compilation results.
    // Directory names vary by version (older GrShaderCache, newer GraphiteDawnCache), so keep several;
    // deleting them only makes the next startup recompile once.
    "ShaderCache",
    "ScriptCache",
    "GraphiteDawnCache",
];

/*
Directories that look like caches but **must absolutely never** be whitelisted (documented here to prevent being added by mistake):

- `WebStorage` / `IndexedDB` / `Local Storage` / `Session Storage`
  Clients store login state and site data in these directories; deleting them logs the user out.
  Devin's `WebStorage` is 66MB — tempting by size, but it is user data in nature.
  It also contains `CacheStorage` (83MB on Devin), which is equally off-limits: the path is inside `WebStorage`,
  so deleting it loses offline site data.
- `Network`   stores cookies; a few dozen KB can still cost the user all login state.
- `Backups`   unsaved-file backups for VS Code-based apps; deleting loses unsaved edits.
- `User`      settings, snippets, workspaceStorage, globalStorage.
- `cli`       the CLI binary itself for some clients, not a cache.
- `plugins`   installed plugin binaries. Their subdirectories are often named `cache`, but hold plugin packages,
              measured 224MB for WorkBuddy and 31MB for Codex; deleting breaks the corresponding features.
- `extensions` / `node_modules`
              the extension binaries and their dependencies. There really are subdirectories named `cache`,
              but they are inside dependencies, so deleting breaks the extension (see the skip logic below).
- `ModularData`  TRAE's chat history database, encrypted and machine-bound; deleting loses all history.
- `sessions`  session history of various CLIs (Codex 136MB; the whole Grok Build suite lives here).
- `bin`       executables of CLIs such as Grok Build / Qwen.
*/

/// Maximum depth to search below a tool's root directory.
///
/// Some tools keep caches not directly under the root (e.g. `.codeium/windsurf/<version>/Cache`),
/// scanning only one level would miss them; but going infinitely deep would mistake the tool's own runtime directories for caches.
const MAX_SCAN_DEPTH: usize = 3;

/// Maximum number of file entries returned per call in the detail modal.
///
/// Normal cache directories have only a few thousand files, but "mistaken-for-cache" directories can be deep and wide.
/// Without a cap, IPC serialization and frontend memory would both blow up, so truncate and clearly state that only the first N entries are shown.
const MAX_LISTED_FILES: usize = 50_000;

pub struct AiToolScanner {
    appdata: PathBuf,
    local_appdata: PathBuf,
    user_profile: PathBuf,
}

impl Default for AiToolScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl AiToolScanner {
    pub fn new() -> Self {
        // Consistent with the social scanner: environment variables take priority, falling back to the default path,
        // so a missing variable doesn't fail the whole scan.
        let user_profile = std::env::var("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(r"C:\Users\Default"));
        let local_appdata = std::env::var("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| user_profile.join("AppData").join("Local"));
        let appdata = std::env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| user_profile.join("AppData").join("Roaming"));

        Self {
            appdata,
            local_appdata,
            user_profile,
        }
    }

    pub fn scan(&self) -> AiToolScanResult {
        let mut groups = Vec::new();

        for spec in build_specs(&self.appdata, &self.local_appdata, &self.user_profile) {
            let mut entries = Vec::new();
            for root in &spec.roots {
                self.collect_cleanable_children(root, spec.extra_cleanable_names, &mut entries);
            }
            if entries.is_empty() {
                continue;
            }

            // Put the largest first, so the user immediately sees the biggest space saver
            entries.sort_by(|a, b| b.size.cmp(&a.size));

            groups.push(AiToolGroup {
                tool_id: spec.id.to_string(),
                tool_name: spec.name.to_string(),
                total_size: entries.iter().map(|e| e.size).sum(),
                file_count: entries.iter().map(|e| e.file_count).sum(),
                entries,
            });
        }

        // Sort by occupied size descending: the first item seen is the one that frees the most space.
        // For equal sizes, keep the order from the tools.rs list (sort_by is stable),
        // so frequently used tools still come first among equal-sized ones.
        groups.sort_by(|a, b| b.total_size.cmp(&a.total_size));

        AiToolScanResult {
            total_size: groups.iter().map(|g| g.total_size).sum(),
            total_files: groups.iter().map(|g| g.file_count).sum(),
            groups,
        }
    }

    /// Find whitelisted directories under root and count them. Return silently if root does not exist.
    ///
    /// `extra_names` are directory names **additionally allowed for this tool** (e.g. Codex's `.tmp`).
    /// Not in the global whitelist because a name like `.tmp` may be an important staging directory in other tools.
    /// Visible to the `tests` module: whitelist boundaries must be validated with real directory structures.
    pub(super) fn collect_cleanable_children(
        &self,
        root: &Path,
        extra_names: &[&str],
        entries: &mut Vec<AiCacheEntry>,
    ) {
        if !root.is_dir() {
            return;
        }
        self.walk_for_caches(root, 0, extra_names, entries);
    }

    /// Recursively find whitelisted directories, up to MAX_SCAN_DEPTH levels.
    ///
    /// **Stop on match**: a whitelisted directory will not contain further caches needing separate counting,
    /// going deeper would just count descendants of the same Cache twice.
    fn walk_for_caches(
        &self,
        dir: &Path,
        depth: usize,
        extra_names: &[&str],
        entries: &mut Vec<AiCacheEntry>,
    ) {
        if depth > MAX_SCAN_DEPTH {
            return;
        }
        let children = match std::fs::read_dir(dir) {
            Ok(children) => children,
            Err(error) => {
                // Insufficient permissions or directory in use: skip this level, don't fail the whole scan
                log::debug!("跳过 {}：{error}", dir.display());
                return;
            }
        };

        for child in children.filter_map(|entry| entry.ok()) {
            // Symlinks and junctions report `is_dir()` false here (junctions actually report
            // is_dir=false / is_symlink=true), so they naturally are not followed to other drives,
            // and nothing outside the tool's root gets counted or deleted via links.
            if !child.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = child.file_name().to_string_lossy().to_string();
            // Skip VCS directories entirely: the whitelist matches by name, while `.git/logs` holds the reflog,
            // deleting it would lose the user's version history — not at all the same as "cleaning cache".
            if name == ".git" {
                continue;
            }
            // Skip the `plugins` directory entirely: its `cache` holds **installed plugin binaries**, not caches.
            // WorkBuddy's plugins/cache measured 224MB (built-in plugins + connectors),
            // Codex's plugins/cache is the openai-bundled runtime — deleting breaks features outright.
            if name == "plugins" {
                continue;
            }
            // Skip `extensions` / `node_modules` entirely as well.
            // VS Code-based clients have extension paths like `extensions/<extension>/node_modules/<package>/cache`,
            // which falls within the scan depth and would be mistaken for a cache by name — but it is inside extension dependencies,
            // deleting breaks the extension. The user wants the client's cache, not the extension's own.
            if name == "extensions" || name == "node_modules" {
                continue;
            }
            if is_cleanable_name(&name, extra_names) {
                let (size, file_count) = Self::measure_dir(&child.path());
                if file_count > 0 {
                    // Walk up the path looking for links: the user may have moved `.workbuddy`
                    // entirely to the D drive with FolderMove, so what is scanned here is `.workbuddy\logs`，
                    // and judging by the path alone would attribute the usage to the C drive
                    let link = crate::scanner::reparse::analyze(&child.path());
                    entries.push(AiCacheEntry {
                        name,
                        path: child.path().to_string_lossy().to_string(),
                        size,
                        file_count,
                        link,
                    });
                }
                continue;
            }
            self.walk_for_caches(&child.path(), depth + 1, extra_names, entries);
        }
    }

    /// Determines whether a path is a cache directory managed by this module.
    ///
    /// Deletion is irreversible; paths from the frontend are **never trusted** and must be re-validated:
    /// (1) the path must not contain `.` / `..`; (2) the directory name must be allowed for this tool; (3) it must be under its root directory.
    pub fn is_managed_cache_dir(&self, path: &Path) -> bool {
        // `Path::starts_with` is a **lexical** comparison:`...\Cursor\Cache\..\..\..\logs` would be judged
        // as under the root, while `remove_dir_all` operates on the resolved real path —
        // without blocking such paths first, the prefix check would be meaningless.
        if path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        {
            return false;
        }

        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            return false;
        };

        build_specs(&self.appdata, &self.local_appdata, &self.user_profile)
            .iter()
            .any(|spec| {
                if !is_cleanable_name(name, spec.extra_cleanable_names) {
                    return false;
                }
                spec.roots.iter().any(|root| {
                    // Must be strictly **below** the root: the root directory itself cannot be deleted
                    path.components().count() > root.components().count()
                        && starts_with_ignore_case(path, root)
                })
            })
    }

    /// Deletes the selected cache directories, returning the success count and failure details.
    ///
    /// Failure is the norm rather than the exception: while a tool is running, Chromium cache files are often memory-mapped,
    /// and Windows refuses deletion with a sharing violation. Previously this only logged and returned the success count as the whole result,
    /// so users saw "deletion complete" while the size did not change at all. Failures must be returned to the frontend with their reasons.
    pub fn delete_dirs(&self, paths: &[String]) -> AiCacheDeleteResult {
        let mut deleted = 0usize;
        let mut failed = Vec::new();

        for raw in paths {
            let path = PathBuf::from(raw);
            if !self.is_managed_cache_dir(&path) {
                log::warn!("拒绝删除非托管路径: {raw}");
                failed.push(AiCacheDeleteFailure {
                    path: raw.clone(),
                    reason: "不在可清理范围内".to_string(),
                });
                continue;
            }
            match std::fs::remove_dir_all(&path) {
                Ok(()) => {
                    deleted += 1;
                    log::info!("已删除缓存目录: {raw}");
                }
                Err(error) => {
                    // A directory being in use is common (tool still open); log it and continue deleting other directories
                    log::warn!("删除失败 {raw}: {error}");
                    failed.push(AiCacheDeleteFailure {
                        path: raw.clone(),
                        reason: error.to_string(),
                    });
                }
            }
        }

        AiCacheDeleteResult { deleted, failed }
    }

    /// Lists all files under a cache directory (truncated and flagged when over the limit).
    ///
    /// Called on demand (only when the user clicks "View All"); do not stuff all files into the result during scan —
    /// a single tool can have thousands of files, and returning all of them would bloat the scan result.
    pub fn list_files(&self, path: &Path) -> AiCacheFileList {
        let mut files = Vec::new();
        let mut truncated = false;
        for entry in WalkDir::new(path)
            .follow_links(false)
            .into_iter()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_file())
        {
            if files.len() >= MAX_LISTED_FILES {
                truncated = true;
                break;
            }
            if let Ok(metadata) = entry.metadata() {
                files.push(AiCacheFile {
                    path: entry.path().to_string_lossy().to_string(),
                    size: metadata.len(),
                    // The detail modal sorts by modification time; metadata is already being read here, so fetching mtime costs no extra syscall
                    modified_time: metadata
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|elapsed| elapsed.as_secs())
                        .unwrap_or(0),
                });
            }
        }
        AiCacheFileList { files, truncated }
    }

    /// Counts the total size and number of all files in a directory. Skip unreadable entries (cache directories are often in use).
    fn measure_dir(path: &Path) -> (u64, usize) {
        let mut size = 0u64;
        let mut file_count = 0usize;
        for entry in WalkDir::new(path)
            .follow_links(false)
            .into_iter()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_file())
        {
            if let Ok(metadata) = entry.metadata() {
                size += metadata.len();
                file_count += 1;
            }
        }
        (size, file_count)
    }
}

/// Whether a directory name is cleanable: global whitelist or additionally authorized for this tool.
fn is_cleanable_name(name: &str, extra_names: &[&str]) -> bool {
    CLEANABLE_DIR_NAMES.contains(&name) || extra_names.contains(&name)
}

/// Case-insensitively determines whether `path` is under `root`.
///
/// Case-insensitivity is required: scanning gets the **actual** directory names on disk, while the list contains fixed literals.
/// if their casing differs, `Path::starts_with` judges "not under root",
/// showing up as "scannable, selectable, but undeletable", with the UI reporting just 0.
fn starts_with_ignore_case(path: &Path, root: &Path) -> bool {
    let mut path_components = path.components();
    for root_component in root.components() {
        let Some(path_component) = path_components.next() else {
            return false;
        };
        let path_part = path_component.as_os_str().to_string_lossy();
        let root_part = root_component.as_os_str().to_string_lossy();
        if !path_part.eq_ignore_ascii_case(&root_part) {
            return false;
        }
    }
    true
}
