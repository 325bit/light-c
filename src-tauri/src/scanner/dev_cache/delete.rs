//! Deletion and path validation.
//!
//! Deletion is irreversible, so paths from the frontend are **never trusted** and are all revalidated here:
//! ① no `.` / `..` components ② must exactly equal a target directory (cache entry),
//! or be **under a version root with a version-number directory name** (version entry)
//! ③ read-only entries and any version root itself are always rejected.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::measure::{display_path, measure};
use super::targets::{self, Env, VersionPattern};
use super::types::{DevCacheDeleteFailure, DevCacheDeleteResult, DevRisk};

/// Normalize: strip extension prefixes, convert forward slashes to backslashes, remove trailing separator, lowercase.
/// Used for string-level equality checks, avoiding misses caused by case and separator differences.
fn norm(raw: &str) -> String {
    let mut text = display_path(Path::new(raw));
    while text.ends_with('\\') && text.len() > 3 {
        text.pop();
    }
    text.to_ascii_lowercase()
}

pub fn delete(paths: &[String]) -> DevCacheDeleteResult {
    let env = Env::detect();
    let specs = targets::targets(&env);

    // Deletable set: only Cache / Confirm. Readonly never gets in.
    let mut deletable: HashSet<String> = HashSet::new();
    // Keep the read-only set separate so a match gives "why it cannot be deleted" instead of a generic "out of scope"
    let mut readonly: HashSet<String> = HashSet::new();
    for spec in &specs {
        let key = norm(&display_path(&spec.path));
        match spec.risk {
            DevRisk::Readonly => {
                readonly.insert(key);
            }
            _ => {
                deletable.insert(key);
            }
        }
    }

    // Version entries: allowed but must strictly be "a version-number directory under a version root", and not the current version
    let mut version_bases: Vec<(String, VersionPattern)> = Vec::new();
    let mut protected_versions: HashSet<String> = HashSet::new();
    for spec in targets::version_specs(&env) {
        version_bases.push((norm(&display_path(&spec.base)), spec.pattern));
        if let Some(current) = current_version_path(&spec) {
            protected_versions.insert(norm(&current));
        }
    }

    let mut deleted = 0usize;
    let mut freed = 0u64;
    let mut failed = Vec::new();

    for raw in paths {
        let path = PathBuf::from(raw);

        if crate::path_guard::has_relative_components(&path) {
            failed.push(DevCacheDeleteFailure {
                path: raw.clone(),
                reason: "路径包含 . 或 .. 组件".to_string(),
            });
            continue;
        }

        let key = norm(raw);

        if readonly.contains(&key) {
            failed.push(DevCacheDeleteFailure {
                path: raw.clone(),
                reason: "该项标记为只展示，不提供删除".to_string(),
            });
            continue;
        }

        let is_target = deletable.contains(&key);
        let is_version = if is_target {
            false
        } else {
            match_version_entry(&key, &version_bases)
        };

        if !is_target && !is_version {
            failed.push(DevCacheDeleteFailure {
                path: raw.clone(),
                reason: "不在可清理范围内".to_string(),
            });
            continue;
        }
        if is_version && protected_versions.contains(&key) {
            failed.push(DevCacheDeleteFailure {
                path: raw.clone(),
                reason: "这是当前正在使用的版本".to_string(),
            });
            continue;
        }

        if !path.exists() {
            failed.push(DevCacheDeleteFailure {
                path: raw.clone(),
                reason: "目录不存在".to_string(),
            });
            continue;
        }

        // Measure the size before deleting; after deletion it cannot be measured
        let (before, _, _) = measure(&path);
        match std::fs::remove_dir_all(super::measure::long_path(&path)) {
            Ok(()) => {
                deleted += 1;
                freed += before;
                log::info!("已删除开发空间目录: {raw}（{} 字节）", before);
            }
            Err(error) => {
                // A directory being in use is common (IDE / build processes still open); log it and continue with the other entries
                log::warn!("删除失败 {raw}: {error}");
                failed.push(DevCacheDeleteFailure {
                    path: raw.clone(),
                    reason: error.to_string(),
                });
            }
        }
    }

    DevCacheDeleteResult {
        deleted,
        freed,
        failed,
    }
}

/// Determine whether a path has the form "version root / version-number directory".
fn match_version_entry(key: &str, bases: &[(String, VersionPattern)]) -> bool {
    for (base, pattern) in bases {
        let Some(rest) = key.strip_prefix(base.as_str()) else {
            continue;
        };
        // Must start with base\, not be base itself (that is the parent directory and must never be deleted)
        let Some(name) = rest.strip_prefix('\\') else {
            continue;
        };
        // Only one level allowed: `base\1.2.3.4`, anything deeper is not recognized
        if name.is_empty() || name.contains('\\') {
            continue;
        }
        if super::measure::has_version_form(name, pattern) {
            return true;
        }
    }
    false
}

/// Read the currently active version directory of a group (absolute path).
///
/// Recomputed at deletion time rather than reusing the scan result — the user may have updated the software between the two operations.
/// Directory listing and current-version determination logic is **fully shared** with the scan side, not rewritten here.
fn current_version_path(spec: &targets::VersionSpec) -> Option<String> {
    let versions = super::measure::list_versions(spec);
    if versions.is_empty() {
        return None;
    }
    let (name, _) = super::measure::resolve_current(spec, &versions);
    let name = name?;
    versions
        .iter()
        .find(|(candidate, _)| candidate == &name)
        .map(|(_, path)| display_path(path))
}
