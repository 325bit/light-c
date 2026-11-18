//! Whitelist boundaries and abnormal inputs.

use super::super::AiToolScanner;
use std::fs;
use std::path::{Path, PathBuf};

/// Same value retrieval as `AiToolScanner::new`, for path-validation test cases to build real directories.
fn user_profile() -> PathBuf {
    PathBuf::from(std::env::var("USERPROFILE").unwrap_or_else(|_| r"C:\Users\Default".to_string()))
}

fn appdata() -> PathBuf {
    std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| user_profile().join("AppData").join("Roaming"))
}

/// Directories outside the whitelist must be ignored — this is the security bottom line:
/// the cost of deleting the wrong session history or credentials is far greater than saving a few extra GB.
#[test]
fn only_whitelisted_directories_are_collected() {
    let base = std::env::temp_dir().join("lightc-ai-clean-test");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("Cache")).unwrap();
    fs::create_dir_all(base.join("sessions")).unwrap();
    // Chromium's new shader cache: added to the whitelist this round, pinned here too
    fs::create_dir_all(base.join("ShaderCache")).unwrap();
    fs::write(base.join("Cache").join("a.bin"), vec![0u8; 1024]).unwrap();
    fs::write(base.join("sessions").join("chat.db"), vec![0u8; 4096]).unwrap();
    fs::write(base.join("ShaderCache").join("s.bin"), vec![0u8; 256]).unwrap();

    let scanner = AiToolScanner::new();
    let mut entries = Vec::new();
    scanner.collect_cleanable_children(&base, &[], &mut entries);

    assert_eq!(entries.len(), 2, "只应命中 Cache 与 ShaderCache");
    let cache = entries.iter().find(|entry| entry.name == "Cache").unwrap();
    assert_eq!(cache.file_count, 1);
    assert_eq!(cache.size, 1024);

    let _ = fs::remove_dir_all(&base);
}

/// A tool's private extra directory names (Codex's `.tmp`) apply only under that tool.
#[test]
fn extra_names_only_apply_when_requested() {
    let base = std::env::temp_dir().join("lightc-ai-clean-extra");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join(".tmp")).unwrap();
    fs::write(base.join(".tmp").join("a.bin"), vec![0u8; 128]).unwrap();

    let scanner = AiToolScanner::new();

    // No extra list given: `.tmp` doesn't count as cache (in other tools it may be an important staging area)
    let mut without = Vec::new();
    scanner.collect_cleanable_children(&base, &[], &mut without);
    assert!(without.is_empty(), "`.tmp` 不该进全局白名单");

    // Extra list given: then it counts
    let mut with = Vec::new();
    scanner.collect_cleanable_children(&base, &[".tmp"], &mut with);
    assert_eq!(with.len(), 1);
    assert_eq!(with[0].name, ".tmp");

    let _ = fs::remove_dir_all(&base);
}

/// A non-existent directory must not panic, nor have side effects other than returning empty.
#[test]
fn missing_root_is_ignored() {
    let scanner = AiToolScanner::new();
    let mut entries = Vec::new();
    scanner.collect_cleanable_children(Path::new(r"Z:\definitely-not-exists"), &[], &mut entries);
    assert!(entries.is_empty());
}

/// A `cache` inside an extension/plugin/dependency directory must not be treated as cleanable cache.
///
/// This isn't a hypothetical: VS Code-family clients have extensions with a nested structure like
/// `extensions/<extension>/node_modules/<package>/cache`,
/// which a directory-name match would hit exactly — deleting the whole extension dependency would break it.
#[test]
fn cache_inside_extension_or_plugin_dirs_is_skipped() {
    let base = std::env::temp_dir().join("lightc-ai-clean-skip");
    let _ = fs::remove_dir_all(&base);

    // What should really be cleaned: Cache at the root
    fs::create_dir_all(base.join("Cache")).unwrap();
    fs::write(base.join("Cache").join("a.bin"), vec![0u8; 512]).unwrap();

    // The three nested cases that shouldn't be cleaned: extension dependency, plugin, git repository
    for path in [
        base.join("extensions")
            .join("foo.pkg")
            .join("node_modules")
            .join("bar")
            .join("cache"),
        base.join("plugins").join("cache"),
        base.join(".git").join("logs"),
    ] {
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("x.bin"), vec![0u8; 4096]).unwrap();
    }

    let scanner = AiToolScanner::new();
    let mut entries = Vec::new();
    scanner.collect_cleanable_children(&base, &[], &mut entries);

    assert_eq!(entries.len(), 1, "只有根目录下的 Cache 应该被收集");
    assert_eq!(entries[0].name, "Cache");

    let _ = fs::remove_dir_all(&base);
}

/// Paths containing `..` must be rejected.
///
/// `Path::starts_with` is a lexical comparison; `...\Cursor\Cache\..\..\..\logs` would be judged
/// as under the root directory, while `remove_dir_all` acts on the resolved real path —
/// without this check, the prefix validation would be useless.
#[test]
fn path_traversal_is_rejected() {
    let scanner = AiToolScanner::new();
    let root = appdata().join("Cursor");

    // Normal case: a cache directory under the root is managed
    assert!(scanner.is_managed_cache_dir(&root.join("Cache")));

    // Use `..` to escape the root, while the name still masquerades as logs in the whitelist
    let escaped = root
        .join("Cache")
        .join("..")
        .join("..")
        .join("..")
        .join("logs");
    assert!(!scanner.is_managed_cache_dir(&escaped), "带 `..` 的路径必须拒绝");

    // The root directory itself must never be deleted
    assert!(!scanner.is_managed_cache_dir(&root));
}

/// Paths with inconsistent case must still be recognized.
///
/// Scanning gets the real directory name from disk, while the manifest has fixed literals; when they differ,
/// `Path::starts_with` would silently reject the deletion (UI just reports 0), so comparison must ignore case.
#[test]
fn root_match_ignores_case() {
    let scanner = AiToolScanner::new();
    // The manifest writes "Cursor", here we simulate "CURSOR" on disk
    assert!(scanner.is_managed_cache_dir(&appdata().join("CURSOR").join("Cache")));
    // Same verification for a dot-directory in the user directory
    assert!(scanner.is_managed_cache_dir(&user_profile().join(".CODEX").join("cache")));
}

/// Non-managed paths must be rejected and counted in the failure list.
///
/// The UI relies on this list to say "not deleted", so rejection must go through the same return channel,
/// not just be logged and treated as success.
#[test]
fn delete_reports_unmanaged_paths_as_failures() {
    let scanner = AiToolScanner::new();
    // The name is in the whitelist (logs), but not under any tool's root directory
    let target = std::env::temp_dir().join("logs");
    let result = scanner.delete_dirs(&[target.to_string_lossy().to_string()]);

    assert_eq!(result.deleted, 0);
    assert_eq!(result.failed.len(), 1);
    assert_eq!(result.failed[0].reason, "不在可清理范围内");
}

/// Codex's `.tmp` counts as cache only under Codex.
#[test]
fn extra_names_are_scoped_to_their_tool() {
    let scanner = AiToolScanner::new();
    assert!(scanner.is_managed_cache_dir(&user_profile().join(".codex").join(".tmp")));
    assert!(!scanner.is_managed_cache_dir(&appdata().join("devin").join(".tmp")));
}

/// Doubao's directory structure: cache is buried in `User Data\Default`, handled differently at runtime.
///
/// This case pins three easy-to-get-wrong points:
/// 1. Multi-level nesting (`Doubao\User Data\Default\Cache`) must be scannable, otherwise Doubao shows 0;
/// 2. `sandbox_runtime\downloads` is a cleanable directory **authorized by this tool** (runtime installer package, 234MB locally),
///    but it isn't in the global whitelist — when another tool uses this name it may be the user's downloads directory;
/// 3. `sandbox_runtime\bases` (runtime in use) and `Default\sandbox_envs_dir` (session environment)
///    would break functionality if deleted, so they must never enter the list.
#[test]
fn doubao_layout_separates_cache_from_sandbox_runtime() {
    let base = std::env::temp_dir().join("lightc-ai-clean-doubao");
    let _ = fs::remove_dir_all(&base);
    let user_data = base.join("User Data");
    let default = user_data.join("Default");

    fs::create_dir_all(default.join("Cache")).unwrap();
    fs::create_dir_all(default.join("GPUCache")).unwrap();
    fs::create_dir_all(default.join("Cache").join("nested")).unwrap();
    fs::create_dir_all(user_data.join("sandbox_runtime").join("bases").join("hash")).unwrap();
    fs::create_dir_all(user_data.join("sandbox_runtime").join("downloads")).unwrap();
    fs::create_dir_all(default.join("sandbox_envs_dir").join("envs")).unwrap();
    fs::create_dir_all(user_data.join("gecko_cache")).unwrap();

    fs::write(default.join("Cache").join("a.bin"), vec![0u8; 2048]).unwrap();
    fs::write(default.join("GPUCache").join("g.bin"), vec![0u8; 512]).unwrap();
    fs::write(
        user_data.join("sandbox_runtime").join("downloads").join("runtime.tar"),
        vec![0u8; 4096],
    )
    .unwrap();
    fs::write(user_data.join("gecko_cache").join("gecko2.db"), vec![0u8; 1024]).unwrap();

    let scanner = AiToolScanner::new();

    // ① Doubao's own list: downloads counts, bases / sandbox_envs_dir / gecko_cache don't
    let mut entries = Vec::new();
    scanner.collect_cleanable_children(&base, &["downloads"], &mut entries);

    let mut names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, vec!["Cache", "GPUCache", "downloads"]);
    assert_eq!(
        entries.iter().map(|entry| entry.size).sum::<u64>(),
        2048 + 512 + 4096,
        "bases / sandbox_envs_dir / gecko_cache 不计入体积"
    );

    // ② No extra list (other tools): downloads doesn't count. Prevent someone from slipping it into the global whitelist
    let mut without = Vec::new();
    scanner.collect_cleanable_children(&base, &[], &mut without);
    assert!(
        without.iter().all(|entry| entry.name != "downloads"),
        "downloads 只能按工具授权，不能进全局白名单"
    );

    let _ = fs::remove_dir_all(&base);
}
