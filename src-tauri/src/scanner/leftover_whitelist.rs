// ============================================================================
// Uninstall leftover user whitelist
// ============================================================================
//
// Whitelist entries are saved by full path and protect all children under that path. This lets users handle one
// false positive precisely, without expanding protection because app names are similar.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

const WHITELIST_FILE_NAME: &str = "leftover_whitelist.json";
const WHITELIST_SCHEMA_VERSION: u32 = 1;

/// Serialize read-modify-write operations, so consecutive clicks or concurrent commands cannot overwrite each other's whitelist content.
static WHITELIST_WRITE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeftoverWhitelistEntry {
    pub path: String,
    pub added_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct LeftoverWhitelistFile {
    #[serde(default = "default_schema_version")]
    version: u32,
    #[serde(default)]
    entries: Vec<LeftoverWhitelistEntry>,
}

fn default_schema_version() -> u32 {
    WHITELIST_SCHEMA_VERSION
}

fn whitelist_file_path() -> PathBuf {
    crate::data_dir::get_data_dir().join(WHITELIST_FILE_NAME)
}

/// Get whitelist entries; a corrupted or missing file does not block the main cleanup flow.
pub fn list_entries() -> Result<Vec<LeftoverWhitelistEntry>, String> {
    load_entries_from(&whitelist_file_path())
}

/// Add the path corresponding to a scan result. Only existing absolute paths are accepted, preventing the frontend from constructing invalid protection rules.
pub fn add_entry(path: &str) -> Result<LeftoverWhitelistEntry, String> {
    let _guard = WHITELIST_WRITE_LOCK
        .lock()
        .map_err(|_| "卸载残留白名单锁定失败".to_string())?;
    let store_path = whitelist_file_path();
    let normalized_path = normalize_existing_path(path)?;
    let mut entries = load_entries_from(&store_path)?;

    if let Some(entry) = entries
        .iter()
        .find(|entry| comparison_key(&entry.path) == comparison_key(&normalized_path))
    {
        return Ok(entry.clone());
    }

    let entry = LeftoverWhitelistEntry {
        path: normalized_path,
        added_at: Utc::now().to_rfc3339(),
    };
    entries.push(entry.clone());
    save_entries_to(&store_path, &entries)?;
    Ok(entry)
}

/// Remove an exact whitelist item; even if the path no longer exists, users are allowed to clean up stale config.
pub fn remove_entry(path: &str) -> Result<(), String> {
    let _guard = WHITELIST_WRITE_LOCK
        .lock()
        .map_err(|_| "卸载残留白名单锁定失败".to_string())?;
    let store_path = whitelist_file_path();
    let mut entries = load_entries_from(&store_path)?;
    let previous_len = entries.len();
    // Use the same comparison rules as contains_path: paths returned by the frontend may carry a `\\?\` prefix, be 8.3 short names, or differ in case,
    // comparing raw strings directly would cause "deleted in the UI but still in the file".
    let target = comparison_key(path);
    entries.retain(|entry| comparison_key(&entry.path) != target);

    if entries.len() != previous_len {
        save_entries_to(&store_path, &entries)?;
    }
    Ok(())
}

/// Path matching uses case-insensitive directory boundaries, preventing `App` from wrongly matching `AppData`.
pub fn contains_path(entries: &[LeftoverWhitelistEntry], candidate: &Path) -> bool {
    let candidate_path = comparison_key(&candidate.to_string_lossy());
    entries.iter().any(|entry| {
        let protected_path = comparison_key(&entry.path);
        candidate_path == protected_path
            || candidate_path
                .strip_prefix(&protected_path)
                .is_some_and(|suffix| suffix.starts_with('\\'))
    })
}
fn load_entries_from(store_path: &Path) -> Result<Vec<LeftoverWhitelistEntry>, String> {
    if !store_path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(store_path)
        .map_err(|error| format!("读取卸载残留白名单失败 {}: {}", store_path.display(), error))?;
    let file: LeftoverWhitelistFile = serde_json::from_str(&content)
        .map_err(|error| format!("解析卸载残留白名单失败 {}: {}", store_path.display(), error))?;
    Ok(file.entries)
}

fn save_entries_to(store_path: &Path, entries: &[LeftoverWhitelistEntry]) -> Result<(), String> {
    let parent = store_path
        .parent()
        .ok_or_else(|| "卸载残留白名单存储路径无效".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("创建卸载残留白名单目录失败 {}: {}", parent.display(), error))?;
    let file = LeftoverWhitelistFile {
        version: WHITELIST_SCHEMA_VERSION,
        entries: entries.to_vec(),
    };
    let content = serde_json::to_string_pretty(&file)
        .map_err(|error| format!("序列化卸载残留白名单失败: {}", error))?;
    fs::write(store_path, content)
        .map_err(|error| format!("保存卸载残留白名单失败 {}: {}", store_path.display(), error))
}

fn normalize_existing_path(path: &str) -> Result<String, String> {
    let input = Path::new(path);
    if !input.is_absolute() {
        return Err(format!("白名单路径必须是绝对路径: {}", path));
    }
    let canonical_path = fs::canonicalize(input)
        .map_err(|error| format!("无法确认白名单路径 {}: {}", path, error))?;
    Ok(normalize_path_for_storage(&canonical_path))
}

fn normalize_path_for_storage(path: &Path) -> String {
    trim_trailing_separator(strip_extended_length_prefix(
        &path.to_string_lossy().replace('/', "\\"),
    ))
}

fn normalize_path_for_compare(path: &Path) -> String {
    normalize_path_for_storage(path).to_ascii_lowercase()
}

fn normalize_string_path_for_compare(path: &str) -> String {
    trim_trailing_separator(strip_extended_length_prefix(&path.replace('/', "\\")))
        .to_ascii_lowercase()
}

/// Check whether a path may contain an 8.3 short name.
///
/// Short names always contain `~` (e.g. `PASSAC~1`), but long names may also legally contain `~`:
/// it is a valid filename character. A false positive only costs one extra canonicalize, never a
/// wrong match, so this check is intentionally permissive.
fn may_contain_short_name(path: &str) -> bool {
    path.contains('~')
}

/// Comparison key: use the real long path when available, otherwise fall back to normalized literal text.
///
/// Why this is needed: the write side (`normalize_existing_path`) calls `fs::canonicalize` and
/// stores the long name recorded by the filesystem, while a scanned path can come from an
/// environment variable as an 8.3 short name. On machines with long or non-ASCII user names,
/// `%TEMP%` is `C:\Users\PASSAC~1\...`. The two spellings never match, so the whitelist silently
/// stops working -- and it is the only user-controlled guard before permanent deletion.
///
/// Cost control: one canonicalize costs about 50µs in measurements; doing it for every
/// candidate would add half a second per 10,000 files and defeat this tool's speed goal.
/// Pay that cost only when `~` is present (a necessary symptom of short names); ordinary
/// long paths keep the pure string comparison, so the hot path is unchanged.
fn comparison_key(path: &str) -> String {
    if !may_contain_short_name(path) {
        return normalize_string_path_for_compare(path);
    }
    canonicalize_longest_existing_prefix(Path::new(path))
        .unwrap_or_else(|| normalize_string_path_for_compare(path))
}

/// Replace the longest existing ancestor with its real long name, then append the missing tail verbatim.
///
/// Why not canonicalize the whole path: most deletion candidates no longer exist (that is why
/// they are being cleaned), and `fs::canonicalize` fails on missing paths. The whitelist must
/// still catch "existing parent + missing child", otherwise protecting `SomeApp` would not protect
/// `SomeApp\cache` once the child disappears.
fn canonicalize_longest_existing_prefix(path: &Path) -> Option<String> {
    let mut missing_suffix: Vec<String> = Vec::new();
    let mut current = path;
    loop {
        if let Ok(canonical) = fs::canonicalize(current) {
            // Normalize the resolved ancestor the same way, then append the missing tail with the same rules
            let mut resolved = normalize_path_for_compare(&canonical);
            for part in missing_suffix.iter().rev() {
                if !resolved.ends_with('\\') {
                    resolved.push('\\');
                }
                resolved.push_str(&part.to_ascii_lowercase());
            }
            return Some(resolved);
        }
        // Walk up until an ancestor exists; if even the drive root fails, give up and let the caller fall back to literal comparison
        missing_suffix.push(current.file_name()?.to_string_lossy().to_string());
        current = current.parent()?;
    }
}

/// Strip the Windows extended-length path prefix (`\\?\` and `\\.\`).
///
/// Explanation: whitelist entries go through `fs::canonicalize`, which on Windows adds a `\\?\` prefix to absolute paths;
/// but paths in scan results have no such prefix, so string comparison never matches and the whitelist effectively fails.
/// Strip the prefix on both sides before comparing; the path itself is unchanged, we just make the two comparable.
fn strip_extended_length_prefix(path: &str) -> String {
    // UNC long path `\\?\UNC\server\share` is restored to `\\server\share`.
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{}", rest);
    }
    path.strip_prefix(r"\\?\")
        .or_else(|| path.strip_prefix(r"\\.\"))
        .map(str::to_string)
        .unwrap_or_else(|| path.to_string())
}

fn trim_trailing_separator(mut path: String) -> String {
    while path.len() > 3 && path.ends_with('\\') {
        path.pop();
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_path_protects_descendants_without_matching_similar_prefixes() {
        let entries = vec![LeftoverWhitelistEntry {
            path: r"C:\Fixture\App".to_string(),
            added_at: "2026-01-01T00:00:00Z".to_string(),
        }];

        assert!(contains_path(&entries, Path::new(r"c:\fixture\app\cache")));
        assert!(contains_path(&entries, Path::new(r"C:\Fixture\App")));
        assert!(!contains_path(&entries, Path::new(r"C:\Fixture\AppData")));
    }

    #[test]
    fn strips_extended_length_path_prefix() {
        // The \\?\ and \\.\ prefixes must be stripped, otherwise they cannot be compared with plain paths from scan results.
        assert_eq!(
            strip_extended_length_prefix(r"\\?\C:\Users\Test\AppData\Local\App"),
            r"C:\Users\Test\AppData\Local\App"
        );
        assert_eq!(
            strip_extended_length_prefix(r"\\.\C:\Users\Test"),
            r"C:\Users\Test"
        );
        // The UNC form must be restored to \\server\share, not turned into server\share.
        assert_eq!(
            strip_extended_length_prefix(r"\\?\UNC\server\share\App"),
            r"\\server\share\App"
        );
        // Plain paths are left unchanged.
        assert_eq!(
            strip_extended_length_prefix(r"C:\Users\Test"),
            r"C:\Users\Test"
        );
    }

    #[test]
    fn canonicalized_whitelist_entry_matches_scanned_path() {
        // This is a regression test for this whitelist failure: stored paths come from fs::canonicalize (which adds a \\?\ prefix on Windows),
        // while scan paths come from directory traversal (no prefix); the two must be judged the same path.
        let root = std::env::temp_dir().join(format!(
            "lightc-whitelist-prefix-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let scanned_directory = root.join("AppData").join("Local").join("SomeApp");
        fs::create_dir_all(&scanned_directory).expect("创建测试目录失败");

        let canonical = fs::canonicalize(&scanned_directory).expect("canonicalize 失败");
        let entries = vec![LeftoverWhitelistEntry {
            path: normalize_path_for_storage(&canonical),
            added_at: "2026-01-01T00:00:00Z".to_string(),
        }];

        // Paths given by the scan side have no prefix and must also hit the whitelist.
        assert!(
            contains_path(&entries, &scanned_directory),
            "带 \\\\?\\ 前缀的入库路径必须能匹配扫描路径"
        );
        assert!(contains_path(&entries, &scanned_directory.join("cache")));
        assert!(!contains_path(
            &entries,
            &root.join("AppData").join("Local").join("OtherApp")
        ));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn path_with_tilde_still_matches_its_canonical_form() {
        // A `~` in a directory name takes the canonicalize branch in comparison_key (`~` indicates an 8.3 short name
        // but is also legal in long names). Verify that both spellings still resolve to the same whitelist entry.
        let root = std::env::temp_dir().join(format!(
            "lightc~whitelist-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let protected = root.join("Some~App");
        fs::create_dir_all(&protected).expect("创建测试目录失败");

        let canonical = fs::canonicalize(&protected).expect("canonicalize 失败");
        let entries = vec![LeftoverWhitelistEntry {
            path: normalize_path_for_storage(&canonical),
            added_at: "2026-01-01T00:00:00Z".to_string(),
        }];

        // Both the literal spelling (with `~`) and the canonical spelling must match, including child paths
        assert!(
            contains_path(&entries, &protected),
            "含 ~ 的字面路径必须命中"
        );
        assert!(
            contains_path(&entries, &canonical),
            "canonical 拼写必须命中"
        );
        assert!(contains_path(&entries, &protected.join("cache")));
        assert!(!contains_path(&entries, &root.join("Other~App")));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn comparison_ignores_case_separators_and_prefix() {
        // Paths returned by the frontend may differ in case, use forward slashes, or carry a \\?\ prefix, all must be treated as the same entry,
        // otherwise you get "deleted in the UI but actually still there" or "protected but not effective".
        let stored = r"\\?\C:\Users\Test\AppData\Local\SomeApp";
        let candidates = [
            r"C:\Users\Test\AppData\Local\SomeApp",
            r"c:\users\test\appdata\local\someapp",
            r"C:/Users/Test/AppData/Local/SomeApp",
            r"C:\Users\Test\AppData\Local\SomeApp\",
        ];

        let entries = vec![LeftoverWhitelistEntry {
            path: stored.to_string(),
            added_at: "2026-01-01T00:00:00Z".to_string(),
        }];
        for candidate in candidates {
            assert!(
                contains_path(&entries, Path::new(candidate)),
                "应命中白名单: {}",
                candidate
            );
            assert_eq!(
                normalize_string_path_for_compare(stored),
                normalize_string_path_for_compare(candidate),
                "比较口径应一致: {}",
                candidate
            );
        }
    }
    #[test]
    fn whitelist_store_round_trip_preserves_entries() {
        let root = std::env::temp_dir().join(format!(
            "lightc-leftover-whitelist-test-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let store_path = root.join(WHITELIST_FILE_NAME);
        let entries = vec![LeftoverWhitelistEntry {
            path: r"C:\Fixture\App".to_string(),
            added_at: "2026-01-01T00:00:00Z".to_string(),
        }];

        save_entries_to(&store_path, &entries).expect("保存测试白名单失败");
        let loaded = load_entries_from(&store_path).expect("读取测试白名单失败");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].path, entries[0].path);

        let _ = fs::remove_dir_all(root);
    }
}
