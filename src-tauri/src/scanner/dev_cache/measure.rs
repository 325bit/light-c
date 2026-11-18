//! Scanning and measurement.
//!
//! Unlike LightC's existing `ai_tool_cleaner`, which **finds** whitelist subdirectories under a tool root,
//! here each target is a definite directory and we just measure its size directly.
//!
//! Parallel per-level BFS with rayon: one directory level is spread across all cores in one `par_iter`, then we move to the next level.
//! Do not use "recursion + par_iter", which would blow up into exponentially many tasks in deep directories.

use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::time::Instant;
use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};
use winreg::RegKey;

use super::targets::{self, Env, ProbeHive, RegProbe, VersionPattern, VersionSpec};
use super::types::{
    DevCacheEntry, DevCacheGroup, DevCacheScanResult, DevRisk, VersionEntry, VersionGroup,
    VersionLeftoverScanResult,
};

#[derive(Default)]
struct LevelPart {
    bytes: u64,
    files: usize,
    skipped: usize,
    dirs: Vec<PathBuf>,
}

/// Add a `\\?\` prefix to an absolute path to bypass MAX_PATH.
///
/// Also normalize forward slashes to backslashes: Win32 extended paths `\\?\` **do not accept** `C:/Windows`,
/// while callers (Git Bash, some shells) silently convert paths to forward slashes.
pub fn long_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy().replace('/', "\\");
    if text.starts_with(r"\\?\") {
        return PathBuf::from(text);
    }
    if !Path::new(&text).is_absolute() {
        return PathBuf::from(text);
    }
    if let Some(rest) = text.strip_prefix(r"\\") {
        PathBuf::from(format!(r"\\?\UNC\{}", rest))
    } else {
        PathBuf::from(format!(r"\\?\{}", text))
    }
}

/// Display path: strip the `\\?\` prefix and normalize forward slashes.
pub fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\");
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{}", rest)
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        text
    }
}

fn read_level(dir: &Path) -> LevelPart {
    let mut part = LevelPart::default();
    let reader = match std::fs::read_dir(long_path(dir)) {
        Ok(reader) => reader,
        Err(_) => {
            // Insufficient permissions or directory in use: skip this level rather than failing the whole scan
            part.skipped += 1;
            return part;
        }
    };
    for entry in reader.filter_map(|item| item.ok()) {
        match entry.file_type() {
            // Symlinks and junctions have is_dir() == false, so they are naturally not followed onto other drives
            Ok(kind) if kind.is_dir() => part.dirs.push(entry.path()),
            Ok(kind) if kind.is_file() => match entry.metadata() {
                Ok(meta) => {
                    part.bytes += meta.len();
                    part.files += 1;
                }
                Err(_) => part.skipped += 1,
            },
            _ => {}
        }
    }
    part
}

/// Measure directory size level by level in parallel; return (bytes, file count, skipped count).
pub fn measure(root: &Path) -> (u64, usize, usize) {
    let mut bytes = 0u64;
    let mut files = 0usize;
    let mut skipped = 0usize;
    let mut frontier: Vec<PathBuf> = vec![root.to_path_buf()];

    while !frontier.is_empty() {
        let parts: Vec<LevelPart> = frontier.par_iter().map(|dir| read_level(dir)).collect();
        let mut next: Vec<PathBuf> = Vec::new();
        for part in parts {
            bytes += part.bytes;
            files += part.files;
            skipped += part.skipped;
            next.extend(part.dirs);
        }
        frontier = next;
    }
    (bytes, files, skipped)
}

/// Full scan of **toolchain caches only** — what the "dev cache cleanup" page shows.
///
/// The two other halves of this module live on the "leftover & cache" page and have their own entry points:
/// multi-version directories (`scan_version_leftovers`) and other software caches (`scan_other_app_caches`).
/// Running them here would make each page traverse the other's directories for nothing.
pub fn scan() -> DevCacheScanResult {
    scan_targets(|group| group != targets::OTHER_GROUP)
}

/// Full scan of the **"other software caches"** group only — rendered on the "leftover & cache" page.
///
/// These are the downloaded driver installers / launcher web caches that garbage cleanup and driver cleanup both
/// leave alone (NVIDIA, Epic, Steam, DaVinci…). Measurement and deletion validation are the same as the toolchain
/// caches — only the owning page differs, because the question they answer ("what did software leave behind")
/// is the one that page already asks.
pub fn scan_other_app_caches() -> DevCacheScanResult {
    scan_targets(|group| group == targets::OTHER_GROUP)
}

/// Shared implementation: measure every target whose group passes `keep`, then assemble groups and counters.
///
/// The group filter runs **before** measuring, not after: skipping a whole page's worth of directories here is
/// exactly the point of splitting the commands, and filtering afterwards would leave the traversal cost in place.
fn scan_targets(keep: impl Fn(&str) -> bool) -> DevCacheScanResult {
    let started = Instant::now();
    let env = Env::detect();

    let specs: Vec<targets::TargetSpec> = targets::targets(&env)
        .into_iter()
        .filter(|spec| keep(spec.group))
        .collect();

    // Targets are independent of each other; measure in parallel and the slowest one determines total time
    let measured: Vec<(u64, usize, usize)> =
        specs.par_iter().map(|spec| measure(&spec.path)).collect();

    let mut skipped = 0usize;
    let mut entries: Vec<DevCacheEntry> = specs
        .iter()
        .zip(measured)
        .map(|(spec, (size, file_count, skip))| {
            skipped += skip;
            let exists = spec.path.is_dir();
            DevCacheEntry {
                id: spec.id.to_string(),
                name: spec.name.to_string(),
                path: display_path(&spec.path),
                size,
                file_count,
                risk: spec.risk,
                note: spec.note.to_string(),
                exists,
                // Relocated directories (junction / symlink) must be labeled with their real location:
                // otherwise `.workbuddy` is clearly on drive D but the UI says `C:\Users\...\.workbuddy`
                link: crate::scanner::reparse::analyze(&spec.path),
            }
        })
        .collect();

    // If a directory does not exist, do not show the row at all: a 0 B row just means "that tool is not installed",
    // and sitting in the list it can neither be checked nor serves a purpose, only making users think the scan missed something.
    // (Read-only entries are the same — "there is something here but it should not be touched" only holds when it really exists)
    entries.retain(|entry| entry.exists);

    let mut groups = build_groups(&specs, entries);
    // Sort by size descending both within and across groups: the first thing seen is the block that saves the most
    for group in &mut groups {
        group.entries.sort_by(|a, b| b.size.cmp(&a.size));
    }
    groups.sort_by(|a, b| b.total_size.cmp(&a.total_size));

    DevCacheScanResult {
        total_size: groups.iter().map(|g| g.total_size).sum(),
        total_files: groups.iter().map(|g| g.file_count).sum(),
        safe_size: groups
            .iter()
            .flat_map(|g| g.entries.iter())
            .filter(|entry| entry.risk == DevRisk::Cache)
            .map(|entry| entry.size)
            .sum(),
        groups,
        elapsed_ms: started.elapsed().as_millis() as u64,
        skipped,
    }
}

/// Full scan of **multi-version leftovers only** (the same software keeping several version directories side by side).
///
/// Group order is by reclaimable size descending rather than by the static target order: unlike caches,
/// most machines only have one or two such groups, and what the user cares about is which one frees the most.
pub fn scan_version_leftovers() -> VersionLeftoverScanResult {
    let started = Instant::now();
    let env = Env::detect();

    let mut skipped = 0usize;
    let mut groups: Vec<VersionGroup> = Vec::new();
    for spec in targets::version_specs(&env) {
        if let Some((group, group_skipped)) = scan_one_version_group(&spec) {
            skipped += group_skipped;
            groups.push(group);
        }
    }
    groups.sort_by(|a, b| b.reclaimable_size.cmp(&a.reclaimable_size));

    VersionLeftoverScanResult {
        total_size: groups.iter().map(|g| g.total_size).sum(),
        total_files: groups
            .iter()
            .flat_map(|g| g.entries.iter())
            .map(|entry| entry.file_count)
            .sum(),
        reclaimable_size: groups.iter().map(|g| g.reclaimable_size).sum(),
        groups,
        elapsed_ms: started.elapsed().as_millis() as u64,
        skipped,
    }
}

/// Group by `spec.group`; group order follows the appearance order in targets.rs to avoid jumping around on each refresh.
///
/// Group names stay here rather than in targets.rs: they are display text for grouping only, and the "other" group
/// keeps a Chinese fallback name in the same table — the frontend looks the section title up from i18n.
fn build_groups(specs: &[targets::TargetSpec], entries: Vec<DevCacheEntry>) -> Vec<DevCacheGroup> {
    let group_names: &[(&str, &str)] = &[
        ("pkg", "包管理器缓存"),
        ("build", "构建与索引缓存"),
        ("logs", "日志与追踪"),
        ("confirm", "需确认的项目"),
        (targets::OTHER_GROUP, "其它软件缓存"),
        ("readonly", "只展示 · 请勿删除"),
    ];

    let mut order: Vec<&str> = Vec::new();
    for spec in specs {
        if !order.contains(&spec.group) {
            order.push(spec.group);
        }
    }

    order
        .into_iter()
        .filter_map(|group_id| {
            let group_entries: Vec<DevCacheEntry> = entries
                .iter()
                .filter(|entry| {
                    specs
                        .iter()
                        .any(|spec| spec.id == entry.id && spec.group == group_id)
                })
                .cloned()
                .collect();
            if group_entries.is_empty() {
                return None;
            }
            let name = group_names
                .iter()
                .find(|(id, _)| *id == group_id)
                .map(|(_, name)| name.to_string())
                .unwrap_or_else(|| group_id.to_string());
            Some(DevCacheGroup {
                group_id: group_id.to_string(),
                group_name: name,
                total_size: group_entries.iter().map(|entry| entry.size).sum(),
                file_count: group_entries.iter().map(|entry| entry.file_count).sum(),
                entries: group_entries,
            })
        })
        .collect()
}

/// List "version-number subdirectories" under the version root, descending by version number (current version is usually first).
///
/// Scanning and deletion **share this one implementation**: previously each side wrote its own, so the same bug comparing
/// "directory name vs parsed version number" was made twice, fixing one and missing the other.
pub(super) fn list_versions(spec: &VersionSpec) -> Vec<(String, PathBuf)> {
    let Ok(reader) = std::fs::read_dir(long_path(&spec.base)) else {
        return Vec::new();
    };
    let mut versions: Vec<(String, PathBuf)> = Vec::new();
    for entry in reader.filter_map(|item| item.ok()) {
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        // Only recognize version-number directories: `Optimized` (hard-link aggregation), `*.deleting` (half-uninstalled leftovers)
        // and other non-version directories are skipped entirely — deleting them has unpredictable consequences
        if parse_version(&name, &spec.pattern).is_none() {
            continue;
        }
        versions.push((name, entry.path()));
    }
    versions.sort_by(|a, b| compare_version(&b.0, &a.0, &spec.pattern));
    versions
}

/// Determine the name of the currently used version directory, and **the basis for that decision** (shown to users).
///
/// Prefer the registry; only fall back to "highest version number" when unreadable, and say clearly in the reason that it is an inference.
/// If the registry has a value but the directory does not exist (just upgraded or rolled back), say so too — do not silently treat it as an inference.
pub(super) fn resolve_current(
    spec: &VersionSpec,
    versions: &[(String, PathBuf)],
) -> (Option<String>, String) {
    let fallback = versions.first().map(|(name, _)| name.clone());

    let Some(probed) = spec.current_probe.as_ref().and_then(probe_current_version) else {
        return (fallback, "未读到注册表，按版本号最高推断".to_string());
    };

    // The registry stores a pure version number (`3.6.11`) while a directory name may be `app-3.6.11`,
    // so we must compare **parsed version numbers**; comparing raw directory names never matches
    let matched = versions
        .iter()
        .find(|(name, _)| {
            parse_version(name, &spec.pattern)
                .map(|parsed| parsed.eq_ignore_ascii_case(&probed))
                .unwrap_or(false)
        })
        .map(|(name, _)| name.clone());

    match matched {
        Some(name) => (Some(name), "注册表记录的当前版本".to_string()),
        None => (
            fallback,
            format!("注册表记录 {probed}，但该版本目录不存在，按现存最高版本推断"),
        ),
    }
}

/// Scan one product's version directory set. Returns `None` when there is nothing to clean
/// (nonexistent root / fewer than two versions), plus the number of directories skipped while measuring.
fn scan_one_version_group(spec: &VersionSpec) -> Option<(VersionGroup, usize)> {
    let versions = list_versions(spec);
    if versions.len() < 2 {
        return None;
    }

    let (current, current_reason) = resolve_current(spec, &versions);

    let measured: Vec<(u64, usize, usize)> =
        versions.par_iter().map(|(_, path)| measure(path)).collect();
    let skipped: usize = measured.iter().map(|(_, _, skip)| *skip).sum();

    let entries: Vec<VersionEntry> = versions
        .iter()
        .zip(measured)
        .map(|((name, path), (size, file_count, _))| {
            let is_current = current.as_deref() == Some(name.as_str());
            VersionEntry {
                version: parse_version(name, &spec.pattern).unwrap_or_else(|| name.clone()),
                path: display_path(path),
                size,
                file_count,
                is_current,
                // Attach the basis only to the current-version item: old versions have no "why it is not current" to explain
                current_reason: if is_current {
                    current_reason.clone()
                } else {
                    String::new()
                },
            }
        })
        .collect();

    let total_size = entries.iter().map(|entry| entry.size).sum();
    let reclaimable_size = entries
        .iter()
        .filter(|entry| !entry.is_current)
        .map(|entry| entry.size)
        .sum();

    Some((
        VersionGroup {
            id: spec.id.to_string(),
            name: spec.name.to_string(),
            base_path: display_path(&spec.base),
            total_size,
            reclaimable_size,
            entries,
        },
        skipped,
    ))
}

/// Directory name → version string. Return None if it cannot be parsed.
///
/// The two naming forms have different segment counts, so segment count must be constrained per pattern:
/// - `FourPart` (Edge / WebView / Copilot): the whole string is a four-part version number, **no prefix allowed** —
///   loosening it to "any number of parts" would treat half directories like `157.0.4341`, or even `1.2`, as version directories;
/// - `AppPrefix` (GitHub Desktop): the "prefix + three parts" form like `app-3.6.11` **must have the prefix**,
///   otherwise a sibling like `packages` that happens to look like a version number would be misrecognized.
pub fn parse_version(name: &str, pattern: &VersionPattern) -> Option<String> {
    let candidate = match pattern {
        VersionPattern::FourPart => {
            if version_segments(name).len() != 4 {
                return None;
            }
            name
        }
        VersionPattern::AppPrefix => {
            // Windows directory names are case-insensitive, so `App-3.6.11` must be recognized too;
            // the prefix is pure ASCII, so slicing by byte will not cut into a multibyte character
            if name.len() < 4 || !name[..4].eq_ignore_ascii_case("app-") {
                return None;
            }
            let rest = &name[4..];
            let count = version_segments(rest).len();
            if !(3..=4).contains(&count) {
                return None;
            }
            rest
        }
    };
    Some(candidate.to_string())
}

/// Split on `.`; return the segments only if all are pure digits, otherwise return an empty Vec (meaning "not a version number").
fn version_segments(value: &str) -> Vec<&str> {
    let parts: Vec<&str> = value.split('.').collect();
    if parts
        .iter()
        .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
    {
        parts
    } else {
        Vec::new()
    }
}

fn version_tuple(value: &str, pattern: &VersionPattern) -> Vec<u64> {
    let candidate = match pattern {
        VersionPattern::FourPart => value,
        VersionPattern::AppPrefix => value.strip_prefix("app-").unwrap_or(value),
    };
    candidate
        .split('.')
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

fn compare_version(a: &str, b: &str, pattern: &VersionPattern) -> std::cmp::Ordering {
    version_tuple(a, pattern).cmp(&version_tuple(b, pattern))
}

/// Whether a directory name has the shape of a "version directory". Deletion validation reuses the same check,
/// ensuring "scan can find it" and "delete can remove it" use the same rule.
pub fn has_version_form(name: &str, pattern: &VersionPattern) -> bool {
    parse_version(name, pattern).is_some()
}

/// Read the "current version number" from the registry.
///
/// The same key may be under the 64-bit view, the 32-bit view (WOW6432Node), or an explicitly written
/// path containing `WOW6432Node` — the Edge family has hit this: the directory clearly existed but was unreadable because only one combination was tried,
/// needlessly degrading to "infer by highest version number". Try all combinations here and accept whichever reads successfully.
fn probe_current_version(probe: &RegProbe) -> Option<String> {
    let hive = match probe.hive {
        ProbeHive::Hklm => HKEY_LOCAL_MACHINE,
    };
    let base = probe.key.replace("SOFTWARE\\WOW6432Node\\", "SOFTWARE\\");

    // Try the 32-bit view first (Edge's EdgeUpdate key is usually registered there), then the 64-bit view
    let attempts: [(&str, u32); 3] = [
        (base.as_str(), KEY_WOW64_32KEY),
        (base.as_str(), KEY_WOW64_64KEY),
        (probe.key, KEY_WOW64_64KEY),
    ];

    for (key_path, view) in attempts {
        let Ok(key) = RegKey::predef(hive).open_subkey_with_flags(key_path, KEY_READ | view) else {
            continue;
        };
        let Ok(value) = key.get_value::<String, _>(probe.value) else {
            continue;
        };
        let trimmed = value.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    None
}
