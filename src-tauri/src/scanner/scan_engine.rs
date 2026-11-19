// ============================================================================
// Scan engine - core scanning logic implementation
// Uses parallel scanning to optimize performance
// ============================================================================

use log::{debug, info, warn};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};
use walkdir::WalkDir;

use super::{CategoryScanResult, FileInfo, JunkCategory, ScanResult};
use crate::path_guard::is_rebuildable_system_cache_path;

/// Scan engine
pub struct ScanEngine {
    /// List of categories to scan
    categories: Vec<JunkCategory>,
    /// Maximum scan depth
    max_depth: usize,
}

impl ScanEngine {
    /// Create a new scan engine
    pub fn new() -> Self {
        ScanEngine {
            categories: JunkCategory::all(),
            max_depth: 10,
        }
    }

    /// Set the categories to scan
    pub fn with_categories(mut self, categories: Vec<JunkCategory>) -> Self {
        self.categories = categories;
        self
    }

    /// Set the maximum scan depth
    pub fn with_max_depth(mut self, depth: usize) -> Self {
        self.max_depth = depth;
        self
    }

    /// Run a full scan (all categories).
    ///
    /// M1 audit fix: the old engine spawned one unbounded thread per category and let
    /// every category independently re-walk its roots, so overlapping subtrees paid
    /// their metadata IO several times over. Now the roots of all selected categories
    /// are merged into a single plan (deduped, nested roots attributed to their own
    /// walker), a bounded worker pool walks every directory exactly once, and a
    /// per-file classifier attributes each file to every category whose roots
    /// contain it — so the wire shape (files may appear in several categories) and
    /// the per-root depth semantics are preserved.
    pub fn scan(&self) -> ScanResult {
        let mut requested: Vec<(JunkCategory, Vec<PathBuf>)> = Vec::new();
        for category in &self.categories {
            let mut resolved: Vec<PathBuf> = Vec::new();
            for scan_path in category.get_scan_paths() {
                resolved.extend(scan_path.resolve_all());
            }
            requested.push((category.clone(), resolved));
        }
        self.scan_resolved_roots(&requested)
    }

    /// Scan an explicit set of (category, resolved roots) pairs.
    ///
    /// Split from [`ScanEngine::scan`] so tests can feed fixture trees without
    /// touching environment variables. Canonicalization and dedup happen inside
    /// [`ScanPlan::build`], exactly where the old code did them per category.
    fn scan_resolved_roots(&self, requested: &[(JunkCategory, Vec<PathBuf>)]) -> ScanResult {
        let start_time = Instant::now();
        info!("开始单遍历扫描，共 {} 个分类", requested.len());

        // Selection order (deduped) defines the output order. The old engine pushed
        // results in thread-completion order, so the category list was nondeterministic
        // from run to run; deriving it from the selection is a strict improvement.
        let mut selection: Vec<JunkCategory> = Vec::new();
        for (category, _) in requested {
            if !selection.contains(category) {
                selection.push(category.clone());
            }
        }

        let plan = ScanPlan::build(requested);
        info!("扫描计划: {} 个去重扫描根", plan.roots.len());

        // Per-category accumulation, indexed by position in `selection`.
        let mut buckets: Vec<Vec<FileInfo>> = vec![Vec::new(); selection.len()];
        let mut recycle_results: Vec<(JunkCategory, bool, CategoryScanResult)> = Vec::new();
        // Roots whose walker panicked. A panicking root must not vanish silently (the
        // M2 contract): its categories surface in failed_categories instead.
        let failed_roots: Mutex<Vec<usize>> = Mutex::new(Vec::new());

        // Bounded worker pool over all roots: min(root count, available parallelism).
        // Work is claimed through an atomic cursor, so every root is walked exactly
        // once no matter how the workers progress — 18 unbounded threads become a
        // pool capped at the machine's core count. Declared outside `thread::scope`
        // because the scoped workers borrow it for the scope's lifetime.
        let next_root = AtomicUsize::new(0);
        // Per-root wall time, collected for the granularity diagnosis: when the
        // slowest root approaches the total wall time, the worker pool cannot go
        // wider and the traversal needs finer-grained work inside that root.
        let root_timings: Mutex<Vec<RootWalkTiming>> = Mutex::new(Vec::new());
        // The dedicated recycle-bin scanner shares this parallel window but is not
        // a root walk; time it separately so the wall-clock dominator is visible.
        let recycle_elapsed: Mutex<Option<Duration>> = Mutex::new(None);
        let worker_count = plan
            .roots
            .len()
            .min(
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1),
            )
            .max(1);
        thread::scope(|scope| {
            // Recycle bin entries use the dedicated scanner (physical $R/$I layout with
            // multi-user SIDs — WalkDir would not see the same picture as Explorer).
            // Run concurrently with the traversal, as the old engine did.
            let recycle_handles: Vec<_> = selection
                .iter()
                .filter(|category| matches!(category, JunkCategory::RecycleBin))
                .map(|category| {
                    let category = category.clone();
                    // Borrow, don't move: the spawned closure is `move` (for the
                    // cloned category), but the Mutex must stay shared via scope.
                    let recycle_elapsed = &recycle_elapsed;
                    scope.spawn(move || {
                        let mut category_result = CategoryScanResult::new(category.clone());
                        let recycle_started = Instant::now();
                        let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            super::recycle_bin::scan_current_user(&category, &mut category_result);
                        }))
                        .is_ok();
                        *recycle_elapsed
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner) =
                            Some(recycle_started.elapsed());
                        (ok, category_result)
                    })
                })
                .collect();

            let walker_handles: Vec<_> = (0..worker_count)
                .map(|_| {
                    scope.spawn(|| {
                        let mut emissions: Vec<(JunkCategory, FileInfo)> = Vec::new();
                        loop {
                            let index = next_root.fetch_add(1, Ordering::SeqCst);
                            if index >= plan.roots.len() {
                                break;
                            }
                            let walk_started = Instant::now();
                            let walked =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    walk_root(self, &plan, index, self.max_depth)
                                }));
                            match walked {
                                Ok(root_emissions) => {
                                    root_timings
                                        .lock()
                                        .unwrap_or_else(PoisonError::into_inner)
                                        .push(RootWalkTiming {
                                            path: plan.roots[index].path.clone(),
                                            elapsed: walk_started.elapsed(),
                                            emissions: root_emissions.len(),
                                        });
                                    emissions.extend(root_emissions);
                                }
                                Err(_) => {
                                    let mut failed =
                                        failed_roots.lock().unwrap_or_else(PoisonError::into_inner);
                                    failed.push(index);
                                }
                            }
                        }
                        emissions
                    })
                })
                .collect();

            for handle in walker_handles {
                match handle.join() {
                    Ok(emissions) => {
                        for (category, file_info) in emissions {
                            let index = selection
                                .iter()
                                .position(|c| c == &category)
                                .expect("分类来自 selection，必然存在");
                            buckets[index].push(file_info);
                        }
                    }
                    // A worker panicking outside walk_root is a bug; flag every
                    // root-holding category rather than losing the signal.
                    Err(_) => {
                        let mut failed =
                            failed_roots.lock().unwrap_or_else(PoisonError::into_inner);
                        failed.extend(0..plan.roots.len());
                    }
                }
            }

            for handle in recycle_handles {
                match handle.join() {
                    Ok((ok, category_result)) => recycle_results.push((
                        category_result.category.clone(),
                        ok,
                        category_result,
                    )),
                    Err(_) => recycle_results.push((
                        JunkCategory::RecycleBin,
                        false,
                        CategoryScanResult::new(JunkCategory::RecycleBin),
                    )),
                }
            }
        });

        log_root_timing_summary(
            &root_timings
                .into_inner()
                .unwrap_or_else(PoisonError::into_inner),
            plan.roots.len(),
            worker_count,
            start_time.elapsed(),
        );
        if let Some(recycle_ms) = recycle_elapsed
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
            .map(|elapsed| elapsed.as_millis())
        {
            info!(
                "[ScanEngine 耗时] 回收站专用扫描 {}ms（与遍历并行，可能才是墙钟主导者）",
                recycle_ms
            );
        }

        // Aggregate in deterministic selection order; categories with no files are
        // still listed (the old engine always pushed a CategoryScanResult per category).
        let mut result = ScanResult::new();
        let mut failed_categories: Vec<String> = Vec::new();
        for root_index in failed_roots
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
        {
            for (category, _) in &plan.roots[root_index].rules {
                let name = category.display_name().to_string();
                if !failed_categories.contains(&name) {
                    failed_categories.push(name);
                }
            }
        }

        for category in &selection {
            let mut category_result =
                match recycle_results.iter().position(|(c, _, _)| c == category) {
                    Some(index) => {
                        let (_, ok, category_result) = recycle_results.remove(index);
                        if !ok {
                            let name = category.display_name().to_string();
                            if !failed_categories.contains(&name) {
                                failed_categories.push(name);
                            }
                        }
                        category_result
                    }
                    None => {
                        let mut category_result = CategoryScanResult::new(category.clone());
                        let index = selection
                            .iter()
                            .position(|c| c == category)
                            .expect("分类来自 selection，必然存在");
                        for file_info in buckets[index].drain(..) {
                            category_result.add_file(file_info);
                        }
                        category_result
                    }
                };

            info!(
                "分类 {} 扫描完成: {} 个文件, {}",
                category.display_name(),
                category_result.file_count,
                category_result.human_readable_total_size()
            );
            category_result.sort_files_by_size_desc();
            result.add_category_result(category_result);
        }

        if !failed_categories.is_empty() {
            warn!(
                "本次扫描有 {} 个分类失败: {}",
                failed_categories.len(),
                failed_categories.join(", ")
            );
        }
        result.failed_categories = failed_categories;

        let duration = start_time.elapsed();
        result.set_duration(duration.as_millis() as u64);

        info!(
            "扫描完成，共发现 {} 个文件，总大小 {}，耗时 {}ms",
            result.total_file_count,
            result.human_readable_total_size(),
            result.scan_duration_ms
        );

        result
    }

    /// Scan a single category
    pub fn scan_category(&self, category: &JunkCategory) -> CategoryScanResult {
        let mut result = CategoryScanResult::new(category.clone());

        // The Recycle Bin's physical directory contains multi-user SIDs and Shell metadata; a dedicated scanner is required to stay consistent with Explorer.
        if matches!(category, JunkCategory::RecycleBin) {
            super::recycle_bin::scan_current_user(category, &mut result);
            result.sort_files_by_size_desc();
            return result;
        }

        let scan_paths = category.get_scan_paths();
        let patterns = category.get_file_patterns();

        // Collect all resolved paths, dedupe, then scan
        // e.g. %TEMP% and %TMP% may point to the same directory, avoid scanning twice
        let mut unique_paths: HashSet<std::path::PathBuf> = HashSet::new();
        let mut resolved_list: Vec<std::path::PathBuf> = Vec::new();

        for scan_path in scan_paths {
            for resolved_path in scan_path.resolve_all() {
                // Try to canonicalize the path to eliminate differences from symlinks, casing, etc.
                let canonical = match std::fs::canonicalize(&resolved_path) {
                    Ok(p) => p,
                    Err(_) => resolved_path.clone(), // if canonicalization fails, use a clone of the original path
                };
                if unique_paths.insert(canonical.clone()) {
                    resolved_list.push(canonical);
                } else {
                    debug!("跳过重复路径: {:?}", resolved_path);
                }
            }
        }

        for resolved_path in &resolved_list {
            debug!("扫描路径: {:?}", resolved_path);
            self.scan_path(resolved_path, category, &patterns, &mut result);
        }

        // Parallel threads and directory traversal randomize file order; sort uniformly by size descending,
        // so users see the biggest space hogs first within each category.
        result.sort_files_by_size_desc();
        result
    }

    /// Scan a specific path
    fn scan_path(
        &self,
        path: &Path,
        category: &JunkCategory,
        patterns: &[&str],
        result: &mut CategoryScanResult,
    ) {
        // Check whether the path exists
        if !path.exists() {
            debug!("路径不存在: {:?}", path);
            return;
        }

        // If it's a file, handle it directly
        if path.is_file() {
            if let Some(file_info) = self.get_file_info(path, category) {
                result.add_file(file_info);
            }
            return;
        }

        // Traverse the directory, counting files only; skip directory entries to avoid double counting with files
        let walker = WalkDir::new(path)
            .max_depth(self.max_depth)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                !self.is_system_protected(e.path())
                    && !self.is_persistent_app_profile_path(e.path())
            });

        for entry in walker.filter_map(|e| e.ok()) {
            let entry_path = entry.path();

            // Skip the root directory itself
            if entry_path == path {
                continue;
            }

            // Only process files, skip directories (avoids duplicate counting between calculate_dir_size and per-file stats)
            if !entry.file_type().is_file() {
                continue;
            }

            // Check whether it matches the patterns
            if !self.matches_patterns(entry_path, patterns) {
                continue;
            }

            // Second line of defense: even if new scan roots are added later, never treat WebView/Chromium persistent Profile data as junk.
            if self.is_persistent_app_profile_path(entry_path) {
                continue;
            }

            // Get file information from the metadata walkdir already carries
            if let Some(file_info) = self.file_info_from_metadata(entry_path, &entry, category) {
                result.add_file(file_info);
            }
        }
    }

    /// Build a FileInfo from a walkdir entry's metadata.
    ///
    /// On Windows the metadata comes from the directory read itself (FindFirstFileW
    /// data cached in the DirEntry), so this costs no extra syscall — an explicit
    /// fs::metadata here would be one avoidable CreateFile/stat per matched file.
    /// Values are identical to fs::metadata for regular files: symlinks never reach
    /// the callers (is_file() is false for them and follow_links is off).
    fn file_info_from_metadata(
        &self,
        path: &Path,
        entry: &walkdir::DirEntry,
        category: &JunkCategory,
    ) -> Option<FileInfo> {
        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                debug!("无法获取文件元数据 {:?}: {}", path, e);
                return None;
            }
        };
        Some(self.file_info_from_parts(path, &metadata, category))
    }

    /// Get file information (files only; directories are already skipped in scan_path)
    fn get_file_info(&self, path: &Path, category: &JunkCategory) -> Option<FileInfo> {
        let metadata = match fs::metadata(path) {
            Ok(m) => m,
            Err(e) => {
                debug!("无法获取文件元数据 {:?}: {}", path, e);
                return None;
            }
        };

        Some(self.file_info_from_parts(path, &metadata, category))
    }

    /// Shared FileInfo construction: size and mtime come straight from the metadata.
    fn file_info_from_parts(
        &self,
        path: &Path,
        metadata: &fs::Metadata,
        category: &JunkCategory,
    ) -> FileInfo {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "未知".to_string());

        let size = metadata.len();

        let modified_time = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        FileInfo::new(
            path.to_string_lossy().to_string(),
            name,
            size,
            modified_time,
            false, // always a file, directories never reach this function
            category.clone(),
        )
    }

    /// Check whether a file name matches the patterns
    fn matches_patterns(&self, path: &Path, patterns: &[&str]) -> bool {
        // If the patterns contain "*", match all files
        if patterns.contains(&"*") {
            return true;
        }

        let file_name = match path.file_name() {
            Some(name) => name.to_string_lossy().to_ascii_lowercase(),
            None => return false,
        };

        for pattern in patterns {
            if self.matches_glob(&file_name, pattern) {
                return true;
            }
        }

        false
    }

    /// Simple glob pattern matching
    fn matches_glob(&self, name: &str, pattern: &str) -> bool {
        // Windows file-name matching is case-insensitive; name and pattern must use the same normalization.
        let name = name.to_ascii_lowercase();
        let pattern = pattern.to_ascii_lowercase();

        if pattern == "*" {
            return true;
        }

        // Treat a single asterisk as a string of any length, supporting combinations of prefix, suffix and middle wildcards.
        let starts_with_wildcard = pattern.starts_with('*');
        let ends_with_wildcard = pattern.ends_with('*');
        let parts: Vec<&str> = pattern.split('*').filter(|part| !part.is_empty()).collect();
        if parts.is_empty() {
            return true;
        }

        let mut cursor = 0usize;
        for (index, part) in parts.iter().enumerate() {
            let is_first = index == 0;
            let is_last = index == parts.len() - 1;

            if is_first && !starts_with_wildcard {
                if !name[cursor..].starts_with(part) {
                    return false;
                }
                cursor += part.len();
                continue;
            }

            let Some(relative_position) = name[cursor..].find(part) else {
                return false;
            };
            let match_start = cursor + relative_position;
            if is_last && !ends_with_wildcard && !name[match_start..].ends_with(part) {
                return false;
            }
            cursor = match_start + part.len();
        }

        true
    }

    /// Check whether a path is system-protected (should not be scanned)
    fn is_system_protected(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy().to_ascii_lowercase();

        // Protect critical system directories
        let protected_paths = [
            "system32",
            "syswow64",
            "winsxs",
            "assembly",
            "\\windows\\servicing",
            "\\windows\\installer",
            "\\windows\\logs\\cbs",
            "\\program files",
            "\\program files (x86)",
            "\\programdata\\microsoft\\windows defender",
            "\\users\\default",
        ];

        for protected in &protected_paths {
            // Under Defender and System32, only cache subdirectories explicitly marked by the cleanup wizard are scannable; parent dirs stay protected.
            if path_str.contains(protected) && !is_rebuildable_system_cache_path(&path_str) {
                return true;
            }
        }

        // Protect critical system file extensions
        if let Some(ext) = path.extension() {
            let ext_str = ext.to_string_lossy().to_ascii_lowercase();
            let protected_exts = ["sys", "dll", "exe", "msi", "cat", "mum", "manifest"];
            // Only protect these extensions under the Windows directory
            if path_str.contains("\\windows\\") && protected_exts.contains(&ext_str.as_str()) {
                return true;
            }
        }

        false
    }

    /// Check whether a path is persistent WebView/Chromium app Profile data.
    ///
    /// Note:
    /// MSIX/Tauri/Electron/WebView2 apps often keep user state under LocalCache, e.g. Claude's
    /// EBWebView\Default. The IndexedDB, Local Storage, and various SQLite/LevelDB files in there
    /// are not pure caches; deleting them loses conversation lists, app preferences, or indexes.
    fn is_persistent_app_profile_path(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy().to_ascii_lowercase();
        if !path_str.contains("\\appdata\\local\\packages\\") && !path_str.contains("\\ebwebview\\")
        {
            return false;
        }

        let persistent_markers = [
            "\\ebwebview\\default\\",
            "\\local storage\\",
            "\\indexeddb\\",
            "\\session storage\\",
            "\\databases\\",
            "\\file system\\",
            "\\shared_proto_db\\",
            "\\shared dictionary\\",
            "\\autofillaimodelcache\\",
            "\\autofillstrikedatabase\\",
            "\\budgetdatabase\\",
            "\\browsingtopicssitedata",
            "\\browsingtopicsstate",
            "\\bookmarkmergedsurfaceordering",
        ];

        persistent_markers
            .iter()
            .any(|marker| path_str.contains(marker))
    }
}

impl Default for ScanEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Single-traversal scan plan (M1)
//
// The old engine walked each category's roots independently, so any overlap
// between roots was paid multiple times in metadata IO. The plan merges all
// roots up front: identical roots collapse into one entry carrying every
// referencing category, and a root nested under another root is pruned from the
// ancestor's walk so each directory is traversed exactly once — by the nested
// root's own walker, which keeps the old per-root depth semantics intact.
// ============================================================================

/// One resolved scan root plus the categories/patterns that reference it.
struct ScanRoot {
    /// Canonicalized path (`canonicalize` keeps the `\\?\` extended prefix on
    /// Windows, exactly like the old per-category roots did).
    path: PathBuf,
    /// `path_guard::compare_key` of `path`: separators normalized to `\`,
    /// `\\?\` stripped, ASCII lowercased — the same comparison form the delete
    /// engines use (M13), so containment checks share one rule.
    key: String,
    /// Number of `\`-separated components in `key` (`c:\a\b` → 3, `c:\` → 1).
    components: usize,
    /// Categories referencing this root with their file patterns (deduped per
    /// category; one root may serve several categories).
    rules: Vec<(JunkCategory, Vec<&'static str>)>,
    /// Keys of *directory* roots strictly nested under this root. This root's
    /// walker prunes them so their subtree is walked exactly once — by their own
    /// walker. File roots are never pruned: the ancestor walk handled them in
    /// the old engine and still does.
    nested_dir_keys: Vec<String>,
}

impl ScanRoot {
    fn new(path: PathBuf, category: JunkCategory, patterns: Vec<&'static str>) -> Self {
        let key = crate::path_guard::compare_key(&path);
        let components = key.split('\\').filter(|part| !part.is_empty()).count();
        ScanRoot {
            path,
            key,
            components,
            rules: vec![(category, patterns)],
            nested_dir_keys: Vec::new(),
        }
    }

    fn push_rule(&mut self, category: JunkCategory, patterns: Vec<&'static str>) {
        if !self.rules.iter().any(|(existing, _)| *existing == category) {
            self.rules.push((category, patterns));
        }
    }
}

/// Component-boundary containment on compare keys: `key` lives under `root_key`.
///
/// `c:\temp` must not match `c:\tempx\f.txt` — the next character after the
/// prefix has to be a separator (or the root itself ends with one, e.g. `c:\`).
fn key_contains(root_key: &str, key: &str) -> bool {
    if root_key.ends_with('\\') {
        key.starts_with(root_key)
    } else {
        key.len() > root_key.len()
            && key.starts_with(root_key)
            && key.as_bytes()[root_key.len()] == b'\\'
    }
}

struct ScanPlan {
    /// Every unique root in first-seen order; each gets its own walker.
    roots: Vec<ScanRoot>,
}

impl ScanPlan {
    /// Merge (category, roots) pairs into the plan: canonicalize, dedupe
    /// globally, collect per-category rules, then mark nested directory roots
    /// for pruning in their ancestors' walks.
    fn build(requested: &[(JunkCategory, Vec<PathBuf>)]) -> ScanPlan {
        let mut roots: Vec<ScanRoot> = Vec::new();
        for (category, paths) in requested {
            let patterns = category.get_file_patterns();
            for path in paths {
                // Old behavior: canonicalize to eliminate symlink/casing deltas;
                // on failure fall back to the literal path.
                let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.clone());
                match roots.iter_mut().find(|root| root.path == canonical) {
                    Some(root) => root.push_rule(category.clone(), patterns.clone()),
                    None => {
                        roots.push(ScanRoot::new(canonical, category.clone(), patterns.clone()))
                    }
                }
            }
        }

        for index in 0..roots.len() {
            roots[index].nested_dir_keys = roots
                .iter()
                .filter(|other| other.path.is_dir())
                .filter(|other| key_contains(&roots[index].key, &other.key))
                .map(|other| other.key.clone())
                .collect();
        }

        ScanPlan { roots }
    }
}

/// Reproduce the old per-root walk's filter chain for cross-root attribution.
///
/// When a file is attributed to a root other than its own walker's root (only
/// possible with nested roots), the old engine would only have reached that file
/// if every ancestor from the root down passed the filter predicate. Check the
/// same chain — including the root itself, because walkdir's `filter_entry`
/// applies the predicate to the root too, and a protected root yielded nothing.
fn chain_reachable(engine: &ScanEngine, file_path: &Path, root: &ScanRoot) -> bool {
    for ancestor in file_path.ancestors().skip(1) {
        if engine.is_system_protected(ancestor) || engine.is_persistent_app_profile_path(ancestor) {
            return false;
        }
        if crate::path_guard::compare_key(ancestor) == root.key {
            return true;
        }
    }
    false
}

/// Wall time of one root's traversal, collected for the granularity diagnosis:
/// when the slowest root approaches the total wall time, the bounded worker pool
/// cannot go wider and that root needs internal parallelism instead.
struct RootWalkTiming {
    path: PathBuf,
    elapsed: Duration,
    /// (category, file) pairs emitted by this root — a file in several roots
    /// counts once per root, which is fine for a distribution indicator.
    emissions: usize,
}

/// Log the per-root timing distribution: one info line with the headline numbers
/// (slowest root vs wall clock is the granularity verdict), the full sorted
/// distribution at debug level for anyone drilling in.
fn log_root_timing_summary(
    timings: &[RootWalkTiming],
    root_count: usize,
    worker_count: usize,
    wall: Duration,
) {
    if timings.is_empty() {
        return;
    }
    let total_walk_ms: u128 = timings.iter().map(|t| t.elapsed.as_millis()).sum();
    let wall_ms = wall.as_millis().max(1);
    let slowest = timings
        .iter()
        .max_by_key(|t| t.elapsed)
        .expect("timings 非空，max_by_key 必有值");
    let slowest_ms = slowest.elapsed.as_millis();
    info!(
        "[ScanEngine 耗时] 根 {} 个 / {} 工作线程 / 墙钟 {}ms / 遍历合计 {}ms / 最慢根 {}ms ({}, {} 项, 占墙钟 {:.0}%)",
        root_count,
        worker_count,
        wall_ms,
        total_walk_ms,
        slowest_ms,
        slowest.path.display(),
        slowest.emissions,
        slowest_ms * 100 / wall_ms,
    );
    let mut sorted: Vec<&RootWalkTiming> = timings.iter().collect();
    sorted.sort_by_key(|timing| std::cmp::Reverse(timing.elapsed));
    for timing in &sorted {
        debug!(
            "[ScanEngine 耗时] 根 {:>6}ms / {:>6} 项 / {}",
            timing.elapsed.as_millis(),
            timing.emissions,
            timing.path.display()
        );
    }
}

/// Walk one root and classify every file it contains.
///
/// Returns (category, file) pairs; a file can appear more than once when
/// several categories' roots contain it — the old per-root walks did the same.
fn walk_root(
    engine: &ScanEngine,
    plan: &ScanPlan,
    root_index: usize,
    max_depth: usize,
) -> Vec<(JunkCategory, FileInfo)> {
    let root = &plan.roots[root_index];
    let mut emissions: Vec<(JunkCategory, FileInfo)> = Vec::new();

    if root.path.is_file() {
        // Parity with the old scan_path: an explicit file root is emitted for every
        // referencing category without pattern matching.
        for (category, _) in &root.rules {
            if let Some(file_info) = engine.get_file_info(&root.path, category) {
                emissions.push((category.clone(), file_info));
            }
        }
        return emissions;
    }

    if !root.path.is_dir() {
        // resolve_all() filtered non-existent paths; a root that vanished between
        // resolution and traversal is simply empty (old scan_path logged and returned).
        debug!("扫描根不存在，跳过: {:?}", root.path);
        return emissions;
    }

    let nested_dir_keys = &root.nested_dir_keys;
    let walker = WalkDir::new(&root.path)
        .max_depth(max_depth)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            let entry_path = entry.path();
            if !nested_dir_keys.is_empty()
                && nested_dir_keys
                    .iter()
                    .any(|key| *key == crate::path_guard::compare_key(entry_path))
            {
                // The nested root has its own walker; pruning here is what makes
                // the whole scan a single traversal per directory.
                return false;
            }
            !engine.is_system_protected(entry_path)
                && !engine.is_persistent_app_profile_path(entry_path)
        });

    for entry in walker.filter_map(|entry| entry.ok()) {
        // Skip the root itself (the old code compared entry_path == path).
        if entry.depth() == 0 {
            continue;
        }
        // Only files are classified; directories never reach the result.
        if !entry.file_type().is_file() {
            continue;
        }

        let entry_path = entry.path();
        let file_key = crate::path_guard::compare_key(entry_path);
        let file_components = root.components + entry.depth();

        // Classifier: attribute the file to every root that contains it, within
        // that root's depth budget — the union of the old per-root walks.
        for (candidate_index, candidate) in plan.roots.iter().enumerate() {
            if candidate.components > file_components || !key_contains(&candidate.key, &file_key) {
                continue;
            }
            if file_components - candidate.components > max_depth {
                continue;
            }
            if candidate_index != root_index && !chain_reachable(engine, entry_path, candidate) {
                continue;
            }
            for (category, patterns) in &candidate.rules {
                if engine.matches_patterns(entry_path, patterns) {
                    // walkdir already carries the directory-entry metadata; taking it
                    // from the entry avoids one CreateFile/stat per matched file.
                    if let Some(file_info) =
                        engine.file_info_from_metadata(entry_path, &entry, category)
                    {
                        emissions.push((category.clone(), file_info));
                    }
                }
            }
        }
    }

    emissions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glob_matching() {
        let engine = ScanEngine::new();

        assert!(engine.matches_glob("test.log", "*.log"));
        assert!(engine.matches_glob("test.LOG", "*.log"));
        assert!(!engine.matches_glob("test.txt", "*.log"));
        assert!(engine.matches_glob("thumbcache_256.db", "thumbcache_*.db"));
    }

    #[test]
    fn test_webview_profile_data_is_protected() {
        let engine = ScanEngine::new();

        assert!(engine.is_persistent_app_profile_path(Path::new(
            r"C:\Users\PC\AppData\Local\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Local\app.formatif.desktop\EBWebView\Default\IndexedDB\index.leveldb\LOG"
        )));
        assert!(engine.is_persistent_app_profile_path(Path::new(
            r"C:\Users\PC\AppData\Local\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Local\app.formatif.desktop\EBWebView\Default\BrowsingTopicsSiteData"
        )));
        assert!(!engine.is_persistent_app_profile_path(Path::new(
            r"C:\Users\PC\AppData\Local\Google\Chrome\User Data\Default\Code Cache\js\cache"
        )));
    }

    #[test]
    fn test_rebuildable_system_cache_exception_is_narrow() {
        let engine = ScanEngine::new();

        // The exception only covers explicit cache directories; it must not open up all of System32 or the Defender root.
        assert!(!engine.is_system_protected(Path::new(r"C:\Windows\System32\d3d_cache\shader.bin")));
        assert!(!engine.is_system_protected(Path::new(
            r"C:\ProgramData\Microsoft\Windows Defender\Support\MPLog.log"
        )));
        assert!(engine.is_system_protected(Path::new(r"C:\Windows\System32\kernel32.dll")));
        assert!(engine.is_system_protected(Path::new(
            r"C:\ProgramData\Microsoft\Windows Defender\Quarantine\entry.bin"
        )));
    }

    #[test]
    fn test_category_files_are_sorted_by_size_desc() {
        let mut result = CategoryScanResult::new(JunkCategory::WindowsTemp);
        for (name, size) in [
            ("small.tmp", 10_u64),
            ("large.tmp", 4096),
            ("medium.tmp", 512),
        ] {
            result.add_file(FileInfo::new(
                format!(r"C:\Users\Test\AppData\Local\Temp\{}", name),
                name.to_string(),
                size,
                0,
                false,
                JunkCategory::WindowsTemp,
            ));
        }

        result.sort_files_by_size_desc();

        let sizes = result
            .files
            .iter()
            .map(|file| file.size)
            .collect::<Vec<_>>();
        assert_eq!(sizes, vec![4096, 512, 10]);
        // Sorting only changes display order, not the category statistics.
        assert_eq!(result.file_count, 3);
        assert_eq!(result.total_size, 4618);
    }

    // ==================================================================
    // M1: single-traversal engine vs the old per-root walk (reference)
    // ==================================================================

    use std::collections::BTreeSet;

    /// Reference implementation of the OLD per-root walk semantics: per category,
    /// dedupe its canonical roots, walk each root with the same filter predicate,
    /// and pattern-match every file. The single-traversal engine must produce the
    /// exact same (category, path) set on any input.
    fn reference_scan_per_root(
        engine: &ScanEngine,
        requested: &[(JunkCategory, Vec<PathBuf>)],
        max_depth: usize,
    ) -> BTreeSet<(String, String)> {
        let mut expected = BTreeSet::new();
        for (category, paths) in requested {
            // The dedicated recycle-bin scanner predates this refactor and is out of M1's scope.
            if matches!(category, JunkCategory::RecycleBin) {
                let mut category_result = CategoryScanResult::new(category.clone());
                crate::scanner::recycle_bin::scan_current_user(category, &mut category_result);
                for file in &category_result.files {
                    expected.insert((category.display_name().to_string(), file.path.clone()));
                }
                continue;
            }

            let patterns = category.get_file_patterns();
            let mut unique_roots: Vec<PathBuf> = Vec::new();
            for path in paths {
                let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.clone());
                if !unique_roots.contains(&canonical) {
                    unique_roots.push(canonical);
                }
            }

            for root in unique_roots {
                if root.is_file() {
                    if let Some(file_info) = engine.get_file_info(&root, category) {
                        expected.insert((category.display_name().to_string(), file_info.path));
                    }
                    continue;
                }
                if !root.is_dir() {
                    continue;
                }
                let walker = WalkDir::new(&root)
                    .max_depth(max_depth)
                    .follow_links(false)
                    .into_iter()
                    .filter_entry(|entry| {
                        !engine.is_system_protected(entry.path())
                            && !engine.is_persistent_app_profile_path(entry.path())
                    });
                for entry in walker.filter_map(|entry| entry.ok()) {
                    if entry.depth() == 0 || !entry.file_type().is_file() {
                        continue;
                    }
                    if engine.matches_patterns(entry.path(), &patterns) {
                        if let Some(file_info) = engine.get_file_info(entry.path(), category) {
                            expected.insert((category.display_name().to_string(), file_info.path));
                        }
                    }
                }
            }
        }
        expected
    }

    fn result_to_set(result: &ScanResult) -> BTreeSet<(String, String)> {
        let mut actual = BTreeSet::new();
        for category in &result.categories {
            for file in &category.files {
                actual.insert((
                    category.category.display_name().to_string(),
                    file.path.clone(),
                ));
            }
        }
        actual
    }

    fn unique_temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lightc_m1_{}_{}_{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn category_file_names(result: &ScanResult, category: &JunkCategory) -> Vec<String> {
        result
            .categories
            .iter()
            .filter(|c| &c.category == category)
            .flat_map(|c| c.files.iter().map(|file| file.name.clone()))
            .collect()
    }

    #[test]
    fn test_single_traversal_matches_per_root_reference() {
        let engine = ScanEngine::new();
        let base = unique_temp_dir("parity");
        // Layout (depths measured from root A):
        //   A/top.log             → LogFiles only
        //   A/sub = root B        → nested root, walked by its own walker
        //   A/sub/nested.log      → LogFiles AND WindowsTemp (cross-root attribution)
        //   A/sub/nested.tmp      → WindowsTemp only
        //   A/sub/deep/inner.log  → depth 3 from A, depth 2 from B
        let dir_a = base.join("A");
        let dir_b = dir_a.join("sub");
        let dir_deep = dir_b.join("deep");
        fs::create_dir_all(&dir_deep).unwrap();
        fs::write(dir_a.join("top.log"), b"1").unwrap();
        fs::write(dir_b.join("nested.log"), b"22").unwrap();
        fs::write(dir_b.join("nested.tmp"), b"333").unwrap();
        fs::write(dir_deep.join("inner.log"), b"4444").unwrap();

        // The duplicated dir_b mimics %TEMP% vs %TMP% resolving to the same directory.
        let requested = vec![
            (JunkCategory::LogFiles, vec![dir_a.clone()]),
            (
                JunkCategory::WindowsTemp,
                vec![dir_b.clone(), dir_b.clone()],
            ),
        ];

        let reference = reference_scan_per_root(&engine, &requested, engine.max_depth);
        let result = engine.scan_resolved_roots(&requested);

        assert_eq!(
            reference,
            result_to_set(&result),
            "单遍历结果必须与旧逐根遍历逐条一致"
        );

        // Multi-category attribution: nested.log lives under both roots.
        let log_files = category_file_names(&result, &JunkCategory::LogFiles);
        assert!(log_files.contains(&"top.log".to_string()));
        assert!(log_files.contains(&"nested.log".to_string()));
        assert!(log_files.contains(&"inner.log".to_string()));
        // WindowsTemp sees both files under its root, and the duplicate root must not
        // double-count nested.tmp (old per-category dedupe preserved).
        let temp_files = category_file_names(&result, &JunkCategory::WindowsTemp);
        assert_eq!(
            temp_files
                .iter()
                .filter(|name| *name == "nested.tmp")
                .count(),
            1,
            "重复根去重后每个文件只计一次"
        );
        assert!(temp_files.contains(&"nested.log".to_string()));

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn test_nested_root_depth_anchor_matches_reference() {
        // max_depth 2: A/d1/d2/x.log sits at depth 3 from A (outside A's walk) but
        // depth 1 from B = A/d1/d2 (inside B's walk). The old per-root walks produced
        // exactly this asymmetry; the single traversal must reproduce it via the
        // per-root depth budget, not lose the file or leak it into LogFiles.
        let engine = ScanEngine::new().with_max_depth(2);
        let base = unique_temp_dir("depth");
        let dir_a = base.join("A");
        let dir_b = dir_a.join("d1").join("d2");
        fs::create_dir_all(&dir_b).unwrap();
        fs::write(dir_a.join("near.log"), b"1").unwrap();
        fs::write(dir_b.join("x.log"), b"22").unwrap();

        let requested = vec![
            (JunkCategory::LogFiles, vec![dir_a.clone()]),
            (JunkCategory::WindowsTemp, vec![dir_b.clone()]),
        ];

        let reference = reference_scan_per_root(&engine, &requested, engine.max_depth);
        let result = engine.scan_resolved_roots(&requested);
        assert_eq!(reference, result_to_set(&result));

        let log_files = category_file_names(&result, &JunkCategory::LogFiles);
        assert!(log_files.contains(&"near.log".to_string()));
        assert!(
            !log_files.contains(&"x.log".to_string()),
            "A 根深度预算外的文件不得进入 LogFiles"
        );
        let temp_files = category_file_names(&result, &JunkCategory::WindowsTemp);
        assert!(temp_files.contains(&"x.log".to_string()));

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn test_plan_merges_duplicate_roots_and_marks_nesting() {
        let base = unique_temp_dir("plan");
        let dir_a = base.join("A");
        let dir_b = dir_a.join("sub");
        fs::create_dir_all(&dir_b).unwrap();

        let plan = ScanPlan::build(&[
            (JunkCategory::LogFiles, vec![dir_a.clone(), dir_a.clone()]),
            (JunkCategory::WindowsTemp, vec![dir_b.clone()]),
        ]);

        assert_eq!(plan.roots.len(), 2, "重复根必须合并为一个");
        assert_eq!(
            plan.roots[0].rules.len(),
            1,
            "同一分类重复引用不产生重复规则"
        );
        // The plan canonicalizes roots, so the expected key comes from the canonical
        // form too (temp dirs may hand out 8.3 short names like PASSAC~1).
        let canonical_dir_b = fs::canonicalize(&dir_b).unwrap();
        assert_eq!(
            plan.roots[0].nested_dir_keys,
            vec![crate::path_guard::compare_key(&canonical_dir_b)],
            "嵌套根必须被标记，供祖先遍历剪枝"
        );

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn test_key_contains_respects_component_boundary() {
        assert!(key_contains(r"c:\temp", r"c:\temp\a.txt"));
        assert!(
            !key_contains(r"c:\temp", r"c:\tempx\a.txt"),
            "前缀必须止于组件边界"
        );
        assert!(key_contains(r"c:\", r"c:\anything"));
        assert!(
            !key_contains(r"c:\temp", r"c:\temp"),
            "根不包含其自身（目录对目录）"
        );
    }

    #[test]
    #[ignore = "真机对照验证（M1 上线门槛）：全分类真实根扫描较慢；手动运行 cargo test --lib -- --ignored。两次扫描间文件系统若发生变化可能产生瞬时差异"]
    fn test_real_machine_parity_reference_vs_single_traversal() {
        // Surface the per-root timing summary with --nocapture (the app's
        // env_logger::init lives in run(), which tests never call).
        let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
            .try_init();
        let engine = ScanEngine::new();
        let mut requested: Vec<(JunkCategory, Vec<PathBuf>)> = Vec::new();
        for category in JunkCategory::all() {
            let mut resolved: Vec<PathBuf> = Vec::new();
            for scan_path in category.get_scan_paths() {
                resolved.extend(scan_path.resolve_all());
            }
            requested.push((category, resolved));
        }

        let reference = reference_scan_per_root(&engine, &requested, engine.max_depth);
        let result = engine.scan_resolved_roots(&requested);
        let actual = result_to_set(&result);

        let missing: Vec<_> = reference.difference(&actual).collect();
        let extra: Vec<_> = actual.difference(&reference).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "真机对照发现差异。\n旧实现有而新实现缺失: {missing:?}\n新实现多出: {extra:?}"
        );
    }
}
