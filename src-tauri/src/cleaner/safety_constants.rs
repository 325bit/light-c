// ============================================================================
// Deletion safety constants (shared by delete_engine and enhanced_delete)
// Avoid two engines maintaining inconsistent protection lists
// ============================================================================

/// Path prefixes absolutely forbidden from deletion (lowercase, starts_with match)
pub const PROTECTED_PATH_PREFIXES: &[&str] = &[
    "c:\\windows\\system32",
    "c:\\windows\\syswow64",
    "c:\\windows\\winsxs",
    "c:\\windows\\servicing",
    "c:\\windows\\assembly",
    "c:\\windows\\boot",
    "c:\\windows\\fonts",
    "c:\\windows\\inf",
    "c:\\windows\\microsoft.net",
    "c:\\windows\\security",
    "c:\\program files",
    "c:\\program files (x86)",
    "c:\\users\\default",
    "c:\\users\\public\\desktop",
    "c:\\programdata\\microsoft\\windows",
    "c:\\programdata\\microsoft\\windows defender",
    "c:\\recovery",
    "c:\\$recycle.bin",
    // Old Windows installations (kept about 10 days after a system upgrade for rollback).
    // These directories are system-owned with inconsistent internal permissions: a direct remove_dir_all fails midway and leaves
    // a "half-deleted" state, and only Disk Cleanup / Storage Sense are supported entry points for cleaning them.
    // The frontend already marks this category as system-managed with no delete option; this is a second gate, in case another path bypasses it.
    "c:\\windows.old",
    "c:\\$windows.~bt",
    "c:\\$windows.~ws",
];

/// File names absolutely forbidden from deletion (lowercase, exact match)
pub const PROTECTED_FILES: &[&str] = &[
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

/// Extensions forbidden from deletion under the Windows directory (lowercase)
pub const PROTECTED_EXTENSIONS_IN_WINDOWS: &[&str] = &[
    "sys", "dll", "exe", "drv", "ocx", "cpl", "msi", "msp", "msu", "cat", "mum", "manifest",
];

/// Determines whether this is a system cache subdirectory explicitly allowed for rebuild by the Windows Cleanup Wizard.
/// These directories sit under a protected system root, so the delete engine must allow only exact subpaths, not loosen the whole parent directory.
pub fn is_rebuildable_system_cache_path(path: &str) -> bool {
    let normalized = path.replace('/', "\\").to_ascii_lowercase();
    [
        "\\windows\\system32\\d3d_cache",
        "\\programdata\\microsoft\\windows defender\\localcopy",
        "\\programdata\\microsoft\\windows defender\\support",
        "\\programdata\\microsoft\\windows defender\\scans\\history\\service",
        // Windows Error Reporting queue and archive directories: hold only crash reports, rebuilt by the system at the next crash.
        // Deep scans hit ReportQueue/ReportArchive, so these must be allowed too, otherwise they would be found but undeletable.
        "\\programdata\\microsoft\\windows\\wer\\reportqueue",
        "\\programdata\\microsoft\\windows\\wer\\reportarchive",
    ]
    .iter()
    .any(|marker| {
        let Some(start) = normalized.find(marker) else {
            return false;
        };
        let suffix = &normalized[start + marker.len()..];
        let has_drive_root_boundary = start > 0 && normalized.as_bytes()[start - 1] == b':';
        // Must match both the system drive root and the full directory name, to avoid wrongly allowing nested fake paths or Support2.
        has_drive_root_boundary && (suffix.is_empty() || suffix.starts_with('\\'))
    })
}

#[cfg(test)]
mod tests {
    use super::is_rebuildable_system_cache_path;

    #[test]
    fn matches_only_explicit_cache_directory_boundaries() {
        // Only allow directories explicitly listed by the cleanup wizard, to avoid wrongly allowing similarly named directories.
        assert!(is_rebuildable_system_cache_path(
            r"C:\ProgramData\Microsoft\Windows Defender\Support\MPLog.log"
        ));
        assert!(is_rebuildable_system_cache_path(
            r"C:\ProgramData\Microsoft\Windows Defender\Scans\History\Service\DetectionHistory\entry.bin"
        ));
        assert!(!is_rebuildable_system_cache_path(
            r"C:\ProgramData\Microsoft\Windows Defender\Support2\MPLog.log"
        ));
        // Spoofed directory names must not be allowed for deletion.
        assert!(!is_rebuildable_system_cache_path(
            r"C:\ProgramData\Microsoft\Windows Defender\Scans\History\ServiceEvil\entry.bin"
        ));
        assert!(!is_rebuildable_system_cache_path(
            r"C:\ProgramData\Microsoft\Windows Defender\Quarantine\entry.bin"
        ));
        assert!(!is_rebuildable_system_cache_path(
            r"C:\Temp\ProgramData\Microsoft\Windows Defender\Support\entry.bin"
        ));
    }
}
