// ============================================================================
// Path guard - the single entry point for deletion-path safety validation
//
// C1 audit fix. The three delete engines previously relied on raw string-prefix
// matching (`to_lowercase()` + `starts_with`), which can be bypassed by three
// spellings of the very same path:
//
// 1. Relative components: `C:\Temp\..\Windows\System32\config` passes a prefix
//    check on the literal string, but `remove_dir_all` resolves `..` and deletes
//    the resolved target — a completely different directory than the one validated.
//    → reject any path containing `.` / `..` components before doing anything.
// 2. Mixed separators: `C:/Windows/System32/foo.dll` matches none of the
//    `\`-based protected prefixes and sails through. → unify separators to `\`
//    before any comparison.
// 3. Extended-length prefix: `\\?\C:\Windows\...` keeps the `\\?\` prefix in the
//    string, so every `\`-based prefix check misses. → strip the prefix first.
//
// The validation template is `scanner/leftovers.rs::is_safe_leftover_path_under`
// (component-wise comparison + relative-component rejection), promoted here so
// every delete engine shares one implementation instead of its own.
// ============================================================================

use std::path::{Component, Path};

/// Whether the path contains `.` or `..` components.
///
/// On Windows, `Path::components()` already treats `/` as a separator, so
/// traversal spelled with forward slashes (`C:/a/../b`) is detected as well.
/// The `\\?\` prefix does not hide it: components still yield `ParentDir`.
pub fn has_relative_components(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
}

/// Validate a user-supplied path before any deletion is attempted.
///
/// Returns `Err` when the path is empty or contains relative components, so a
/// tampered frontend cannot smuggle a `..` traversal past the engines' own
/// prefix checks. Call this as the first step of every delete entry point.
pub fn validate_deletable(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Err("路径为空，已拒绝删除".to_string());
    }
    if has_relative_components(path) {
        return Err(format!(
            "路径包含相对组件（. 或 ..），已拒绝删除: {}",
            path.to_string_lossy()
        ));
    }
    Ok(())
}

/// Normalize a path string for safety comparison:
/// strip the `\\?\` extended-length prefix, then unify `/` to `\`.
///
/// The result is NOT lowercased on purpose — each call site lowercases with its
/// own policy (`to_lowercase` vs `to_ascii_lowercase`). Normalization must come
/// before matching, otherwise bypass spellings 2 and 3 above defeat the checks.
pub fn normalize(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let stripped = raw.strip_prefix(r"\\?\").unwrap_or(&raw);
    stripped.replace('/', "\\")
}

/// The canonical lowercase comparison form shared by every delete engine.
///
/// M13 audit fix: the engines each spelled "normalize then lowercase" slightly
/// differently (`to_lowercase` vs `to_ascii_lowercase`, some with their own
/// separator replace) — eight copies of the same rule with different quality.
/// All of them now call this one function. Lowercasing is ASCII-only, which is
/// the correct policy for Windows path matching: the protected/scope constants
/// are pure ASCII, and Unicode lowercasing has no effect on them.
pub fn compare_key(path: &Path) -> String {
    normalize(path).to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_relative_components_in_every_spelling() {
        assert!(has_relative_components(Path::new(
            r"C:\Temp\..\Windows\System32\config"
        )));
        // Forward slashes are separators on Windows, so traversal via `/` is caught too
        assert!(has_relative_components(Path::new(
            "C:/Temp/../Windows/System32"
        )));
        assert!(has_relative_components(Path::new(r"\\?\C:\Windows\..")));
        // Interior `.` components are normalized away by Path::components() and are
        // harmless (they resolve to the same path), so only `..` needs detection here.
        assert!(has_relative_components(Path::new(r"Temp\..\file.tmp")));
        assert!(has_relative_components(Path::new("..")));

        assert!(!has_relative_components(Path::new(
            r"C:\Windows\System32\foo.dll"
        )));
        assert!(!has_relative_components(Path::new(r"C:\Temp\file..tmp")));
        // `.` inside a file name is part of the name, not a CurDir component
        assert!(!has_relative_components(Path::new(r"C:\a.b\c")));
    }

    #[test]
    fn validate_rejects_traversal_and_empty_paths() {
        assert!(validate_deletable(Path::new(r"C:\Temp\..\Windows\config")).is_err());
        assert!(validate_deletable(Path::new("")).is_err());
        assert!(validate_deletable(Path::new(r"C:\Temp\cache.bin")).is_ok());
    }

    #[test]
    fn normalize_strips_extended_prefix_and_unifies_separators() {
        assert_eq!(
            normalize(Path::new(r"\\?\C:\Windows\System32\foo.dll")),
            r"C:\Windows\System32\foo.dll"
        );
        assert_eq!(
            normalize(Path::new("C:/Windows/System32/foo.dll")),
            r"C:\Windows\System32\foo.dll"
        );
        assert_eq!(
            normalize(Path::new(r"C:\Windows\System32\foo.dll")),
            r"C:\Windows\System32\foo.dll"
        );
        assert_eq!(normalize(Path::new("C:/")), r"C:\");
    }
}
