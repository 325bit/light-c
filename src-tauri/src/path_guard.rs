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
// S3a audit fix. The "may this path be deleted" decision used to exist in four
// places with four different answers: `delete_engine::is_protected_path`,
// `enhanced_delete::is_system_protected`, `permanent_delete::check_protected_path`
// (its own 17-entry table), and `scan_engine::is_system_protected` (a scanner
// filter, left alone — it answers "should this be scanned", not "may this be
// deleted"). They had already drifted: `C:\Windows.old` was in the shared table
// but not in the permanent-delete one, and `C:\Windows` was only in the latter.
// Everything now funnels through `ensure_deletable`.
//
// The template was `scanner/leftovers.rs::is_safe_leftover_path_under`
// (component-wise comparison + relative-component rejection). It lives here now:
// leftovers, ai_tool_cleaner and dev_cache all call `has_relative_components`
// instead of carrying their own copy — M13 counted three byte-identical versions
// of exactly this check before they were folded in.
// ============================================================================

use std::path::{Component, Path};

use crate::error::{AppError, ErrorCode};

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
///
/// The error is typed (`invalid_path`) so the command layer can surface a real
/// code over IPC instead of a sentence the UI would have to pattern-match.
pub fn validate_deletable(path: &Path) -> Result<(), AppError> {
    if path.as_os_str().is_empty() {
        return Err(AppError::new(ErrorCode::InvalidPath).detail("path is empty"));
    }
    if has_relative_components(path) {
        return Err(AppError::new(ErrorCode::InvalidPath)
            .detail("path contains a relative component (`.` or `..`)")
            .arg("path", path.to_string_lossy()));
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

// ============================================================================
// Deletion policy
//
// `ensure_deletable` is the only place that decides whether a path may be
// deleted. It runs five layers in a fixed order:
//
//   1. empty / `.` `..`            → invalid_path
//   2. protected path rules        → protected
//   3. protected file names        → protected
//   4. protected extensions under  → protected   (scope-gated, see below)
//      the Windows directory
//   5. user / drive roots          → protected   (scope-gated)
//   6. cleanup allowlist           → invalid_path (JunkClean only)
//
// Layers 4-6 are gated by scope because the engines serve different product
// intents and the cost of a false positive differs:
//
//   - Deep-scan results legitimately include installers (.exe/.msi) sitting in
//     `C:\Windows\Temp`, so the extension layer must not apply there.
//   - Large-file cleanup deletes a file the user picked by hand; an allowlist
//     written for scanner output would refuse every video on the Desktop.
// ============================================================================

/// Which deletion surface is asking.
///
/// The scope decides whether an allowlist constrains the target and which extra
/// protection layers apply — never whether the protection *table* applies. A
/// path in `PROTECTED_PATHS` is refused under every scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteScope {
    /// Scanner-produced junk (junk clean, simple cleanup). Allowlist applies.
    JunkClean,
    /// Deep-scan results. No allowlist: the deep scanner already classified the
    /// target, and its categories include installers under `C:\Windows\Temp`.
    EnhancedClean,
    /// Uninstall leftovers. No allowlist (the scanner and the user whitelist
    /// decide), plus shell-folder protection: a "leftover" inside Documents is
    /// a scanner bug, not a cleanup target.
    Leftovers,
    /// A file the user picked by hand (large-file cleanup). No allowlist.
    ///
    /// Not wired yet — `delete_files` still passes `JunkClean`, which is the
    /// behaviour C1 shipped. Wiring it is a product decision, not a refactor:
    /// the allowlist exists to stop a compromised renderer from deleting
    /// arbitrary files, and large-file cleanup is the feature that needs it gone.
    #[allow(dead_code)]
    UserSelected,
}

/// How a protected entry is matched.
#[derive(Debug, Clone, Copy)]
enum ProtectedRule {
    /// Absolute prefix, matched from the start of the normalized path.
    Absolute(&'static str),
    /// A folder directly under a user profile (`<profile>\documents`), matched
    /// at that exact position and everything below it.
    ProfileSubtree(&'static str),
    /// A profile folder protected only as a whole (`<profile>\appdata\local`),
    /// not as a subtree — junk cleanup legitimately deletes files inside it.
    ProfileExact(&'static str),
}

impl ProtectedRule {
    fn label(self) -> &'static str {
        match self {
            Self::Absolute(prefix) => prefix,
            Self::ProfileSubtree(child) | Self::ProfileExact(child) => child,
        }
    }
}

/// The single protection table.
///
/// M13/S3a: this is the union of `safety_constants::PROTECTED_PATH_PREFIXES`
/// (21 entries, used by the junk and enhanced engines) and
/// `permanent_delete::PROTECTED_PATHS` (17 entries, its own copy). Both had
/// entries the other lacked; `C:\Windows.old` in particular was protected
/// against junk deletion but not against permanent deletion, even though
/// `safety_constants` described it as "a second gate, in case another path
/// bypasses the frontend".
///
/// Entries are lowercase and match at a component boundary, so
/// `c:\program files` no longer matches `c:\program filesx`.
const PROTECTED_PATHS: &[ProtectedRule] = &[
    // Windows system directories.
    ProtectedRule::Absolute(r"c:\windows\system32"),
    ProtectedRule::Absolute(r"c:\windows\syswow64"),
    ProtectedRule::Absolute(r"c:\windows\winsxs"),
    ProtectedRule::Absolute(r"c:\windows\servicing"),
    ProtectedRule::Absolute(r"c:\windows\assembly"),
    ProtectedRule::Absolute(r"c:\windows\boot"),
    ProtectedRule::Absolute(r"c:\windows\fonts"),
    ProtectedRule::Absolute(r"c:\windows\inf"),
    ProtectedRule::Absolute(r"c:\windows\microsoft.net"),
    ProtectedRule::Absolute(r"c:\windows\security"),
    // Old Windows installations (kept about 10 days after a system upgrade for
    // rollback). System-owned with inconsistent internal permissions: a direct
    // remove_dir_all fails midway and leaves a half-deleted state, and only Disk
    // Cleanup / Storage Sense are supported entry points for cleaning them.
    ProtectedRule::Absolute(r"c:\windows.old"),
    ProtectedRule::Absolute(r"c:\$windows.~bt"),
    ProtectedRule::Absolute(r"c:\$windows.~ws"),
    // Program directories.
    ProtectedRule::Absolute(r"c:\program files"),
    ProtectedRule::Absolute(r"c:\program files (x86)"),
    ProtectedRule::Absolute(r"c:\programdata\microsoft\windows"),
    ProtectedRule::Absolute(r"c:\programdata\microsoft\windows defender"),
    // Recovery / boot / volume metadata.
    ProtectedRule::Absolute(r"c:\recovery"),
    ProtectedRule::Absolute(r"c:\boot"),
    ProtectedRule::Absolute(r"c:\system volume information"),
    ProtectedRule::Absolute(r"c:\$recycle.bin"),
    // Profile containers that are never a cleanup target as a whole.
    ProtectedRule::Absolute(r"c:\users\default"),
    // The shared desktop is not inside a real profile, so the profile rules below
    // would never see it. It is a shortcut folder every user sees, so it stays
    // protected under every scope.
    ProtectedRule::Absolute(r"c:\users\public\desktop"),
    // Shell folders. Only the `Leftovers` scope applies these: large-file
    // cleanup deletes user-chosen files from the Desktop on purpose.
    ProtectedRule::ProfileSubtree(r"\documents"),
    ProtectedRule::ProfileSubtree(r"\desktop"),
    ProtectedRule::ProfileSubtree(r"\downloads"),
    ProtectedRule::ProfileSubtree(r"\pictures"),
    ProtectedRule::ProfileSubtree(r"\videos"),
    ProtectedRule::ProfileSubtree(r"\music"),
    // The profile containers themselves — protected as a whole, not as a
    // subtree, because every cache the app cleans lives below them.
    ProtectedRule::ProfileExact(r"\appdata\local"),
    ProtectedRule::ProfileExact(r"\appdata\roaming"),
];

/// File names absolutely forbidden from deletion (lowercase, exact match).
const PROTECTED_FILES: &[&str] = &[
    // Windows core system files
    "ntoskrnl.exe",
    "hal.dll",
    "ntdll.dll",
    "kernel32.dll",
    "kernelbase.dll",
    "user32.dll",
    "gdi32.dll",
    "advapi32.dll",
    "shell32.dll",
    "ole32.dll",
    "bootmgr",
    "bcd",
    "ntldr",
    "boot.ini",
    "pagefile.sys",
    "hiberfil.sys",
    "swapfile.sys",
    "desktop.ini",
    "ntuser.dat",
    "usrclass.dat",
    // Social app config files (to prevent data loss from mistaken deletion)
    "config.data",
    "accinfo.dat",
    "msg.db",
    "micromsg.db",
    "contact.db",
    "emotion.db",
    "favorite.db",
    "publicmsg.db",
    "nt_db",
    "nt_config",
];

/// Extensions forbidden from deletion under the Windows directory (lowercase).
const PROTECTED_EXTENSIONS_IN_WINDOWS: &[&str] = &[
    "sys", "dll", "exe", "drv", "ocx", "cpl", "msi", "msp", "msu", "cat", "mum", "manifest",
];

/// Path fragments that make a junk-clean target acceptable.
///
/// Substring matching on purpose: caches live at arbitrary depth
/// (`…\User Data\Default\Cache`), so anchoring these to a drive root would
/// refuse most of the legitimate targets.
const JUNK_ALLOWLIST: &[&str] = &[
    r"\temp",
    r"\tmp",
    r"\cache",
    r"\caches",
    r"\temporary",
    r"\appdata\local\temp",
    r"\appdata\local\microsoft\windows\temporary",
    r"\appdata\local\microsoft\windows\inetcache",
    r"\appdata\local\microsoft\windows\explorer", // thumbnail cache
    r"\windows\temp",
    r"\windows\prefetch",
    r"\windows\softwaredistribution\download",
    r"\windows\softwaredistribution\deliveryoptimization",
    r"\deliveryoptimization\",
    r"\programdata\microsoft\windows defender\localcopy",
    r"\programdata\microsoft\windows defender\support",
    r"\d3d_cache",
    r"\$recycle.bin", // recycle bin contents
    r"\.log",
    r"\.tmp",
    r"\.bak",
    r"\crash",
    r"\dumps",
];

/// Extensions that make a junk-clean target acceptable regardless of location.
const JUNK_ALLOWLIST_EXTENSIONS: &[&str] = &["tmp", "temp", "log", "bak", "old", "dmp", "etl"];

/// The layer switches for one scope.
struct Policy {
    /// Reject `.exe/.dll/…` anywhere under `\windows\`.
    windows_extensions: bool,
    /// Reject drive roots, `<drive>:\users`, and `<profile>\appdata\local|roaming`.
    user_roots: bool,
    /// Apply the shell-folder rules from `PROTECTED_PATHS`.
    shell_folders: bool,
    /// Reject targets outside the cleanup allowlist.
    allowlist: Option<&'static [&'static str]>,
}

fn policy_for(scope: DeleteScope) -> Policy {
    match scope {
        DeleteScope::JunkClean => Policy {
            windows_extensions: true,
            user_roots: true,
            shell_folders: false,
            allowlist: Some(JUNK_ALLOWLIST),
        },
        DeleteScope::UserSelected => Policy {
            windows_extensions: true,
            user_roots: true,
            shell_folders: false,
            allowlist: None,
        },
        // Deep-scan categories include installers under `C:\Windows\Temp`, so
        // the extension layer would refuse targets the scanner deliberately
        // surfaced. The prefix table and the file-name list still apply.
        DeleteScope::EnhancedClean => Policy {
            windows_extensions: false,
            user_roots: true,
            shell_folders: false,
            allowlist: None,
        },
        DeleteScope::Leftovers => Policy {
            windows_extensions: false,
            user_roots: true,
            shell_folders: true,
            allowlist: None,
        },
    }
}

/// Decide whether `path` may be deleted under `scope`.
///
/// This is the single decision point. Engines call it once per target and map
/// the error code to their own per-file result type.
pub fn ensure_deletable(path: &Path, scope: DeleteScope) -> Result<(), AppError> {
    validate_deletable(path)?;

    let key = compare_key(path);
    let policy = policy_for(scope);

    if let Some(rule) = protected_rule(&key, policy.shell_folders) {
        return Err(AppError::new(ErrorCode::Protected)
            .detail(format!("matched protection rule: {rule}"))
            .arg("path", path.to_string_lossy()));
    }

    if let Some(name) = protected_file_name(path) {
        return Err(AppError::new(ErrorCode::Protected)
            .detail(format!("protected file name: {name}"))
            .arg("path", path.to_string_lossy()));
    }

    if policy.windows_extensions {
        if let Some(extension) = protected_windows_extension(path, &key) {
            return Err(AppError::new(ErrorCode::Protected)
                .detail(format!(
                    "protected extension under the Windows directory: {extension}"
                ))
                .arg("path", path.to_string_lossy()));
        }
    }

    if policy.user_roots {
        if let Some(root) = user_root(&key) {
            return Err(AppError::new(ErrorCode::Protected)
                .detail(format!("refusing to delete a {root}"))
                .arg("path", path.to_string_lossy()));
        }
    }

    if let Some(allowlist) = policy.allowlist {
        if !in_allowlist(path, &key, allowlist) {
            return Err(AppError::new(ErrorCode::InvalidPath)
                .detail("path is outside the cleanup scope")
                .arg("path", path.to_string_lossy()));
        }
    }

    Ok(())
}

/// Whether `path` sits under a system cache directory that Windows itself
/// rebuilds, and which is therefore exempt from the prefix rules.
///
/// These directories sit under a protected system root, so only exact subpaths
/// are exempt — never the whole parent directory.
pub fn is_rebuildable_system_cache_path(path: &str) -> bool {
    let normalized = path.replace('/', "\\").to_ascii_lowercase();
    [
        r"\windows\system32\d3d_cache",
        r"\programdata\microsoft\windows defender\localcopy",
        r"\programdata\microsoft\windows defender\support",
        r"\programdata\microsoft\windows defender\scans\history\service",
        // Windows Error Reporting queue and archive directories: hold only crash
        // reports, rebuilt by the system at the next crash. Deep scans hit
        // ReportQueue/ReportArchive, so these must be allowed too.
        r"\programdata\microsoft\windows\wer\reportqueue",
        r"\programdata\microsoft\windows\wer\reportarchive",
    ]
    .iter()
    .any(|marker| {
        let Some(start) = normalized.find(marker) else {
            return false;
        };
        let suffix = &normalized[start + marker.len()..];
        let has_drive_root_boundary = start > 0 && normalized.as_bytes()[start - 1] == b':';
        // Must match both the system drive root and the full directory name, to
        // avoid wrongly allowing nested fake paths or Support2.
        has_drive_root_boundary && (suffix.is_empty() || suffix.starts_with('\\'))
    })
}

// ============================================================================
// Take-Ownership whitelist (moved out of `enhanced_delete` in S3b)
//
// This is deliberately *not* a protection rule. It answers "may we escalate
// privileges on this path", which is the deletion ladder's question, not the
// gate's — a path can be perfectly deletable and still be wrong to take
// ownership of. It lives next to the protection table anyway, for the same
// reason S3a merged those two: two copies of a whitelist is how the protection
// tables drifted apart in the first place, and the reboot gate needs this one.
// ============================================================================

/// Which known root a Take-Ownership whitelist entry is anchored to (M5 fix).
///
/// Before the fix the whitelist matched with `find()` at *any* depth, so a folder
/// merely named `Cache` under user documents (e.g. `C:\Users\X\Documents\Backup\Cache`)
/// qualified for Take Ownership — granting Administrators full control over a path
/// that is not a junk cache at all. Every entry must now declare which known root
/// anchors it, and a match is only accepted at that exact position.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OwnershipAnchor {
    /// Match only directly under a drive root: `X:<entry>…` (the marker starts
    /// right after the drive colon, the same boundary rule as
    /// `is_rebuildable_system_cache_path`).
    DriveRoot,
    /// Match only directly under the user-profile root: `X:\users\<name><entry>…`.
    ProfileRoot,
    /// Match at any component boundary inside the profile's `AppData\Local` subtree.
    ///
    /// Chromium-based browsers place their `Service Worker` / `Media Cache`
    /// directories below vendor/profile directories that vary per browser
    /// (`Google\Chrome`, `Microsoft\Edge`, Brave\…), so the suffix cannot be
    /// spelled out. Restricting the search to this user-owned subtree keeps the
    /// grant out of user documents and other arbitrary locations.
    ProfileAppDataLocal,
}

/// Safe directories where Take Ownership is allowed.
///
/// The "take ownership" operation is only allowed for files under these
/// directories. This prevents accidental deletion of system files. Each entry is
/// `(anchor, marker)`; see `OwnershipAnchor` for why matching is position-bound
/// instead of a plain substring search.
const SAFE_OWNERSHIP_PATHS: &[(OwnershipAnchor, &str)] = &[
    (OwnershipAnchor::DriveRoot, "\\temp"),
    (OwnershipAnchor::DriveRoot, "\\tmp"),
    (OwnershipAnchor::DriveRoot, "\\cache"),
    (OwnershipAnchor::DriveRoot, "\\caches"),
    (OwnershipAnchor::ProfileRoot, "\\appdata\\local\\temp"),
    (
        OwnershipAnchor::ProfileRoot,
        "\\appdata\\local\\microsoft\\windows\\temporary internet files",
    ),
    (
        OwnershipAnchor::ProfileRoot,
        "\\appdata\\local\\microsoft\\windows\\inetcache",
    ),
    (
        OwnershipAnchor::ProfileRoot,
        "\\appdata\\local\\microsoft\\windows\\caches",
    ),
    (
        OwnershipAnchor::ProfileRoot,
        "\\appdata\\local\\microsoft\\windows\\explorer",
    ),
    (
        OwnershipAnchor::ProfileRoot,
        "\\appdata\\local\\microsoft\\windows\\webcache",
    ),
    (
        OwnershipAnchor::ProfileRoot,
        "\\appdata\\local\\microsoft\\windows\\clipboard",
    ),
    // Service Worker / media cache directories of Chromium-based browsers, at the same level as Cache and equally rebuildable.
    // Their location varies by browser (Google\Chrome, Microsoft\Edge, Brave\…),
    // so they are matched at any component boundary **inside the profile's
    // AppData\Local subtree only** — never in arbitrary user directories.
    (OwnershipAnchor::ProfileAppDataLocal, "\\service worker"),
    (OwnershipAnchor::ProfileAppDataLocal, "\\media cache"),
    (OwnershipAnchor::DriveRoot, "\\windows\\temp"),
    // Windows 10/11 system-level temp directories and kernel crash report directories, containing rebuildable diagnostic data.
    (OwnershipAnchor::DriveRoot, "\\windows\\systemtemp"),
    (OwnershipAnchor::DriveRoot, "\\windows\\livekernelreports"),
    (OwnershipAnchor::DriveRoot, "\\windows\\prefetch"),
    (
        OwnershipAnchor::DriveRoot,
        "\\windows\\softwaredistribution\\download",
    ),
    (
        OwnershipAnchor::DriveRoot,
        "\\windows\\softwaredistribution\\deliveryoptimization",
    ),
    (
        OwnershipAnchor::DriveRoot,
        "\\windows\\serviceprofiles\\networkservice\\appdata\\local\\microsoft\\windows\\deliveryoptimization",
    ),
    (
        OwnershipAnchor::DriveRoot,
        "\\windows\\serviceprofiles\\localservice\\appdata\\local\\fontcache",
    ),
    (
        OwnershipAnchor::DriveRoot,
        "\\programdata\\microsoft\\windows defender\\localcopy",
    ),
    (
        OwnershipAnchor::DriveRoot,
        "\\programdata\\microsoft\\windows defender\\support",
    ),
    (
        OwnershipAnchor::DriveRoot,
        "\\programdata\\microsoft\\windows defender\\scans\\history\\service",
    ),
    // Windows error report queues/archives: jointly held by SYSTEM and TrustedInstaller, so elevated deletion must be allowed.
    (
        OwnershipAnchor::DriveRoot,
        "\\programdata\\microsoft\\windows\\wer",
    ),
    (
        OwnershipAnchor::DriveRoot,
        "\\windows\\system32\\d3d_cache",
    ),
    (OwnershipAnchor::ProfileRoot, "\\appdata\\local\\d3dscache"),
    (OwnershipAnchor::ProfileRoot, "\\appdata\\local\\amd\\dxcache"),
    (
        OwnershipAnchor::ProfileRoot,
        "\\appdata\\local\\nvidia\\dxcache",
    ),
    (
        OwnershipAnchor::ProfileRoot,
        "\\appdata\\local\\nvidia\\glcache",
    ),
    (OwnershipAnchor::ProfileRoot, "\\appdata\\local\\nvidia\\dxc"),
    (
        OwnershipAnchor::ProfileRoot,
        "\\appdata\\local\\nvidia\\computecache",
    ),
    (
        OwnershipAnchor::ProfileRoot,
        "\\appdata\\local\\intel\\shadercache",
    ),
    (OwnershipAnchor::DriveRoot, "\\$recycle.bin"),
];

/// Whether taking ownership of `path` is allowed.
///
/// Used twice in the deletion ladder: to decide whether the privilege-escalation
/// rung may run, and — for `RebootPolicy::OwnershipWhitelist` — whether the
/// reboot queue may be used at all.
pub fn is_safe_for_ownership(path: &Path) -> bool {
    // Same normalization as the protection checks: strip the `\\?\` prefix,
    // unify `/` to `\`, then compare case-insensitively (ASCII only: the
    // whitelist and every anchor are pure ASCII).
    let normalized = compare_key(path);

    // Anchor 1: the drive root — the position right after `X:`.
    let drive_root_anchor = drive_root_end(&normalized);

    // Anchor 2: the user-profile root — the position right after `X:\users\<name>`.
    // Only a path *starting* with `X:\users\` counts; a deeper `\users\`
    // component (e.g. `C:\fake\users\victim`) must not forge a profile root.
    let profile_root_anchor = profile_root_end(&normalized);

    // Anchor 3 region: everything after `<profile>\appdata\local`.
    let appdata_local_from = profile_root_anchor
        .filter(|&end| normalized[end..].starts_with("\\appdata\\local\\"))
        .map(|end| end + "\\appdata\\local".len());

    SAFE_OWNERSHIP_PATHS.iter().any(|&(anchor, marker)| {
        // The remainder after the marker must end the path or start a new
        // component, so `\temp` never matches `C:\temporary` or `Support2`.
        let suffix_is_boundary = |start: usize| {
            let suffix = &normalized[start + marker.len()..];
            suffix.is_empty() || suffix.starts_with('\\')
        };
        let starts_with_marker =
            |start: usize| normalized[start..].starts_with(marker) && suffix_is_boundary(start);

        match anchor {
            OwnershipAnchor::DriveRoot => drive_root_anchor.is_some_and(starts_with_marker),
            OwnershipAnchor::ProfileRoot => profile_root_anchor.is_some_and(starts_with_marker),
            // Markers start with `\`, so every occurrence inside the tail sits
            // on a component boundary — only the region itself is constrained.
            OwnershipAnchor::ProfileAppDataLocal => appdata_local_from
                .and_then(|from| normalized[from..].find(marker).map(|offset| from + offset))
                .is_some_and(suffix_is_boundary),
        }
    })
}

/// Position right after the drive colon in a normalized path (`c:` → 2).
pub(crate) fn drive_root_end(normalized: &str) -> Option<usize> {
    let bytes = normalized.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        Some(2)
    } else {
        None
    }
}

/// Position right after the user-profile root `X:\users\<name>` in a normalized
/// path — where a profile-anchored rule must start. `None` when the path does
/// not start with such a root, so a deeper `\users\` component cannot forge one.
pub(crate) fn profile_root_end(normalized: &str) -> Option<usize> {
    let bytes = normalized.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'\\' {
        return None;
    }
    if !normalized[3..].starts_with("users\\") {
        return None;
    }
    let after_users = 3 + "users\\".len();
    let rest = &normalized[after_users..];
    // The profile root itself has no separator after the user name, so fall back
    // to the end of the string — `user_root` needs to recognise `X:\users\alice`
    // as a target it must refuse, not as a path without a profile anchor.
    let username_len = rest.find('\\').unwrap_or(rest.len());
    if username_len == 0 {
        // `X:\users\` with no username is not a profile root
        return None;
    }
    Some(after_users + username_len)
}

/// The path ends there, or continues with a new component.
fn boundary_after(normalized: &str, end: usize) -> bool {
    normalized[end..].is_empty() || normalized[end..].starts_with('\\')
}

fn protected_rule(key: &str, shell_folders: bool) -> Option<&'static str> {
    // System caches the OS rebuilds are exempt from the prefix rules.
    let exempt = is_rebuildable_system_cache_path(key);
    let profile_end = profile_root_end(key);

    for rule in PROTECTED_PATHS {
        let hit = match *rule {
            ProtectedRule::Absolute(prefix) => {
                key.starts_with(prefix) && boundary_after(key, prefix.len())
            }
            ProtectedRule::ProfileSubtree(child) => {
                shell_folders
                    && profile_end.is_some_and(|end| {
                        key[end..].starts_with(child) && boundary_after(key, end + child.len())
                    })
            }
            ProtectedRule::ProfileExact(child) => {
                profile_end.is_some_and(|end| key[end..] == *child)
            }
        };

        if hit && !exempt {
            return Some(rule.label());
        }
    }

    None
}

fn protected_file_name(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    PROTECTED_FILES
        .iter()
        .find(|protected| name == **protected)
        .map(|_| name)
}

fn protected_windows_extension(path: &Path, key: &str) -> Option<String> {
    if !key.contains(r"\windows\") {
        return None;
    }
    let extension = path.extension()?.to_string_lossy().to_ascii_lowercase();
    PROTECTED_EXTENSIONS_IN_WINDOWS
        .contains(&extension.as_str())
        .then_some(extension)
}

/// Drive roots, `<drive>:\users`, and the profile's `AppData` containers.
fn user_root(key: &str) -> Option<&'static str> {
    if key.len() == 2 && key.ends_with(':') {
        return Some("drive root");
    }
    if key.len() == 3 && key.ends_with('\\') {
        return Some("drive root");
    }
    if profile_root_end(key) == Some(key.len()) {
        return Some("user profile root");
    }
    if key.len() > 7 && key.ends_with(r"\users") && drive_root_end(key).is_some() {
        return Some("user profiles container");
    }
    if key.ends_with(r"\appdata\local") || key.ends_with(r"\appdata\roaming") {
        return Some("AppData root");
    }
    None
}

fn in_allowlist(path: &Path, key: &str, allowlist: &[&str]) -> bool {
    if allowlist.iter().any(|marker| key.contains(marker)) {
        return true;
    }
    path.extension()
        .map(|extension| {
            let extension = extension.to_string_lossy().to_ascii_lowercase();
            JUNK_ALLOWLIST_EXTENSIONS.contains(&extension.as_str())
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    const ALL_SCOPES: [DeleteScope; 4] = [
        DeleteScope::JunkClean,
        DeleteScope::EnhancedClean,
        DeleteScope::Leftovers,
        DeleteScope::UserSelected,
    ];

    fn code(path: &str, scope: DeleteScope) -> Option<ErrorCode> {
        ensure_deletable(Path::new(path), scope)
            .err()
            .map(|e| e.code)
    }

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

    #[test]
    fn rejection_carries_the_invalid_path_code() {
        // The command layer re-validates through this function and forwards the error
        // over IPC, so the code has to be machine-readable — the UI branches on it.
        let error =
            ensure_deletable(Path::new(r"C:\Temp\..\Windows"), DeleteScope::JunkClean).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidPath);
        assert!(
            error.detail.is_some(),
            "detail must explain why it was refused"
        );

        let empty = ensure_deletable(Path::new(""), DeleteScope::JunkClean).unwrap_err();
        assert_eq!(empty.code, ErrorCode::InvalidPath);
    }

    #[test]
    fn every_scope_refuses_the_system_table() {
        // The whole point of S3a: the table is global. `C:\Windows.old` used to be
        // protected against junk deletion but not against permanent deletion.
        for scope in ALL_SCOPES {
            for path in [
                r"C:\Windows.old\Windows\System32\old.dll",
                r"C:\Windows\System32\config\SAM",
                r"C:\Windows\WinSxS\backup\file.dll",
                r"C:\Program Files\App\app.exe",
                r"C:\Program Files (x86)\App\app.exe",
                r"C:\ProgramData\Microsoft\Windows\SystemData\x",
                r"C:\ProgramData\Microsoft\Windows Defender\Quarantine\x",
                r"C:\Recovery\WindowsRE\winre.wim",
                r"C:\Boot\BCD",
                r"C:\System Volume Information\tracking.log",
                r"C:\Users\Default\NTUSER.DAT",
                r"C:\Users\Public\Desktop\shortcut.lnk",
            ] {
                assert_eq!(
                    code(path, scope),
                    Some(ErrorCode::Protected),
                    "{path} must be protected under {scope:?}"
                );
            }
        }
    }

    #[test]
    fn prefix_rules_stop_at_a_component_boundary() {
        // A folder merely *named* like a system directory is not one.
        assert_ne!(
            code(r"C:\Program Filesx\app.exe", DeleteScope::Leftovers),
            Some(ErrorCode::Protected)
        );
        assert_ne!(
            code(r"C:\Windows\System32x\file.dll", DeleteScope::Leftovers),
            Some(ErrorCode::Protected)
        );
        // ...but the real directory and everything below it is.
        assert_eq!(
            code(r"C:\Program Files\app.exe", DeleteScope::Leftovers),
            Some(ErrorCode::Protected)
        );
    }

    #[test]
    fn rebuildable_system_caches_stay_deletable() {
        // Explicitly exempted subpaths must still pass under every scope, otherwise
        // the scanner would list files the engine then refuses to delete.
        for scope in ALL_SCOPES {
            for path in [
                r"C:\Windows\System32\d3d_cache\shader.bin",
                r"C:\ProgramData\Microsoft\Windows Defender\Support\MPLog.log",
                r"C:\ProgramData\Microsoft\Windows\WER\ReportQueue\report.wer",
            ] {
                assert_ne!(
                    code(path, scope),
                    Some(ErrorCode::Protected),
                    "{path} should be exempt under {scope:?}"
                );
            }
        }
        // Spoofed names are not exempt.
        assert_eq!(
            code(
                r"C:\ProgramData\Microsoft\Windows Defender\Support2\MPLog.log",
                DeleteScope::Leftovers
            ),
            Some(ErrorCode::Protected)
        );
    }

    #[test]
    fn shell_folders_are_protected_only_for_permanent_delete() {
        // Large-file cleanup deletes user-chosen files from the Desktop on purpose,
        // so the shell-folder rules must not leak into that scope.
        for path in [
            r"C:\Users\Alice\Documents\report.docx",
            r"C:\Users\Alice\Desktop\movie.iso",
            r"C:\Users\Alice\Downloads\setup.exe",
        ] {
            assert_eq!(
                code(path, DeleteScope::Leftovers),
                Some(ErrorCode::Protected),
                "{path} must be protected for permanent delete"
            );
            assert_ne!(
                code(path, DeleteScope::UserSelected),
                Some(ErrorCode::Protected),
                "{path} must stay deletable when the user picked it"
            );
        }
    }

    #[test]
    fn profile_rules_are_anchored_to_a_real_profile_root() {
        // A deeper `\users\` component must not forge a profile root.
        assert_ne!(
            code(
                r"C:\Backup\users\alice\documents\x.docx",
                DeleteScope::Leftovers
            ),
            Some(ErrorCode::Protected)
        );
        assert_eq!(
            code(r"C:\Users\Alice\Documents\x.docx", DeleteScope::Leftovers),
            Some(ErrorCode::Protected)
        );
    }

    #[test]
    fn appdata_containers_are_protected_but_their_contents_are_not() {
        for scope in ALL_SCOPES {
            assert_eq!(
                code(r"C:\Users\Alice\AppData\Local", scope),
                Some(ErrorCode::Protected),
                "the container itself must be refused"
            );
        }
        // Everything the app actually cleans lives below this point.
        assert_ne!(
            code(
                r"C:\Users\Alice\AppData\Local\Temp\x.tmp",
                DeleteScope::JunkClean
            ),
            Some(ErrorCode::Protected)
        );
    }

    #[test]
    fn drive_and_profile_roots_are_refused() {
        for scope in ALL_SCOPES {
            assert_eq!(code(r"C:\", scope), Some(ErrorCode::Protected));
            assert_eq!(code(r"C:", scope), Some(ErrorCode::Protected));
            assert_eq!(
                code(r"C:\Users\Alice", scope),
                Some(ErrorCode::Protected),
                "the profile root itself is never a target"
            );
        }
    }

    #[test]
    fn windows_binaries_are_refused_where_the_extension_layer_applies() {
        // Junk clean and user-picked deletion refuse binaries under `\windows\`;
        // the deep scanner legitimately surfaces installers in `C:\Windows\Temp`.
        assert_eq!(
            code(r"C:\Windows\notepad.exe", DeleteScope::JunkClean),
            Some(ErrorCode::Protected)
        );
        assert_eq!(
            code(r"C:\Windows\Temp\setup.msi", DeleteScope::JunkClean),
            Some(ErrorCode::Protected)
        );
        assert_ne!(
            code(r"C:\Windows\Temp\setup.msi", DeleteScope::EnhancedClean),
            Some(ErrorCode::Protected),
            "deep scan must be able to delete installers it classified as junk"
        );
    }

    #[test]
    fn junk_clean_refuses_targets_outside_the_allowlist() {
        assert_eq!(
            code(
                r"C:\Users\Alice\Documents\report.docx",
                DeleteScope::JunkClean
            ),
            Some(ErrorCode::InvalidPath)
        );
        // In scope: a cache marker anywhere on the path.
        assert_eq!(
            code(
                r"C:\Users\Alice\AppData\Local\Chrome\User Data\Default\Cache\f_000001",
                DeleteScope::JunkClean
            ),
            None
        );
        // In scope: an allowed extension anywhere on the path.
        assert_eq!(
            code(r"C:\Windows\Temp\install.log", DeleteScope::JunkClean),
            None
        );
        // Out of scope, but not protected — the engine reports it as a scope refusal.
        assert_eq!(
            code(r"C:\Users\Alice\Videos\movie.iso", DeleteScope::JunkClean),
            Some(ErrorCode::InvalidPath)
        );
    }

    #[test]
    fn protected_file_names_apply_to_every_scope() {
        for scope in ALL_SCOPES {
            assert_eq!(
                code(r"C:\Temp\ntoskrnl.exe", scope),
                Some(ErrorCode::Protected)
            );
            assert_eq!(
                code(r"C:\Temp\WeChat Files\msg.db", scope),
                Some(ErrorCode::Protected)
            );
        }
    }

    #[test]
    fn extended_and_mixed_spellings_do_not_bypass_the_table() {
        for path in [
            r"\\?\C:\Windows\System32\config\SAM",
            "C:/Windows/System32/config/SAM",
            r"\\?\C:/Windows.old/x.dll",
        ] {
            for scope in ALL_SCOPES {
                assert_eq!(
                    code(path, scope),
                    Some(ErrorCode::Protected),
                    "{path} must not bypass the table under {scope:?}"
                );
            }
        }
    }

    // ------------------------------------------------------------------
    // Take-Ownership whitelist. Moved here from `enhanced_delete` in S3b,
    // together with the table itself.
    // ------------------------------------------------------------------

    #[test]
    fn ownership_whitelist_covers_the_deep_scan_cache_directories() {
        assert!(is_safe_for_ownership(Path::new(
            "C:\\Windows\\Temp\\test.tmp"
        )));
        assert!(is_safe_for_ownership(Path::new(
            "C:\\Users\\Test\\AppData\\Local\\Temp\\file.log"
        )));
        assert!(is_safe_for_ownership(Path::new(
            "C:\\ProgramData\\Microsoft\\Windows Defender\\Support\\MPLog.log"
        )));
        assert!(!is_safe_for_ownership(Path::new(
            "C:\\ProgramData\\Microsoft\\Windows Defender\\Support2\\MPLog.log"
        )));
        assert!(!is_safe_for_ownership(Path::new(
            "C:\\Windows\\System32\\test.dll"
        )));
        // Cache directories added by deep scanning (Defender scan history, font
        // cache, clipboard, NVIDIA shader variants) must also allow safe elevated
        // deletion, otherwise they are found but can never be deleted.
        assert!(is_safe_for_ownership(Path::new(
            "C:\\ProgramData\\Microsoft\\Windows Defender\\Scans\\History\\Service\\entry.bin"
        )));
        assert!(is_safe_for_ownership(Path::new(
            "C:\\Windows\\ServiceProfiles\\LocalService\\AppData\\Local\\FontCache\\font.bin"
        )));
        assert!(is_safe_for_ownership(Path::new(
            "C:\\Users\\Test\\AppData\\Local\\Microsoft\\Windows\\Clipboard\\entry.bin"
        )));
        assert!(is_safe_for_ownership(Path::new(
            "C:\\Users\\Test\\AppData\\Local\\NVIDIA\\GLCache\\shader.bin"
        )));
    }

    /// M5 regression: a whitelist entry must no longer match at arbitrary depth.
    /// Before the fix, a folder merely named `Cache` under user documents or any
    /// other location qualified for Take Ownership (Administrators: F).
    #[test]
    fn ownership_whitelist_matches_only_at_known_roots() {
        // Arbitrary-depth hits must be rejected:
        assert!(!is_safe_for_ownership(Path::new(
            "C:\\Users\\Test\\Documents\\Backup\\Cache\\thesis.doc"
        )));
        assert!(!is_safe_for_ownership(Path::new(
            "C:\\Games\\cache\\save.dat"
        )));
        assert!(!is_safe_for_ownership(Path::new(
            "C:\\Users\\Test\\Documents\\service worker\\index"
        )));
        // A `\users\` component that is not at the drive root must not forge a profile root:
        assert!(!is_safe_for_ownership(Path::new(
            "C:\\fake\\users\\victim\\AppData\\Local\\Temp\\x.tmp"
        )));
        // Similarly named directories stay rejected (suffix boundary):
        assert!(!is_safe_for_ownership(Path::new("C:\\temporary\\x.tmp")));

        // Legit targets keep working, including every bypass spelling:
        assert!(is_safe_for_ownership(Path::new("C:\\Temp\\x.tmp")));
        assert!(is_safe_for_ownership(Path::new("C:/Windows/Temp/test.tmp")));
        assert!(is_safe_for_ownership(Path::new(
            r"\\?\C:\Windows\Temp\test.tmp"
        )));
        assert!(is_safe_for_ownership(Path::new(
            "C:/Users/Test/AppData/Local/Temp/file.log"
        )));
        assert!(is_safe_for_ownership(Path::new(
            "C:\\Users\\Test\\AppData\\Local\\Google\\Chrome\\User Data\\Default\\Service Worker\\CacheStorage\\abc"
        )));
    }
}
