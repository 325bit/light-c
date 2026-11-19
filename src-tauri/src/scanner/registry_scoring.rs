// ============================================================================
// Registry path utilities (Registry Path Utilities)
//
// Provides two core utilities:
//   1. PathCache    — path existence cache, avoids repeated filesystem calls
//   2. PathResolver — extracts executable paths from command-line strings, expands environment variables
//   3. is_definitely_safe_to_delete — hard filter conditions to confirm a registry entry is safe to delete
// ============================================================================

use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ============================================================================
// 1. Path existence cache (PathCache)
// ============================================================================

/// Path existence cache
///
/// Caches the result of Path::exists() in a HashMap, avoiding repeated filesystem calls.
/// Within one scan, the same path is referenced repeatedly (e.g. multiple MUI values pointing to the same exe).
pub struct PathCache {
    cache: HashMap<String, bool>,
    hits: u64,
    misses: u64,
}

impl PathCache {
    pub fn new() -> Self {
        Self {
            cache: HashMap::with_capacity(256),
            hits: 0,
            misses: 0,
        }
    }

    /// Check whether a path exists (with caching)
    pub fn exists(&mut self, path: &str) -> bool {
        if let Some(&exists) = self.cache.get(path) {
            self.hits += 1;
            return exists;
        }
        self.misses += 1;
        let exists = Path::new(path).exists();
        self.cache.insert(path.to_string(), exists);
        exists
    }

    /// Get cache statistics
    pub fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }
}

impl Default for PathCache {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 2. Path resolver (PathResolver)
// ============================================================================

/// Path resolver
///
/// Handles path extraction and environment-variable expansion in command-line strings.
pub struct PathResolver {
    env_var_re: Regex,
}

impl PathResolver {
    pub fn new() -> Self {
        Self {
            env_var_re: Regex::new(r"%([^%]+)%").unwrap(),
        }
    }

    /// Extract and resolve an executable path from a command-line string
    ///
    /// Supported formats:
    /// - `"C:\Program Files\App\app.exe" %1`
    /// - `C:\Program Files\App\app.exe %1`
    /// - `%SystemRoot%\system32\notepad.exe %1`
    /// - `"C:\Program Files\App\app.exe",-123`
    /// - `C:\PROGRA~1\App\app.exe`
    ///
    /// Returns Option<(resolved path, whether it's in a system directory)>
    pub fn extract_and_resolve(&self, raw_command: &str) -> Option<(PathBuf, bool)> {
        let exe_path = self.extract_exe_path(raw_command)?;
        let resolved = self.expand_env_vars(&exe_path);
        let path = PathBuf::from(&resolved);
        let is_system = self.is_system_path(&resolved);

        Some((path, is_system))
    }

    /// Extract an executable path from a command-line string
    fn extract_exe_path(&self, command: &str) -> Option<String> {
        let command = command.trim();

        if command.is_empty() {
            return None;
        }

        // Handle a quoted path: "C:\path\to\app.exe" args
        if command.starts_with('"') {
            if let Some(end) = command[1..].find('"') {
                let path = &command[1..end + 1];
                if self.looks_like_exe_path(path) {
                    return Some(path.to_string());
                }
            }
        }

        // Handle an unquoted path: C:\path\to\app.exe args
        let first_token = command.split_whitespace().next()?;
        let cleaned = first_token.trim_end_matches(',');

        if self.looks_like_exe_path(cleaned) {
            Some(cleaned.to_string())
        } else {
            None
        }
    }

    /// Check whether a string looks like an executable path (drive letter + exe/dll/sys extension)
    fn looks_like_exe_path(&self, s: &str) -> bool {
        let lower = s.to_ascii_lowercase();
        // Environment-variable paths have no drive letter before expansion; allow a valid %VAR% prefix here and let later resolution handle it.
        let starts_with_environment_variable = lower.starts_with('%')
            && lower[1..]
                .find('%')
                .is_some_and(|end| lower[end + 1..].contains('\\'));
        (lower.ends_with(".exe") || lower.ends_with(".dll") || lower.ends_with(".sys"))
            && lower.len() > 4
            && (lower.chars().nth(1) == Some(':') || starts_with_environment_variable)
    }

    /// Expand environment variables (e.g. %SystemRoot%)
    fn expand_env_vars(&self, s: &str) -> String {
        self.env_var_re
            .replace_all(s, |caps: &regex::Captures| {
                let var_name = &caps[1];
                std::env::var(var_name).unwrap_or_else(|_| match var_name.to_uppercase().as_str() {
                    "SYSTEMROOT" => "C:\\Windows".to_string(),
                    "WINDIR" => "C:\\Windows".to_string(),
                    "PROGRAMFILES" => "C:\\Program Files".to_string(),
                    "PROGRAMFILES(X86)" => "C:\\Program Files (x86)".to_string(),
                    "PROGRAMDATA" => "C:\\ProgramData".to_string(),
                    "USERPROFILE" => "C:\\Users\\Default".to_string(),
                    "APPDATA" => "C:\\Users\\Default\\AppData\\Roaming".to_string(),
                    "LOCALAPPDATA" => "C:\\Users\\Default\\AppData\\Local".to_string(),
                    "TEMP" | "TMP" => "C:\\Windows\\Temp".to_string(),
                    _ => caps[0].to_string(),
                })
            })
            .to_string()
    }

    /// Check whether a path is in a system directory
    pub fn is_system_path(&self, path: &str) -> bool {
        let lower = path.to_ascii_lowercase();
        let system_roots = [
            "c:\\windows\\system32",
            "c:\\windows\\syswow64",
            "c:\\windows\\system",
            "c:\\windows\\winsxs",
            "c:\\windows\\servicing",
            "c:\\windows\\assembly",
        ];

        for root in &system_roots {
            if lower.starts_with(root) {
                return true;
            }
        }

        false
    }
}

impl Default for PathResolver {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 3. Hard delete safety check (Hard Delete Safety Check)
// ============================================================================

/// Confirm that the file path associated with a registry entry meets the "ironclad conditions" — safe to delete
///
/// [Ironclad conditions]
///   1. Path is non-empty and can be extracted from the registry value
///   2. The associated file does not exist on disk (Path.exists == false)
///   3. Path is not in a system directory (Windows / System32 / SysWOW64)
///   4. File name is not a critical system process (rundll32.exe / svchost.exe)
///
/// Any uncertain case returns false — safety first.
pub fn is_definitely_safe_to_delete(extracted_path: &str, path_cache: &mut PathCache) -> bool {
    // Condition 1: path is non-empty
    if extracted_path.is_empty() {
        return false;
    }

    // Condition 2: the file must not exist (existing means the program may still be using it)
    if path_cache.exists(extracted_path) {
        return false;
    }

    // Condition 3: path must not be in a system directory
    let lower = extracted_path.to_ascii_lowercase();
    let system_dirs = [
        "\\windows\\system32",
        "\\windows\\syswow64",
        "\\windows\\system\\",
        "\\windows\\winsxs",
        "\\windows\\servicing",
        "\\windows\\assembly",
    ];
    for dir in &system_dirs {
        if lower.contains(dir) {
            return false;
        }
    }

    // Condition 4: file name must not be a critical system process
    let file_name = Path::new(extracted_path)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("");
    let file_name_lower = file_name.to_ascii_lowercase();
    let system_exes = [
        "rundll32.exe",
        "svchost.exe",
        "explorer.exe",
        "regedit.exe",
        "cmd.exe",
        "powershell.exe",
        "csrss.exe",
        "lsass.exe",
        "services.exe",
        "winlogon.exe",
        "taskmgr.exe",
        "conhost.exe",
        "smss.exe",
        "spoolsv.exe",
        "dllhost.exe",
    ];
    if system_exes.contains(&file_name_lower.as_str()) {
        return false;
    }

    true
}

// ============================================================================
// 4. Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_resolver_basic() {
        let resolver = PathResolver::new();

        let (path, _) = resolver
            .extract_and_resolve(r#""C:\Program Files\App\app.exe" %1"#)
            .unwrap();
        assert_eq!(path, PathBuf::from(r"C:\Program Files\App\app.exe"));

        let (path, _) = resolver.extract_and_resolve(r"C:\App\app.exe %1").unwrap();
        assert_eq!(path, PathBuf::from(r"C:\App\app.exe"));
    }

    #[test]
    fn test_path_resolver_env_var() {
        let resolver = PathResolver::new();
        let result = resolver.extract_and_resolve(r"%SystemRoot%\system32\notepad.exe %1");
        assert!(result.is_some());
        let (path, is_system) = result.unwrap();
        let path_str = path.to_string_lossy().to_ascii_lowercase();
        assert!(path_str.contains("system32"));
        assert!(is_system);
    }

    #[test]
    fn test_path_cache() {
        let mut cache = PathCache::new();

        // The Windows directory definitely exists
        assert!(cache.exists(r"C:\Windows"));
        // Second call hits the cache
        assert!(cache.exists(r"C:\Windows"));

        let (hits, misses) = cache.stats();
        assert_eq!(hits, 1);
        assert_eq!(misses, 1);
    }

    #[test]
    fn test_is_definitely_safe_system_exe() {
        let mut cache = PathCache::new();
        // System32 paths are not allowed
        assert!(!is_definitely_safe_to_delete(
            r"C:\Windows\System32\notepad.exe",
            &mut cache
        ));
        // rundll32.exe is not allowed
        assert!(!is_definitely_safe_to_delete(
            r"C:\Program Files\App\rundll32.exe",
            &mut cache
        ));
    }
}
