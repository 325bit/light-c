// ============================================================================
// Social software path adapters
// ============================================================================
//
// Each adapter is only responsible for discovering a piece of software's data directory; the actual traversal and classification is handled uniformly by core.rs.
// This avoids duplicating the scan, deduplication, and risk-grading logic when enhancing a single software's paths.
// ============================================================================

use super::core::{FileCategory, SocialAppPath, SocialScanner};
use log::info;
use std::path::PathBuf;
use winreg::enums::HKEY_CURRENT_USER;
use winreg::RegKey;

impl SocialScanner {
    // Telegram path detection
    // ========================================================================

    // ------------------------------------------------------------------------
    // Install location from the registry
    // ------------------------------------------------------------------------

    /// Reads Telegram's install directory from the registry.
    ///
    /// Why needed: Telegram Desktop puts the exe and tdata in the same directory,
    /// installing to `%APPDATA%\Telegram Desktop` is only the **default** — when the user installs to another drive
    /// there's no trace in the filesystem, only the registry still remembers (local test: `D:\tmp\Telegram Desktop`).
    fn telegram_dirs_from_registry() -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);

        // ① tg:// protocol handler, default value looks like "\"D:\tmp\Telegram Desktop\Telegram.exe\" -- \"%1\""
        if let Ok(key) = hkcu.open_subkey(r"Software\Classes\tg\shell\open\command") {
            if let Ok(command) = key.get_value::<String, _>("") {
                if let Some(dir) = Self::exe_dir_from_command(&command) {
                    dirs.push(dir);
                }
            }
        }

        // ② InstallLocation in the uninstall entry is directly the directory, more reliable than parsing the command line
        if let Ok(uninstall) =
            hkcu.open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Uninstall")
        {
            for name in uninstall.enum_keys().filter_map(|key| key.ok()) {
                let Ok(entry) = uninstall.open_subkey(&name) else {
                    continue;
                };
                let display: String = entry.get_value("DisplayName").unwrap_or_default();
                // Registry DisplayName, not a path (L4).
                if !display.to_lowercase().contains("telegram") {
                    continue;
                }
                if let Ok(location) = entry.get_value::<String, _>("InstallLocation") {
                    if !location.is_empty() {
                        dirs.push(PathBuf::from(location.trim_end_matches('\\')));
                    }
                }
            }
        }

        // The tg protocol and uninstall entry usually point to the same directory; deduplicate before returning,
        // so the upper layer doesn't pay for "scanning the same path twice".
        dirs.sort();
        dirs.dedup();
        dirs
    }

    /// Extracts the exe's containing directory from a command line: both quoted and unquoted forms must be recognized.
    fn exe_dir_from_command(command: &str) -> Option<PathBuf> {
        let exe = match command.strip_prefix('"') {
            Some(rest) => rest.split('"').next()?,
            None => command.split_whitespace().next()?,
        };
        PathBuf::from(exe).parent().map(|dir| dir.to_path_buf())
    }

    /// Detects Telegram paths
    pub(super) fn detect_telegram_paths(&self) -> Option<Vec<SocialAppPath>> {
        let mut paths = Vec::new();

        // Default install is %APPDATA%\Telegram Desktop, but users can install to any drive
        // (local test: D:\tmp\Telegram Desktop); only the registry remembers such locations.
        let mut bases = vec![PathBuf::from(&self.appdata).join("Telegram Desktop")];
        for dir in Self::telegram_dirs_from_registry() {
            if !bases.iter().any(|existing| existing == &dir) {
                bases.push(dir);
            }
        }

        for telegram_base in bases {
            if !telegram_base.exists() {
                continue;
            }
            info!("发现Telegram目录: {}", telegram_base.display());

            // tdata directory (user data, contains the message database)
            let tdata = telegram_base.join("tdata");
            if !tdata.exists() {
                continue;
            }

            // Determine whether this is really account data.
            // ⚠️ Don't look only at .db: new Telegram's tdata contains directories like key_datas + D877F783D5D3EF8C,
            // with no .db at all (local test); looking only at .db would skip the whole new version.
            let has_account = std::fs::read_dir(&tdata)
                .map(|entries| {
                    entries.filter_map(|e| e.ok()).any(|e| {
                        let name = e.file_name().to_string_lossy().to_ascii_lowercase();
                        name.ends_with(".db")
                            || name.contains("cache")
                            || name == "key_datas"
                            || name == "user_data"
                    })
                })
                .unwrap_or(false);

            if !has_account {
                continue;
            }

            paths.push(SocialAppPath {
                app_name: "Telegram".to_string(),
                path: tdata.clone(),
                category: FileCategory::ChatDatabase,
                is_custom_path: false,
            });

            // user_data directory
            let user_data = tdata.join("user_data");
            if user_data.exists() {
                paths.push(SocialAppPath {
                    app_name: "Telegram".to_string(),
                    path: user_data,
                    category: FileCategory::TempCache,
                    is_custom_path: false,
                });
            }
        }

        // The Microsoft Store version doesn't use Roaming\Telegram Desktop; check the package directory's local state.
        for package in self.find_package_directories(&["telegramdesktop", "telegram"]) {
            self.add_named_subdirectories(
                &package.join("LocalCache"),
                &["Cache", "Temp", "Code Cache"],
                "Telegram",
                FileCategory::TempCache,
                false,
                &mut paths,
            );
            self.add_named_subdirectories(
                &package.join(r"LocalCache\Roaming\Telegram Desktop"),
                &["Cache", "Temp", "Code Cache"],
                "Telegram",
                FileCategory::TempCache,
                false,
                &mut paths,
            );
            Self::add_scan_path(
                &mut paths,
                "Telegram",
                package.join("LocalState").join("tdata"),
                FileCategory::ChatDatabase,
                false,
            );
        }

        if paths.is_empty() {
            None
        } else {
            Some(paths)
        }
    }
}

// ========================================================================

#[cfg(test)]
mod tests {
    use super::SocialScanner;
    use std::path::PathBuf;

    /// The registry's tg protocol command has two forms: quoted (path contains spaces) and unquoted.
    /// Parse it wrong and you can't get the install directory, and Telegram becomes "unscannable" again.

    #[test]
    fn exe_dir_from_command_handles_quoted_and_bare_paths() {
        let quoted = r#""D:\tmp\Telegram Desktop\Telegram.exe" -- "%1""#;
        assert_eq!(
            SocialScanner::exe_dir_from_command(quoted),
            Some(PathBuf::from(r"D:\tmp\Telegram Desktop"))
        );

        let bare = r"C:\Apps\Telegram.exe -- %1";
        assert_eq!(
            SocialScanner::exe_dir_from_command(bare),
            Some(PathBuf::from(r"C:\Apps"))
        );

        // An empty string must not panic
        assert_eq!(SocialScanner::exe_dir_from_command(""), None);
    }
}
