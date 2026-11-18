//! Dev space cleanup (light-c version of cscan's capabilities).
//!
//! Role: fill the gap in space coverage that light-c's existing modules **do not cover** —
//! dev toolchain caches (NuGet / Cargo / Gradle / Go / JetBrains…),
//! multiple old-version directories of the same software (like Edge's four EdgeCore sets),
//! and other app caches that cscan can find but junk cleanup and driver cleanup both ignore.
//!
//! The three halves are scanned by three separate commands, because they do not share a page:
//! - `scan_dev_cache` → toolchain caches, shown on "dev cache cleanup";
//! - `scan_version_leftovers` → multi-version directories, shown on "leftover & cache";
//! - `scan_other_app_caches` → other software caches, also shown on "leftover & cache".
//! All three delete through the same `delete_dev_cache_paths` validation.
//!
//! Division of labor with existing modules (no double counting):
//! - Browser / temp files / recycle bin / thumbnails / update caches → `junk_clean`, `leftovers`
//! - Installed driver binaries → `driver_cleanup` (this module handles only **downloaded driver installers**)
//! - WeChat / QQ and other chat data → `social_scanner`
//! - Big files and directory usage distribution → `big_files`, `hotspot`
//!
//! All paths are resolved from environment variables at runtime; the source contains no personal directory literals.

mod delete;
mod measure;
mod targets;
mod types;

// Export names carry a module prefix: `scanner/mod.rs` `pub use`s each submodule into the same level,
// naming them plainly `scan` / `delete` would collide with same-named symbols elsewhere.
pub use delete::delete as delete_dev_cache_paths;
pub use measure::{scan as scan_dev_cache, scan_other_app_caches, scan_version_leftovers};
// Only the result types are needed across modules (the commands layer); other structs can just reference each other within the submodule;
// re-exporting all of them would trigger unused_imports warnings.
pub use types::{DevCacheDeleteResult, DevCacheScanResult, VersionLeftoverScanResult};

#[cfg(test)]
mod tests {
    use super::measure::{display_path, has_version_form, long_path};
    use super::targets::VersionPattern;
    use super::types::DevCacheScanResult;
    use std::path::Path;

    /// Print one cache-scan result; shared by the smoke test so the two halves print in the same shape.
    fn print_cache_scan(label: &str, result: &DevCacheScanResult) {
        println!(
            "{label} 总计 {}（{} 个文件）/ 可直接清理 {} / 耗时 {}ms / 跳过 {}",
            result.total_size,
            result.total_files,
            result.safe_size,
            result.elapsed_ms,
            result.skipped
        );
        for group in &result.groups {
            println!(
                "[{}] {}  {}  {} 项",
                group.group_id,
                group.group_name,
                group.total_size,
                group.entries.len()
            );
            for entry in &group.entries {
                let link_note = match &entry.link {
                    Some(link) => format!(
                        "  ⤴ {:?} {} → {}（实体 {}）",
                        link.kind, link.link_path, link.target, link.resolved
                    ),
                    None => String::new(),
                };
                println!(
                    "    {:?} {:>12}  {}  {}{}",
                    entry.risk, entry.size, entry.name, entry.path, link_note
                );
            }
        }
    }

    /// Edge / WebView / Copilot use four-part version numbers; something like `Optimized` must never be treated as a version directory —
    /// it could be a hard-link aggregation directory, with unpredictable consequences if deleted.
    #[test]
    fn four_part_versions() {
        assert!(has_version_form("157.0.4341.60", &VersionPattern::FourPart));
        assert!(has_version_form("155.0.4283.62", &VersionPattern::FourPart));
        assert!(!has_version_form("Optimized", &VersionPattern::FourPart));
        assert!(!has_version_form("157.0.4341", &VersionPattern::FourPart));
        assert!(!has_version_form("1.2.3.4.5", &VersionPattern::FourPart));
        assert!(!has_version_form("157.0.4341.x", &VersionPattern::FourPart));
    }

    /// GitHub Desktop uses directory names like `app-3.6.11`: three parts + a required prefix.
    ///
    /// This was wrong before — the old implementation always required four parts regardless of pattern, so `app-3.6.11`
    /// was left with three parts after stripping the prefix and judged "not a version directory", so the whole GitHub Desktop group went undetected.
    #[test]
    fn github_desktop_versions() {
        assert!(has_version_form("app-3.6.11", &VersionPattern::AppPrefix));
        // Windows directory names are case-insensitive; recognize prefixes with different casing too
        assert!(has_version_form("App-3.6.11", &VersionPattern::AppPrefix));
        assert!(has_version_form("app-3.6.11.1", &VersionPattern::AppPrefix));
        // Reject anything without a prefix: sibling directories like `packages` also exist,
        // loosening to "anything that looks like a version number counts" would misjudge them
        assert!(!has_version_form("3.6.11", &VersionPattern::AppPrefix));
        // Two parts are too short; derived directories like `-unpacked` contain non-digits
        assert!(!has_version_form("app-3.6", &VersionPattern::AppPrefix));
        assert!(!has_version_form(
            "app-3.6.11-unpacked",
            &VersionPattern::AppPrefix
        ));
        assert!(!has_version_form("packages", &VersionPattern::AppPrefix));
    }

    /// `\\?\` prefix does not accept forward slashes: when callers pass `C:/Windows` it must be normalized,
    /// otherwise read_dir fails outright and the UI shows 0 B.
    #[test]
    fn path_normalization() {
        assert_eq!(
            long_path(Path::new("C:/Windows/Temp")).to_string_lossy(),
            r"\\?\C:\Windows\Temp"
        );
        assert_eq!(
            display_path(Path::new(r"\\?\C:\Windows\Temp")),
            r"C:\Windows\Temp"
        );
        // Do not add the prefix again if already present
        assert_eq!(
            long_path(Path::new(r"\\?\C:\Windows")).to_string_lossy(),
            r"\\?\C:\Windows"
        );
    }

    /// Deletion validation must block "out-of-scope" paths.
    ///
    /// This tests the **real delete function**, only fed paths that can never match —
    /// if one of the three validation layers is ever loosened, this catches it first rather than waiting for a user slip.
    #[test]
    fn delete_rejects_paths_outside_scope() {
        let result = super::delete_dev_cache_paths(&[
            r"C:\definitely-not-a-dev-cache-target".to_string(),
            r"C:\code\..\Windows\System32\drivers".to_string(),
        ]);
        assert_eq!(result.deleted, 0, "范围外路径不该删掉任何东西");
        assert_eq!(result.freed, 0);
        assert_eq!(result.failed.len(), 2);
        assert!(result
            .failed
            .iter()
            .all(|failure| !failure.reason.is_empty()));
    }

    /// Manual smoke test: run a scan against directories on a real machine and print the results.
    ///
    /// Not in the default test pool (`#[ignore]`) — it actually traverses several GB of package caches and results depend on what is installed locally.
    /// Run with: `cargo test --lib smoke_scan_real_machine -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn smoke_scan_real_machine() {
        print_cache_scan("工具缓存", &super::scan_dev_cache());
        // The "other software caches" group now has its own command; print it too so the split can be checked
        // against one run instead of two separate test invocations.
        print_cache_scan("其它软件缓存", &super::scan_other_app_caches());
        let versions = super::scan_version_leftovers();
        println!(
            "多版本残留 总计 {}（{} 个文件）/ 可回收 {} / 耗时 {}ms / 跳过 {}",
            versions.total_size,
            versions.total_files,
            versions.reclaimable_size,
            versions.elapsed_ms,
            versions.skipped
        );
        for group in &versions.groups {
            println!(
                "[ver:{}] {}  {}  可回收 {}  基目录 {}",
                group.id, group.name, group.total_size, group.reclaimable_size, group.base_path
            );
            for entry in &group.entries {
                println!(
                    "    {} {:>12} 当前={} {}",
                    entry.version, entry.size, entry.is_current, entry.current_reason
                );
            }
        }

        // Structural invariants: at most one "current version" per group, and the reclaimable amount must be less than the total
        for group in &versions.groups {
            let current = group
                .entries
                .iter()
                .filter(|entry| entry.is_current)
                .count();
            assert!(current <= 1, "{} 出现 {} 个当前版本", group.name, current);
            assert!(group.reclaimable_size <= group.total_size);
        }
    }
}
