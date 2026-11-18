// ============================================================================
// Context menu scan module
// Scans context menu entries registered in the Windows registry, identifying invalid entries that point to non-existent executables
// ============================================================================
//
// [Scan paths]
// The following paths cover both the HKEY_LOCAL_MACHINE and HKEY_CURRENT_USER trees,
// Each location has both `shell\<key name>` (traditional form) and `shellex\ContextMenuHandlers\<key name>` (COM handler) forms:
//
//   *\shell                        — right-click any file
//   Directory\shell                — right-click a folder icon
//   Directory\Background\shell     — right-click blank space **inside a folder window** (the desktop wallpaper case is
//                                    the DesktopBackground class, not here)
//   Drive\shell                    — right-click a disk drive
//   LibraryFolder\Background\shell — right-click blank space in a "Library"
//   Folder\shellex\...             — COM handler for folders
//   AllFilesystemObjects\shellex\… — COM handler common to files and folders
//
// [Core logic]
// For each shell subkey, read:
//   1. (Default) or MUIVerb value as the menu display name
//   2. Icon value as the icon path
//   3. command subkey's (Default) value as the executable command
// Then extract the exe path from the command string and check whether the file exists
//
// [Safety measures]
// - The scan phase is read-only and never modifies the registry
// - Deletion requires explicit user confirmation; export a separate .reg backup before deleting each entry
// - Only entries where "a path can be extracted and the file is genuinely absent" are marked invalid and checked by default;
//   COM handlers (shellex\ContextMenuHandlers) are display-only: the frontend disables selection and the backend blocks them again
// - HKLM entries are marked as requiring admin privileges; the frontend shows an elevation hint
// ============================================================================

use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::Path;
use winreg::enums::*;
use winreg::RegKey;

// ============================================================================
// Windows API FFI (for resolving MUIVerb indirect strings @path,-id)
// ============================================================================

#[link(name = "shlwapi")]
extern "system" {
    /// https://learn.microsoft.com/en-us/windows/win32/api/shlwapi/nf-shlwapi-shloadindirectstring
    fn SHLoadIndirectString(
        pszSource: *const u16,
        pszOutBuf: *mut u16,
        cchOutBuf: u32,
        ppvReserved: *mut *const std::ffi::c_void,
    ) -> i32;
}

/// Calls SHLoadIndirectString to resolve indirect strings in @path,-id form
/// Returns the original string on failure
fn resolve_indirect_string(raw: &str) -> String {
    if !raw.starts_with('@') {
        return raw.to_string();
    }

    // Expand environment variables first (e.g. @%SystemRoot%\system32\shell32.dll,-21769)
    let expanded = expand_env_vars(raw);

    // Convert to UTF-16 wide characters
    let source_wide: Vec<u16> = expanded.encode_utf16().chain(std::iter::once(0)).collect();
    let mut out_buf: Vec<u16> = vec![0u16; 512]; // 512 characters are enough for the menu text

    let hr = unsafe {
        SHLoadIndirectString(
            source_wide.as_ptr(),
            out_buf.as_mut_ptr(),
            out_buf.len() as u32,
            std::ptr::null_mut(),
        )
    };

    if hr == 0 {
        // S_OK — success, truncate at null
        if let Some(null_pos) = out_buf.iter().position(|&c| c == 0) {
            let resolved_wide = &out_buf[..null_pos];
            return OsString::from_wide(resolved_wide)
                .to_string_lossy()
                .to_string();
        }
    }

    // Resolution failed; fall back to the original value
    raw.to_string()
}

/// Expands `%NAME%` environment variables in a command.
///
/// No fixed list: context menu commands commonly use more than `%SystemRoot%`; per-user installs write
/// `%LOCALAPPDATA%\Programs\...`, so expand using the system's actual values. Unknown variables are left as-is,
/// so the UI still shows what the command originally looked like, and the caller reports Unknown when no absolute path can be resolved.
fn expand_env_vars(path: &str) -> String {
    let mut result = String::with_capacity(path.len());
    let mut rest = path;

    while let Some(start) = rest.find('%') {
        result.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            // Found a matching %: check whether the text between is a real variable name
            Some(end) if end > 0 => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(value) => result.push_str(&value),
                    Err(_) => {
                        result.push('%');
                        result.push_str(name);
                        result.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            // Unpaired %: treat as an ordinary character; don't swallow what follows
            _ => {
                result.push('%');
                rest = after;
            }
        }
    }
    result.push_str(rest);
    result
}

/// Sort rank: invalid entries first, normal ones next (so the list opens looking "nothing's wrong");
/// undetermined ones last — they need no action and shouldn't take up the first screen.
fn status_rank(status: ExeStatus) -> u8 {
    match status {
        ExeStatus::Missing => 0,
        ExeStatus::Ok => 1,
        ExeStatus::Unknown => 2,
    }
}

/// Three-state classification of the target executable.
///
/// Only "a path is mentioned but the file is absent" counts as invalid. Paths that can't be extracted (COM handlers, cascading menus,
/// delegate executors) are always Unknown — not counted as invalid and not checked by default.
fn classify_exe_status(exe_path: Option<&str>) -> ExeStatus {
    match exe_path {
        Some(path) if Path::new(path).exists() => ExeStatus::Ok,
        Some(_) => ExeStatus::Missing,
        None => ExeStatus::Unknown,
    }
}

/// Whether it looks like a CLSID of the form `{90AA3A4E-1CBA-4233-B8BB-535773D48449}`
fn looks_like_clsid(value: &str) -> bool {
    let value = value.trim();
    value.len() == 38 && value.starts_with('{') && value.ends_with('}')
}

/// Resolves the DLL actually loaded by the COM handler `shellex\ContextMenuHandlers\<key name>`.
///
/// Both registry forms must be recognized:
/// 1. The key name itself is a CLSID（`{90AA3A4E-...}`）；
/// 2. The key name is a friendly name（`TortoiseSVN`），`(Default)` value is the CLSID.
///
/// With the CLSID, get the DLL path from `Classes\CLSID\<guid>\InprocServer32`.
/// Check HKCU first, then HKLM — matching Windows' override order.
/// Returns `None` if it cannot be resolved, so the caller reports `Unknown` (not "invalid").
fn resolve_handler_dll(entry_key: &RegKey, key_name: &str) -> Option<String> {
    let clsid = if looks_like_clsid(key_name) {
        key_name.trim().to_string()
    } else {
        let raw = entry_key.get_value::<String, _>("").ok()?;
        if !looks_like_clsid(&raw) {
            return None;
        }
        raw.trim().to_string()
    };

    let hives = [
        (RegKey::predef(HKEY_CURRENT_USER), r"Software\Classes\CLSID"),
        (RegKey::predef(HKEY_LOCAL_MACHINE), r"SOFTWARE\Classes\CLSID"),
    ];
    for (hive, prefix) in hives {
        let path = format!(r"{}\{}\InprocServer32", prefix, clsid);
        let Ok(key) = hive.open_subkey_with_flags(&path, KEY_READ) else {
            continue;
        };
        let Ok(dll) = key.get_value::<String, _>("") else {
            continue;
        };
        // DLL paths are often stored as REG_EXPAND_SZ（`%SystemRoot%\system32\foo.dll`）
        let expanded = expand_env_vars(dll.trim());
        if expanded.is_empty() {
            continue;
        }
        // Managed extensions' InprocServer32 often contain only a bare file name（`mscoree.dll`）。
        // Calling exists() on a bare name searches the current working directory, so it's always false — showing up in the UI as
        // a false "file does not exist" report, and these entries are deletable — handing the user a way to delete by mistake.
        if let Some(resolved) = resolve_bare_module(&expanded) {
            return Some(resolved);
        }
        // If resolution fails at this level, continue to the next (a broken HKCU entry doesn't mean the HKLM one is bad too)
    }
    None
}

/// Completes a bare file name (`mscoree.dll`) into a real path.
///
/// Searches System32 / SysWOW64 / system root / `PATH` in order; returns `None` if none match,
/// so the caller reports `Unknown` rather than "file no longer exists".
fn resolve_bare_module(name: &str) -> Option<String> {
    if name.contains('\\') || name.contains('/') {
        return Some(name.to_string());
    }

    let system_root =
        std::env::var("SystemRoot").unwrap_or_else(|_| String::from(r"C:\Windows"));
    let root = Path::new(&system_root);
    let system_dirs = [
        root.join("System32").join(name),
        root.join("SysWOW64").join(name),
        root.join(name),
    ];
    if let Some(found) = system_dirs.into_iter().find(|candidate| candidate.exists()) {
        return Some(found.to_string_lossy().to_string());
    }

    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.exists())
        .map(|candidate| candidate.to_string_lossy().to_string())
}

/// Takes the first token of a command line (split on whitespace)
fn first_token(command: &str) -> &str {
    command.split_whitespace().next().unwrap_or(command)
}

/// Whether it qualifies as an exe path (only `.exe` endings count, to avoid treating a directory as a program)
fn is_exe_path(candidate: &str) -> bool {
    candidate.len() > 4 && candidate.to_ascii_lowercase().ends_with(".exe")
}

/// Whether it is an absolute path (starts with a drive letter or UNC `\\server\share`)
fn is_absolute_path(token: &str) -> bool {
    let mut chars = token.chars();
    match (chars.next(), chars.next(), chars.next()) {
        (Some(letter), Some(':'), Some('\\' | '/')) => letter.is_ascii_alphabetic(),
        _ => token.starts_with(r"\\"),
    }
}

/// Whether it is a "delegate executor": the command itself is not the real target; the real target is in the arguments
fn is_delegate_executor(token: &str) -> bool {
    let lower = token.to_lowercase();
    ["rundll32", "msiexec", "regsvr32", "wscript", "cscript"]
        .iter()
        .any(|name| lower.contains(name))
}

/// In commands where "the path has spaces but no quotes", rebuild the real exe segment by segment on whitespace.
///
/// Only consider candidates that "end in .exe and whose file really exists"; if none is found, return None to the caller —
/// Don't guess, don't fabricate; a wrong guess means a false-positive-level mistaken deletion.
fn find_existing_exe_prefix(command: &str) -> Option<String> {
    let mut candidate = String::new();
    for part in command.split_whitespace() {
        if !candidate.is_empty() {
            candidate.push(' ');
        }
        candidate.push_str(part);
        if is_exe_path(&candidate) && Path::new(&candidate).exists() {
            return Some(candidate);
        }
    }
    None
}

/// Resolves bare exe names in a command (`cmd.exe`, `explorer.exe`) into real paths.
///
/// Only accepts `.exe` endings with no path separators, then delegates to `resolve_bare_module`.
/// Returns `None` (Unknown) when not found — **never return a fabricated path**; commands resolved via PATH
/// would otherwise be misjudged as "file no longer exists".
fn resolve_system_executable(token: &str) -> Option<String> {
    if !is_exe_path(token) || token.contains('\\') || token.contains('/') {
        return None;
    }
    resolve_bare_module(token)
}

// ============================================================================
// Data structures
// ============================================================================

/// Context menu scan result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMenuScanResult {
    /// All menu entries found during the scan
    pub entries: Vec<ContextMenuEntry>,
    /// Number of invalid entries among them (exe missing)
    pub invalid_count: usize,
    /// Scan duration in milliseconds
    pub scan_duration_ms: u64,
}

/// Result of the existence check for the target executable.
///
/// Deliberately three states rather than a `bool`: being unable to extract a path from the command and "path exists but the file is gone" are two different things.
/// In the former case (`rundll32 shell32.dll,...`, `explorer.exe`, forms using unknown env vars) the target is actually
/// hidden in the arguments or elsewhere; marking it "invalid" would make the user delete normal menus too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExeStatus {
    /// A path was mentioned and the file exists
    Ok,
    /// A path was mentioned but the file is no longer on disk — only this counts as an "invalid entry"
    Missing,
    /// No determinable path could be extracted; cannot be judged
    Unknown,
}

/// A single context menu entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMenuEntry {
    /// Unique ID used to locate the entry for deletion (reg_root + "||" + reg_subpath)
    pub id: String,
    /// Menu display name (MUIVerb indirect string already resolved)
    pub display_name: String,
    /// Registry subkey name (the subkey under shell, e.g. "VSCode")
    pub key_name: String,
    /// Full registry path (for UI display)
    pub registry_path: String,
    /// Registry root ("HKCU" | "HKLM")
    pub reg_root: String,
    /// Subpath relative to the root (e.g. "SOFTWARE\\Classes\\*\\shell\\MyApp")
    pub reg_subpath: String,
    /// Scope of the context menu entry, stable English id:
    /// `file`（any file）/ `folder`（folder）/ `background`（blank space in a folder window）/
    /// `drive`（disk drive）/ `library`（library folder）。UI text is translated by the frontend.
    pub scope: String,
    /// Icon path (raw value, may include an index suffix such as "C:\foo.exe,0")
    pub icon_path: Option<String>,
    /// Original command string (from the command subkey)
    pub command: Option<String>,
    /// Executable path extracted from the command (the pure path with arguments removed)
    pub exe_path: Option<String>,
    /// Whether the target executable is still on disk (three states, see `ExeStatus`)
    pub exe_status: ExeStatus,
    /// Whether deleting this entry requires admin privileges (HKLM entries do)
    pub needs_admin: bool,
    /// Whether this is a system-protected entry (cannot be selected for deletion).
    ///
    /// COM handlers are read-only by default; those whose handler DLL is already gone don't count as protected — they are leftovers and can be cleaned.
    pub is_system_protected: bool,
    /// Risk level ("safe" | "caution" | "danger")
    pub risk_level: String,
}

/// Context menu entry deletion request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMenuDeleteRequest {
    /// Entry's unique ID
    pub id: String,
    /// Registry root ("HKCU" | "HKLM")
    pub reg_root: String,
    /// Subpath relative to the root
    pub reg_subpath: String,
}

/// Context menu deletion result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMenuDeleteResult {
    /// Number of entries successfully deleted
    pub deleted_count: usize,
    /// Number of entries that failed to delete
    pub failed_count: usize,
    /// Detailed result for each entry
    pub details: Vec<ContextMenuDeleteDetail>,
}

/// Deletion details for a single entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMenuDeleteDetail {
    /// Entry ID
    pub id: String,
    /// Whether it succeeded
    pub success: bool,
    /// Failure reason (None on success)
    pub error: Option<String>,
}

// ============================================================================
// About the whitelist
//
// There used to be a key-name whitelist here that skipped "VSCode / 7-Zip / WinRAR / cmd ...".
// It skipped entire entries for "known good software", at the cost of **missing uninstall leftovers** — after the software was uninstalled,
// the registry entries remained under those key names, which is exactly what this module should catch (`HKCU\...\*\shell\VSCode`
// was swallowed whole in local testing).
//
// Now there is no pre-judgment by key name; everything is decided by whether the target executable exists: if the file is there, it is a normal entry (not
// checked by default); if the file is gone, it is a leftover. Built-in system verbs (open / print / properties, etc.) were never
// under these `shell` subkeys in the first place, so no whitelist is needed.
// ============================================================================

// ============================================================================
// Context menu scanner
// ============================================================================

/// Context menu scanner
pub struct ContextMenuScanner;

impl ContextMenuScanner {
    /// Creates a new scanner instance
    pub fn new() -> Self {
        ContextMenuScanner
    }

    /// Performs a full scan and returns all context menu entries found
    pub fn scan(&self) -> Result<ContextMenuScanResult, String> {
        let start = std::time::Instant::now();
        let mut entries: Vec<ContextMenuEntry> = Vec::new();

        // Scan HKEY_LOCAL_MACHINE (admin rights required to delete)
        self.scan_hive(
            RegKey::predef(HKEY_LOCAL_MACHINE),
            "HKLM",
            r"SOFTWARE\Classes",
            true,
            &mut entries,
        );

        // Scan HKEY_CURRENT_USER (current-user level, no admin needed)
        self.scan_hive(
            RegKey::predef(HKEY_CURRENT_USER),
            "HKCU",
            r"Software\Classes",
            false,
            &mut entries,
        );

        // Deduplicate by display name, preferring HKCU entries (user level takes priority)
        entries = Self::deduplicate(entries);

        // Sort: invalid first, undetermined in the middle, normal last; within each group, sort by display name
        entries.sort_by(|a, b| {
            status_rank(a.exe_status)
                .cmp(&status_rank(b.exe_status))
                .then_with(|| {
                    a.display_name
                        .to_lowercase()
                        .cmp(&b.display_name.to_lowercase())
                })
        });

        let invalid_count = entries
            .iter()
            .filter(|entry| entry.exe_status == ExeStatus::Missing)
            .count();
        let scan_duration_ms = start.elapsed().as_millis() as u64;

        log::info!(
            "右键菜单扫描完成: {} 条目（其中 {} 个无效），耗时 {}ms",
            entries.len(),
            invalid_count,
            scan_duration_ms
        );

        Ok(ContextMenuScanResult {
            entries,
            invalid_count,
            scan_duration_ms,
        })
    }

    /// Scans all context menu paths under a given registry hive
    fn scan_hive(
        &self,
        hive: RegKey,
        hive_name: &str,
        classes_path: &str,
        needs_admin: bool,
        entries: &mut Vec<ContextMenuEntry>,
    ) {
        // Define the (scope_id, relative_path_from_classes) to scan
        //
        // scope holds only **stable English ids**; the frontend renders UI text per language.
        // This used to contain Chinese ("any file" / "desktop background"), resulting in a double bug:
        //   · Non-Chinese UIs leaked the Chinese labels verbatim;
        //   · The same id was hard-coded in two places, so updating one missed the other.
        //
        // Both registration forms must be scanned:
        //   * `...\shell\<key name>`            — traditional form, command stored in the command subkey
        //   * `...\shellex\ContextMenuHandlers\<key name>` — COM handler, command is the DLL pointed to by the CLSID
        // The latter is the most common hiding place for uninstall leftovers (software deleted but CLSID registration remains).
        let scan_targets: &[(&str, &str)] = &[
            ("file", r"*\shell"),
            ("folder", r"Directory\shell"),
            // Directory\Background is **blank space inside a folder window**, not the desktop wallpaper case
            // (that is the DesktopBackground class). The old label "desktop background" was simply wrong
            ("background", r"Directory\Background\shell"),
            ("drive", r"Drive\shell"),
            ("library", r"LibraryFolder\Background\shell"),
            // COM handlers: scope follows "where you right-click"; no separate scope is created
            ("file", r"*\shellex\ContextMenuHandlers"),
            ("folder", r"Directory\shellex\ContextMenuHandlers"),
            (
                "background",
                r"Directory\Background\shellex\ContextMenuHandlers",
            ),
            ("folder", r"Folder\shellex\ContextMenuHandlers"),
            ("drive", r"Drive\shellex\ContextMenuHandlers"),
            ("file", r"AllFilesystemObjects\shellex\ContextMenuHandlers"),
        ];

        for (scope, rel_path) in scan_targets {
            let full_rel = format!(r"{}\{}", classes_path, rel_path);

            match hive.open_subkey_with_flags(&full_rel, KEY_READ) {
                Ok(shell_key) => {
                    self.scan_shell_key(
                        &shell_key,
                        hive_name,
                        &format!("{}\\{}", hive_name, full_rel),
                        &format!(r"{}", full_rel),
                        scope,
                        needs_admin,
                        entries,
                    );
                }
                Err(_) => {
                    // Path does not exist; skip silently
                }
            }
        }
    }

    /// Scans all subkeys under a single shell key (each subkey corresponds to one menu item)
    fn scan_shell_key(
        &self,
        shell_key: &RegKey,
        hive_name: &str,
        display_prefix: &str,
        subpath_prefix: &str,
        scope: &str,
        needs_admin: bool,
        entries: &mut Vec<ContextMenuEntry>,
    ) {
        for subkey_name in shell_key.enum_keys().filter_map(|k| k.ok()) {
            let subkey_path = format!(r"{}\{}", subpath_prefix, subkey_name);
            let display_path = format!(r"{}\{}", display_prefix, subkey_name);

            match shell_key.open_subkey_with_flags(&subkey_name, KEY_READ) {
                Ok(entry_key) => {
                    if let Some(entry) = self.build_entry(
                        &entry_key,
                        &subkey_name,
                        &display_path,
                        hive_name,
                        &subkey_path,
                        scope,
                        needs_admin,
                    ) {
                        entries.push(entry);
                    }
                }
                Err(_) => continue,
            }
        }
    }

    /// Builds a ContextMenuEntry from a registry key
    fn build_entry(
        &self,
        entry_key: &RegKey,
        key_name: &str,
        registry_path: &str,
        reg_root: &str,
        reg_subpath: &str,
        scope: &str,
        needs_admin: bool,
    ) -> Option<ContextMenuEntry> {
        // Read the menu display name (MUIVerb first, then (Default), then fall back to the key name)
        let display_name_raw: String = entry_key
            .get_value::<String, _>("MUIVerb")
            .or_else(|_| entry_key.get_value::<String, _>(""))
            .unwrap_or_else(|_| key_name.to_string());

        // Filter out entries with an empty or whitespace-only name
        let display_name_raw = display_name_raw.trim().to_string();
        if display_name_raw.is_empty() && key_name.trim().is_empty() {
            return None;
        }
        let display_name_raw = if display_name_raw.is_empty() {
            key_name.to_string()
        } else {
            display_name_raw
        };

        // Resolve the MUIVerb indirect string (@path,-id → human-readable text)
        let display_name = resolve_indirect_string(&display_name_raw);

        // Read the icon path
        let icon_path: Option<String> = entry_key.get_value::<String, _>("Icon").ok();

        // COM handlers are not driven by the command subkey; their command is the DLL pointed to by the CLSID.
        // Without resolving it, this whole class of entries stays "undetermined" no matter how many scan locations are added.
        let is_com_handler = reg_subpath
            .to_lowercase()
            .contains(r"shellex\contextmenuhandlers");

        // Read the command string (from the default value of the command subkey)
        let command: Option<String> = entry_key
            .open_subkey_with_flags("command", KEY_READ)
            .ok()
            .and_then(|cmd_key| cmd_key.get_value::<String, _>("").ok());

        // Extract the exe path from the command string; for COM handlers, resolve the handler DLL from the CLSID instead
        let exe_path: Option<String> = if is_com_handler {
            resolve_handler_dll(entry_key, key_name)
        } else {
            command
                .as_deref()
                .and_then(|cmd| Self::extract_exe_path(cmd))
        };

        // Three-state classification of the target executable: only "a path is mentioned but the file is absent" counts as invalid.
        // Paths that cannot be extracted (cascading menus, delegate executors, unresolvable CLSIDs) are always Unknown,
        // not counted as invalid and not checked by default — better to miss one than give the user a chance to delete by mistake.
        let exe_status = classify_exe_status(exe_path.as_deref());

        // System-protection check: COM handlers are read-only by default, but those whose **handler DLL is already gone** do not count —
        // that is exactly an uninstall leftover, and keeping it only makes Explorer look for a non-existent DLL on every right-click.
        let is_system_protected = is_com_handler && exe_status != ExeStatus::Missing;

        // Risk classification
        let risk_level = if is_system_protected {
            "danger".to_string()
        } else if needs_admin {
            "caution".to_string()
        } else {
            "safe".to_string()
        };

        let id = format!("{}||{}", reg_root, reg_subpath);

        Some(ContextMenuEntry {
            id,
            display_name,
            key_name: key_name.to_string(),
            registry_path: registry_path.to_string(),
            reg_root: reg_root.to_string(),
            reg_subpath: reg_subpath.to_string(),
            scope: scope.to_string(),
            icon_path,
            command,
            exe_path,
            exe_status,
            needs_admin,
            is_system_protected,
            risk_level,
        })
    }

    /// Extracts the executable path from a command string
    ///
    /// Handles the following common formats:
    /// - `"C:\Program Files\App\app.exe" "%1"` (standard form)
    /// - `C:\Program Files\App\app.exe %1` (path has spaces but no quotes, common in old installers)
    /// - `%SystemRoot%\system32\cmd.exe /c ...`、`%LOCALAPPDATA%\Programs\App\App.exe`
    /// - `cmd.exe /c ...` (bare file name only, resolved via system directories)
    ///
    /// Returning `None` means **undetermined**; the caller then assigns `ExeStatus::Unknown`.
    /// Do not treat `None` as "file does not exist" — that would create a whole class of false positives.
    fn extract_exe_path(cmd: &str) -> Option<String> {
        let raw = cmd.trim();
        if raw.is_empty() {
            return None;
        }

        // Expand environment variables first: per-user installed software (Chrome / VSCode / various Electron apps)
        // mostly writes %LOCALAPPDATA%; recognizing only the %SystemRoot%-style variables would miss the whole class
        let expanded = expand_env_vars(raw);

        // Delegate executors (rundll32 / msiexec / regsvr32 / wscript / cscript) always exist themselves,
        // the thing actually invoked is in the arguments (.dll / .msi / .vbs), so validity cannot be judged from the exe alone
        if is_delegate_executor(first_token(&expanded)) {
            return None;
        }

        // With quotes: take the content of the first pair of quotes
        if let Some(rest) = expanded.strip_prefix('"') {
            if let Some(end) = rest.find('"') {
                let path = rest[..end].trim();
                if !path.is_empty() {
                    return Some(path.to_string());
                }
            }
            // Only an opening quote (broken registry entry); fall back to treating it as a bare path
        }

        // No quotes but spaces in the path: take the earliest prefix that "ends in .exe and whose file really exists".
        // Splitting on the first whitespace yields `C:\Program`, one of the main sources of false positives
        if let Some(found) = find_existing_exe_prefix(&expanded) {
            return Some(found);
        }

        let token = first_token(&expanded);
        if is_absolute_path(token) {
            return Some(token.to_string());
        }

        // Bare file name: only counts if it really exists in a system directory
        resolve_system_executable(token)
    }

    /// Deduplicates entries: when the same registry path exists in both HKCU and HKLM, keep only one.
    ///
    /// The dedup key uses the **full subpath without the hive**, no longer `(scope, key_name)`:
    /// Now each scope has two parent paths, `shell` and `shellex\ContextMenuHandlers`,
    /// deduping by name would mistake two entries in different locations for the same one.
    ///
    /// Which one to keep: prefer the invalid one. When HKCU has a working entry and HKLM has a leftover with the same name,
    /// keeping only HKCU would hide that leftover forever — and it is exactly what this module is looking for.
    fn deduplicate(entries: Vec<ContextMenuEntry>) -> Vec<ContextMenuEntry> {
        use std::collections::HashMap;

        let mut best: HashMap<String, ContextMenuEntry> = HashMap::new();
        // Track order separately: HashMap is unordered, so iterating directly would make the list order differ on every scan
        let mut order: Vec<String> = Vec::new();

        for entry in entries {
            let key = entry.reg_subpath.to_lowercase();
            match best.get(&key) {
                None => {
                    order.push(key.clone());
                    best.insert(key, entry);
                }
                Some(existing) => {
                    let replace = match (existing.exe_status, entry.exe_status) {
                        // The one already kept is invalid; do not replace
                        (ExeStatus::Missing, _) => false,
                        // The new one is invalid; replace the "apparently normal" one
                        (_, ExeStatus::Missing) => true,
                        // Neither is invalid: follow the rule that HKCU overrides HKLM
                        _ => existing.reg_root == "HKLM" && entry.reg_root == "HKCU",
                    };
                    if replace {
                        best.insert(key, entry);
                    }
                }
            }
        }

        order
            .into_iter()
            .filter_map(|key| best.remove(&key))
            .collect()
    }
}

// ============================================================================
// Context menu deleter
// ============================================================================

/// Performs deletion of context menu entries
pub fn delete_context_menu_entries(
    requests: &[ContextMenuDeleteRequest],
) -> ContextMenuDeleteResult {
    let mut details: Vec<ContextMenuDeleteDetail> = Vec::new();
    let mut deleted_count: usize = 0;
    let mut failed_count: usize = 0;

    for req in requests {
        let result = delete_single_entry(req);
        let success = result.is_ok();

        if success {
            deleted_count += 1;
            log::info!("已删除右键菜单条目: {}\\{}", req.reg_root, req.reg_subpath);
        } else {
            failed_count += 1;
            let err_msg = result.err().unwrap_or_else(|| "未知错误".to_string());
            log::warn!(
                "删除右键菜单条目失败: {}\\{} - {}",
                req.reg_root,
                req.reg_subpath,
                err_msg
            );
            details.push(ContextMenuDeleteDetail {
                id: req.id.clone(),
                success: false,
                error: Some(err_msg),
            });
            continue;
        }

        details.push(ContextMenuDeleteDetail {
            id: req.id.clone(),
            success: true,
            error: None,
        });
    }

    // Notify the shell to refresh after deletion. Without this, the user right-clicking on the desktop would find the menu still there —
    // Explorer caches menus, so the change only shows after logout or restart, looking like "the deletion did not take effect".
    // Only send a change notification; do not kill explorer.exe (that would also restart taskbar extensions).
    if deleted_count > 0 {
        if let Err(error) = crate::scanner::shell_icons::restart_explorer() {
            log::warn!("通知外壳刷新失败: {}", error);
        }
    }

    ContextMenuDeleteResult {
        deleted_count,
        failed_count,
        details,
    }
}

/// Deletes a single registry entry (recursively deletes the entire shell subkey)
/// Automatically exports a .reg backup to the data directory before deletion
fn delete_single_entry(req: &ContextMenuDeleteRequest) -> Result<(), String> {
    let subpath = req.reg_subpath.as_str();
    let lower_subpath = subpath.to_lowercase();

    // Safety valve one: the path must fall within an allowed context menu cleanup location.
    // The frontend only sends back the three fields id/root/subpath, and this command is completely open to callers —
    // without validation, a single bad call could delete any key in the registry.
    if !is_deletable_subpath(&lower_subpath) {
        return Err(format!(
            "不在允许清理的右键菜单位置内，已拒绝删除: {}",
            subpath
        ));
    }

    // Select the root hive based on reg_root
    let hive = match req.reg_root.as_str() {
        "HKCU" => RegKey::predef(HKEY_CURRENT_USER),
        "HKLM" => RegKey::predef(HKEY_LOCAL_MACHINE),
        other => return Err(format!("不支持的注册表根: {}", other)),
    };

    // reg_subpath is a full subpath of the form "SOFTWARE\Classes\*\shell\MyApp"
    // It needs to be split into the parent path and the final key name
    let (parent_path, key_name) = match subpath.rfind('\\') {
        Some(pos) => (&subpath[..pos], &subpath[pos + 1..]),
        None => {
            return Err(format!("无效的子路径格式（缺少父路径）: {}", subpath));
        }
    };

    // Safety valve two: for COM handlers, only allow those "whose handler DLL is already gone".
    // This must be resolved again; do not trust the scan result — the user may have reinstalled the software between scan and deletion,
    // or someone may have called the command directly, bypassing the UI. If it cannot be determined, do not delete.
    if lower_subpath.contains(r"shellex\contextmenuhandlers") {
        let entry_key = hive
            .open_subkey_with_flags(subpath, KEY_READ)
            .map_err(|error| format!("无法读取条目 {}: {}", subpath, error))?;
        match resolve_handler_dll(&entry_key, key_name) {
            Some(dll) if !Path::new(&dll).exists() => { /* DLL is gone, so it's a leftover — allow deletion */ }
            Some(dll) => {
                return Err(format!("处理器程序仍然存在（{}），不允许删除", dll));
            }
            None => {
                return Err("无法确认处理器程序是否还存在，已拒绝删除".to_string());
            }
        }
    }

    // Export a .reg backup before deletion
    export_reg_backup(req.reg_root.as_str(), subpath);

    // Open the parent key (write access required)
    let parent_key = hive
        .open_subkey_with_flags(parent_path, KEY_READ | KEY_WRITE)
        .map_err(|e| format!("无法打开父键 {}: {}", parent_path, e))?;

    // Recursively delete the subkey and all its contents
    parent_key
        .delete_subkey_all(key_name)
        .map_err(|e| format!("删除键 {} 失败: {}", key_name, e))?;

    Ok(())
}

/// Parent paths allowed for deletion (hive excluded, all lowercase). Must match one of them, followed by exactly one key name.
///
/// One-to-one with the scan locations. If `scan_targets` changes, update this too, or new locations will be scannable but not deletable.
const DELETABLE_PARENTS: &[&str] = &[
    r"software\classes\*\shell\",
    r"software\classes\directory\shell\",
    r"software\classes\directory\background\shell\",
    r"software\classes\drive\shell\",
    r"software\classes\libraryfolder\background\shell\",
    r"software\classes\*\shellex\contextmenuhandlers\",
    r"software\classes\directory\shellex\contextmenuhandlers\",
    r"software\classes\directory\background\shellex\contextmenuhandlers\",
    r"software\classes\folder\shellex\contextmenuhandlers\",
    r"software\classes\drive\shellex\contextmenuhandlers\",
    r"software\classes\allfilesystemobjects\shellex\contextmenuhandlers\",
];

/// Whether a subpath falls within an allowed cleanup location (input must already be lowercase)
fn is_deletable_subpath(lower_subpath: &str) -> bool {
    DELETABLE_PARENTS.iter().any(|parent| {
        lower_subpath
            .strip_prefix(parent)
            .is_some_and(|name| !name.is_empty() && !name.contains('\\'))
    })
}

/// Backup directory for context menu entries' `.reg` files.
///
/// Not the same directory as the registry redundancy module's `RegistryBackups` — the two also use different filename formats,
/// mixing them would leave users unable to tell which backup came from which module.
pub fn context_menu_backup_dir() -> std::path::PathBuf {
    crate::data_dir::get_data_dir().join("reg_backups")
}

/// Exports a .reg backup of a registry key (for recovering from mistaken deletion)
/// Uses the reg.exe export command; backups are stored under reg_backups/ in the data directory
fn export_reg_backup(reg_root: &str, subpath: &str) {
    let backup_dir = context_menu_backup_dir();
    if let Err(e) = std::fs::create_dir_all(&backup_dir) {
        log::warn!("无法创建注册表备份目录: {}", e);
        return;
    }

    // Generate a unique filename: reg_root_subpath_timestamp.reg
    let safe_name = format!(
        "{}_{}",
        reg_root,
        subpath.replace('\\', "_").replace('*', "all")
    );
    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let file_name = format!("{}_{}.reg", safe_name, timestamp);
    let backup_path = backup_dir.join(&file_name);

    // Call reg.exe export to export
    match std::process::Command::new("reg.exe")
        .args(["export", &format!("{}\\{}", reg_root, subpath)])
        .arg(&backup_path)
        .arg("/y") // overwrite existing file
        .output()
    {
        Ok(output) => {
            if output.status.success() {
                log::info!("已导出注册表备份: {}", backup_path.display());
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                log::warn!("注册表备份导出失败: {}", stderr);
            }
        }
        Err(e) => {
            log::warn!("执行 reg.exe export 失败: {}", e);
        }
    }
}

// ============================================================================
// Unit tests
//
// Mistakes here are costly (misjudging as "invalid" → user selects all → normal menus get deleted),
// so the pure-function parts must be covered by tests: path extraction, variable expansion, and three-state classification.
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_any_env_var_and_keeps_unknown_ones() {
        let system_root = std::env::var("SystemRoot").expect("Windows 一定有 SystemRoot");
        assert_eq!(
            expand_env_vars(r"%SystemRoot%\System32\cmd.exe"),
            format!(r"{}\System32\cmd.exe", system_root)
        );
        // Undefined variables are kept as-is so the UI still shows the command's original form
        assert_eq!(
            expand_env_vars(r"%LIGHTC_NOT_A_REAL_VAR%\app.exe"),
            r"%LIGHTC_NOT_A_REAL_VAR%\app.exe"
        );
        // An unpaired % must not be swallowed
        assert_eq!(expand_env_vars(r"app.exe %1"), r"app.exe %1");
    }

    #[test]
    fn extracts_quoted_path_with_spaces() {
        let extracted = ContextMenuScanner::extract_exe_path(
            r#""C:\Program Files\App\app.exe" "%1""#,
        );
        assert_eq!(extracted.as_deref(), Some(r"C:\Program Files\App\app.exe"));
    }

    #[test]
    fn extracts_unquoted_path_with_spaces() {
        // Create a temp directory with spaces to simulate the old-style "path with spaces but no quotes"
        let dir = std::env::temp_dir().join("lightc ctx menu test");
        std::fs::create_dir_all(&dir).expect("创建临时目录失败");
        let exe = dir.join("app.exe");
        std::fs::write(&exe, [0u8; 1]).expect("写入临时文件失败");

        let command = format!(r"{} -open", exe.display());
        let extracted = ContextMenuScanner::extract_exe_path(&command);

        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(extracted, Some(exe.to_string_lossy().to_string()));
    }

    #[test]
    fn leaves_delegate_executors_unknown() {
        // The real target is in the arguments (.dll / .msi / .vbs), which cannot be determined and must not be marked invalid
        for command in [
            r"rundll32.exe shell32.dll,OpenAs_RunDLL %1",
            r"C:\Windows\System32\msiexec.exe /i foo.msi",
            r"regsvr32.exe /s something.dll",
        ] {
            assert_eq!(
                ContextMenuScanner::extract_exe_path(command),
                None,
                "不该给出路径: {}",
                command
            );
        }
    }

    #[test]
    fn resolves_bare_system_executable() {
        // `cmd.exe /c ...` is resolved via system directories; once resolved it is Ok, not Unknown
        let extracted = ContextMenuScanner::extract_exe_path(r"cmd.exe /c dir");
        let path = extracted.expect("cmd.exe 应该能落到系统目录");
        assert!(Path::new(&path).exists(), "解析出的路径应真实存在: {}", path);
    }

    #[test]
    fn never_fabricates_path_for_unknown_bare_command() {
        // If a bare file name cannot be found, admit it — do not fabricate something that "looks like a path"
        assert_eq!(
            ContextMenuScanner::extract_exe_path("lightc_no_such_command.exe /silent"),
            None
        );
    }

    #[test]
    fn recognizes_clsid_shapes() {
        assert!(looks_like_clsid("{90AA3A4E-1CBA-4233-B8BB-535773D48449}"));
        assert!(looks_like_clsid("  {f81e9010-6ea4-11ce-a7ff-00aa003ca9f6}  "));
        // Friendly names, empty strings, and wrong lengths do not count
        assert!(!looks_like_clsid("TortoiseSVN"));
        assert!(!looks_like_clsid("{90AA3A4E-1CBA}"));
        assert!(!looks_like_clsid(""));
    }

    #[test]
    fn completes_bare_module_names_against_system_dirs() {
        // A managed extension's InprocServer32 is a bare name (mscoree.dll); without completion it would be misjudged as "file does not exist"
        let resolved = resolve_bare_module("kernel32.dll").expect("kernel32.dll 一定在系统目录");
        assert!(Path::new(&resolved).exists());
        assert_eq!(resolve_bare_module("lightc_no_such_module.dll"), None);
        // Names with a path are returned as-is, no guessing
        assert_eq!(
            resolve_bare_module(r"C:\x\y.dll").as_deref(),
            Some(r"C:\x\y.dll")
        );
    }

    #[test]
    fn only_deletes_paths_inside_known_context_menu_locations() {
        // Allowed: scan location + one key name
        assert!(is_deletable_subpath(
            r"software\classes\*\shell\myapp"
        ));
        assert!(is_deletable_subpath(
            r"software\classes\directory\shellex\contextmenuhandlers\tortoisesvn"
        ));
        // Rejected: outside allowed locations
        assert!(!is_deletable_subpath(
            r"software\microsoft\windows\currentversion\run\evil"
        ));
        assert!(!is_deletable_subpath(r"software\classes\*\shell"));
        // Rejected: trying to go one level deeper by abusing the prefix
        assert!(!is_deletable_subpath(
            r"software\classes\*\shell\myapp\command"
        ));
        // Rejected: the parent itself is the target (no key name)
        assert!(!is_deletable_subpath(r"software\classes\directory\shell\"));
    }

    #[test]
    fn classifies_only_known_missing_paths_as_invalid() {
        // Path given but file absent → invalid
        assert_eq!(
            classify_exe_status(Some(r"C:\lightc_no_such_dir\app.exe")),
            ExeStatus::Missing
        );
        // No path extractable → Unknown, not invalid
        assert_eq!(classify_exe_status(None), ExeStatus::Unknown);
        // Path exists → normal
        let system_root = std::env::var("SystemRoot").expect("Windows 一定有 SystemRoot");
        let existing = format!(r"{}\System32\cmd.exe", system_root);
        assert_eq!(classify_exe_status(Some(&existing)), ExeStatus::Ok);
    }
}
