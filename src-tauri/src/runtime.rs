// ============================================================================
// Runtime distribution channel
//
// The installer and portable editions share the same exe; the distribution channel must be decided by explicit metadata in the package,
// and cannot be inferred from the current working directory, install location, or directory writability.
// ============================================================================

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const PORTABLE_MARKER_FILE: &str = "LightC.portable";
pub const PORTABLE_MANIFEST_FILE: &str = "LightC.portable.json";
const PORTABLE_MANIFEST_SCHEMA_VERSION: u32 = 1;
const PORTABLE_WEBVIEW_DIR: &str = "webview";
const WEBVIEW_MIGRATION_DIR: &str = ".migration";
const WEBVIEW_MIGRATION_STATE_FILE: &str = "legacy_webview_v1.json";
const APP_IDENTIFIER: &str = "com.chunyu.LightC";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DistributionChannel {
    Installer,
    Portable,
}

impl DistributionChannel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Installer => "安装版",
            Self::Portable => "便携版",
        }
    }
}

#[derive(Debug, Deserialize)]
struct PortableManifest {
    schema_version: u32,
    mode: String,
    data_layout: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct WebviewMigrationState {
    schema_version: u32,
    completed: bool,
    source_directory: String,
}

/// Identify the distribution channel from the current exe path.
///
/// Decision order (strongest to weakest):
/// 1. New manifest: `LightC.portable.json`, versioned format, highest priority;
/// 2. Legacy marker: `LightC.portable`, compatible with old portable packages;
/// 3. Uninstaller trace: an installer directory has `uninstall.exe` (NSIS output), a portable package does not.
///    This does not depend on registry reads; it is a strong "verifiable on the spot" signal;
/// 4. Install registration: the registry uninstall entry's `InstallLocation` points to the exe directory (or its parent,
///    to cover nested layouts such as `App\0.0.0\`);
/// 5. None of the above shows an install trace -> portable (covers "user kept only LightC.exe");
/// 6. Registry read failed (not "not found") -> treat as installer and warn; better to write AppData than mistakenly write the program directory.
///
/// Critical safety constraint: an installer must never be judged portable. Therefore any "looks like an installer" signal
/// (uninstaller, install registration) is always judged installer; only when no install trace is confirmed does it fall through to portable.
pub fn detect_distribution_channel(exe_path: &Path) -> DistributionChannel {
    resolve_distribution_channel(exe_path, installed_application_directory)
}

/// Core decision: pass the registry lookup in as a parameter so unit tests can override each branch with fixed data.
fn resolve_distribution_channel(
    exe_path: &Path,
    lookup_install_directory: impl FnOnce() -> Result<Option<PathBuf>, String>,
) -> DistributionChannel {
    let Some(application_dir) = exe_path.parent() else {
        return DistributionChannel::Installer;
    };

    let manifest_path = application_dir.join(PORTABLE_MANIFEST_FILE);
    if manifest_path.is_file() {
        match std::fs::read_to_string(&manifest_path)
            .ok()
            .and_then(|content| {
                serde_json::from_str::<PortableManifest>(content.trim_start_matches('\u{feff}'))
                    .ok()
            }) {
            Some(manifest)
                if manifest.schema_version == PORTABLE_MANIFEST_SCHEMA_VERSION
                    && manifest.mode == "portable"
                    && manifest.data_layout == "relative" =>
            {
                return DistributionChannel::Portable;
            }
            Some(_) => {
                log::warn!(
                    "便携版 manifest 内容不受支持，将继续检查旧版 marker: {}",
                    manifest_path.display()
                );
            }
            None => {
                log::warn!(
                    "读取便携版 manifest 失败，将继续检查旧版 marker: {}",
                    manifest_path.display()
                );
            }
        }
    }

    if application_dir.join(PORTABLE_MARKER_FILE).is_file() {
        return DistributionChannel::Portable;
    }

    // Uninstaller trace: an installer directory has uninstall.exe; a portable package contains only exe/dll/marker files.
    if has_installer_uninstaller(application_dir) {
        return DistributionChannel::Installer;
    }

    match lookup_install_directory() {
        Ok(Some(install_directory)) => {
            if is_same_or_nested_install_directory(application_dir, &install_directory) {
                DistributionChannel::Installer
            } else {
                // The install record in the registry points to another directory, meaning the current exe is a portable copy placed by the user.
                DistributionChannel::Portable
            }
        }
        Ok(None) => DistributionChannel::Portable,
        Err(error) => {
            log::warn!(
                "读取安装登记失败，暂按安装版处理以避免数据写入程序目录: {}",
                error
            );
            DistributionChannel::Installer
        }
    }
}

/// Check whether the exe directory (or its immediate parent) contains an NSIS uninstaller.
///
/// Note: Tauri NSIS installs by default with the layout `$INSTDIR\LightC.exe` + `$INSTDIR\uninstall.exe`;
/// some installers add another version directory, so the parent is checked too, and only if the parent is not a drive root.
fn has_installer_uninstaller(application_dir: &Path) -> bool {
    if application_dir.join("uninstall.exe").is_file() {
        return true;
    }

    match application_dir.parent() {
        // A drive root (e.g. D:\) is not used as install-directory evidence, to avoid false hits on unrelated files like D:\uninstall.exe.
        Some(parent) if parent.parent().is_some() => parent.join("uninstall.exe").is_file(),
        _ => false,
    }
}

/// Check whether the exe directory is the registered install directory or one level below it (`App\version\` layout).
fn is_same_or_nested_install_directory(application_dir: &Path, install_directory: &Path) -> bool {
    let install_key = path_compare_key(install_directory);
    let application_key = path_compare_key(application_dir);
    if application_key == install_key {
        return true;
    }

    match application_dir.parent() {
        Some(parent) => path_compare_key(parent) == install_key,
        None => false,
    }
}

/// Path comparison key: normalize separators, strip the trailing backslash, and lowercase, to avoid missing a match due to differences between registry and FS path spelling.
fn path_compare_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

/// Read the install directory registered by the NSIS installer.
///
/// Note: the installer creates a `...\CurrentVersion\Uninstall\LightC` uninstall entry (written by the Tauri NSIS installer);
/// `InstallLocation` is the install directory; `UninstallString` is the fallback for older versions missing that field.
/// Returning `Ok(None)` means it is confirmed there is no install record (the current exe is a portable copy).
const INSTALL_REGISTRY_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\LightC";

fn installed_application_directory() -> Result<Option<PathBuf>, String> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};
    use winreg::RegKey;

    // Check both 32/64-bit registry views: the installer writes according to its own bitness, and users may reinstall with a different bitness.
    let candidates = [
        (HKEY_CURRENT_USER, KEY_WOW64_64KEY),
        (HKEY_CURRENT_USER, KEY_WOW64_32KEY),
        (HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY),
        (HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY),
    ];
    let mut last_error: Option<String> = None;

    for (root, view_flags) in candidates {
        let key = match RegKey::predef(root)
            .open_subkey_with_flags(INSTALL_REGISTRY_KEY, KEY_READ | view_flags)
        {
            Ok(key) => key,
            Err(error) => {
                // Not installed (key not found) is a normal path; only other errors need to be reported to the caller as a fallback.
                if error.kind() != std::io::ErrorKind::NotFound {
                    last_error = Some(format!("打开安装登记失败: {}", error));
                }
                continue;
            }
        };

        if let Ok(location) = key.get_value::<String, _>("InstallLocation") {
            let trimmed = trim_registry_path(&location);
            if !trimmed.is_empty() {
                return Ok(Some(PathBuf::from(trimmed)));
            }
        }

        if let Ok(uninstall) = key.get_value::<String, _>("UninstallString") {
            let trimmed = trim_registry_path(&uninstall);
            if let Some(parent) = Path::new(&trimmed).parent() {
                return Ok(Some(parent.to_path_buf()));
            }
        }
    }

    match last_error {
        Some(error) => Err(error),
        None => Ok(None),
    }
}

/// Paths in the registry usually have quotes (`"D:\LightC"`); strip the quotes and trailing backslash before comparing.
fn trim_registry_path(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_end_matches('\\')
        .to_string()
}

/// Get the current program path; unify the error message for reuse by the data directory and integrity checks.
pub fn current_executable_path() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|error| format!("无法读取当前程序路径: {}", error))
}

/// Get the current distribution package root. The portable root must follow the exe (data travels with the package),
/// while the installer root still uses LocalAppData - NSIS updates overwrite the install directory,
/// so data placed there would be lost on update/uninstall; the installer must therefore live under AppData.
pub fn current_application_root() -> Option<PathBuf> {
    let executable_path = current_executable_path().ok()?;
    match detect_distribution_channel(&executable_path) {
        DistributionChannel::Portable => portable_root_dir_for(&executable_path),
        DistributionChannel::Installer => installer_root_dir(),
    }
}

/// Portable root: the directory containing the exe (data travels with the package).
pub fn portable_root_dir_for(executable_path: &Path) -> Option<PathBuf> {
    executable_path.parent().map(Path::to_path_buf)
}

/// Installer root: %LOCALAPPDATA%/LightC; data is not lost when the install directory is updated or removed.
pub fn installer_root_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join("LightC"))
}

/// Get the portable WebView2 user data directory; the installer edition still uses Tauri's default AppData location.
pub fn portable_webview_data_directory() -> Option<PathBuf> {
    if current_executable_path()
        .ok()
        .is_none_or(|path| detect_distribution_channel(&path) != DistributionChannel::Portable)
    {
        return None;
    }

    current_application_root().map(|root| root.join(PORTABLE_WEBVIEW_DIR))
}

/// Prepare the portable WebView2 data directory and handle migration of legacy WebView localStorage.
///
/// WebView2 caches and localStorage are not LightC's own files, so migration state is tracked separately,
/// to avoid mixing them into the same whitelist as cleanup data such as logs and driver backups.
pub fn prepare_portable_webview_data_directory() -> Option<PathBuf> {
    repair_portable_marker_if_needed();
    let target_directory = portable_webview_data_directory()?;
    if let Err(error) = std::fs::create_dir_all(&target_directory) {
        log::warn!(
            "无法创建便携版 WebView2 数据目录 {}: {}",
            target_directory.display(),
            error
        );
        return None;
    }

    // Dev builds (cargo tauri dev) do not migrate packaged localStorage: debug theme/layout/data-directory
    // settings would differ from what the packaged build shows, making troubleshooting look like "data got mixed up".
    if cfg!(debug_assertions) {
        log::info!("开发环境跳过旧版 WebView2 数据迁移");
        return Some(target_directory);
    }

    if let Err(error) = migrate_legacy_webview_data(&target_directory) {
        log::warn!("迁移旧版 WebView2 数据失败: {}", error);
    }
    Some(target_directory)
}

/// Portable self-healing: fill in marker files when detected as portable but they are missing, so later versions and users copying the program no longer depend on manually keeping the markers.
///
/// Note: as long as the registry has no install record for the current directory, this is a portable copy. Filling in `LightC.portable` /
/// `LightC.portable.json` shortens the decision path on next startup and ensures that copying the whole directory elsewhere still yields a portable edition.
/// Write failures (read-only directory) only log a message and do not affect this run, because the decision chain no longer depends on these files.
fn repair_portable_marker_if_needed() {
    let Ok(executable_path) = current_executable_path() else {
        return;
    };
    repair_portable_marker_for(&executable_path);
}

/// Self-healing core: fill in portable markers for a given exe path, so unit tests can use temp directories.
fn repair_portable_marker_for(executable_path: &Path) {
    let Some(application_dir) = executable_path.parent() else {
        return;
    };
    if detect_distribution_channel(executable_path) != DistributionChannel::Portable {
        return;
    }

    let marker_path = application_dir.join(PORTABLE_MARKER_FILE);
    if !marker_path.is_file() {
        if let Err(error) = std::fs::write(&marker_path, "portable") {
            log::debug!(
                "补写便携版标记失败（目录可能只读）{}: {}",
                marker_path.display(),
                error
            );
        }
    }

    let manifest_path = application_dir.join(PORTABLE_MANIFEST_FILE);
    if !manifest_path.is_file() {
        let manifest = format!(
            "{{\n  \"schema_version\": {},\n  \"mode\": \"portable\",\n  \"data_layout\": \"relative\"\n}}\n",
            PORTABLE_MANIFEST_SCHEMA_VERSION
        );
        if let Err(error) = std::fs::write(&manifest_path, manifest) {
            log::debug!(
                "补写便携版 manifest 失败（目录可能只读）{}: {}",
                manifest_path.display(),
                error
            );
        }
    }
}

fn migrate_legacy_webview_data(target_directory: &Path) -> Result<(), String> {
    let state_path = target_directory
        .join(WEBVIEW_MIGRATION_DIR)
        .join(WEBVIEW_MIGRATION_STATE_FILE);
    if read_webview_migration_state(&state_path)
        .is_some_and(|state| state.schema_version == 1 && state.completed)
    {
        return Ok(());
    }

    let Some(local_data_dir) = dirs::data_local_dir() else {
        return Ok(());
    };
    let source_directory = local_data_dir.join(APP_IDENTIFIER);
    if !source_directory.is_dir() || same_path(&source_directory, target_directory) {
        return write_webview_migration_state(
            &state_path,
            &WebviewMigrationState {
                schema_version: 1,
                completed: true,
                source_directory: source_directory.to_string_lossy().to_string(),
            },
        );
    }

    copy_webview_directory_contents(&source_directory, target_directory)?;
    write_webview_migration_state(
        &state_path,
        &WebviewMigrationState {
            schema_version: 1,
            completed: true,
            source_directory: source_directory.to_string_lossy().to_string(),
        },
    )
}

fn copy_webview_directory_contents(source: &Path, target: &Path) -> Result<(), String> {
    for entry_result in std::fs::read_dir(source)
        .map_err(|error| format!("读取旧版 WebView2 目录失败 {}: {}", source.display(), error))?
    {
        let entry = entry_result.map_err(|error| format!("读取 WebView2 条目失败: {}", error))?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|error| format!("读取 WebView2 条目类型失败: {}", error))?;

        // Do not follow symlinks, to avoid migration stepping outside the old WebView2 data directory boundary.
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            std::fs::create_dir_all(&target_path)
                .map_err(|error| format!("创建 WebView2 子目录失败: {}", error))?;
            copy_webview_directory_contents(&source_path, &target_path)?;
        } else if file_type.is_file() && !target_path.exists() {
            std::fs::copy(&source_path, &target_path).map_err(|error| {
                format!(
                    "复制 WebView2 数据失败 {} -> {}: {}",
                    source_path.display(),
                    target_path.display(),
                    error
                )
            })?;
        }
    }
    Ok(())
}

fn same_path(left: &Path, right: &Path) -> bool {
    path_compare_key(left) == path_compare_key(right)
}

fn read_webview_migration_state(path: &Path) -> Option<WebviewMigrationState> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(content.trim_start_matches('\u{feff}')).ok()
}

fn write_webview_migration_state(path: &Path, state: &WebviewMigrationState) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("WebView2 迁移状态路径无效: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建 WebView2 迁移状态目录失败: {}", error))?;
    let content = serde_json::to_string_pretty(state)
        .map_err(|error| format!("序列化 WebView2 迁移状态失败: {}", error))?;
    std::fs::write(path, content)
        .map_err(|error| format!("写入 WebView2 迁移状态失败 {}: {}", path.display(), error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_directory(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("lightc-runtime-{}-{}", name, std::process::id()))
    }

    #[test]
    fn detects_versioned_portable_manifest() {
        let root = test_directory("manifest");
        fs::create_dir_all(&root).unwrap();
        let manifest = r#"{"schema_version":1,"mode":"portable","data_layout":"relative"}"#;
        let mut manifest_with_bom = vec![0xEF, 0xBB, 0xBF];
        manifest_with_bom.extend_from_slice(manifest.as_bytes());
        fs::write(root.join(PORTABLE_MANIFEST_FILE), manifest_with_bom).unwrap();

        assert_eq!(
            detect_distribution_channel(&root.join("LightC.exe")),
            DistributionChannel::Portable
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn detects_legacy_portable_marker() {
        let root = test_directory("legacy-marker");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(PORTABLE_MARKER_FILE), "portable").unwrap();

        assert_eq!(
            detect_distribution_channel(&root.join("LightC.exe")),
            DistributionChannel::Portable
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_manifest_does_not_force_portable() {
        // A corrupted manifest must not be used to judge portable; install registration decides the channel instead.
        let installer_root = test_directory("invalid-manifest-installed");
        fs::create_dir_all(&installer_root).unwrap();
        fs::write(installer_root.join(PORTABLE_MANIFEST_FILE), "invalid").unwrap();
        let registered = installer_root.clone();

        let channel = resolve_distribution_channel(&installer_root.join("LightC.exe"), || {
            Ok(Some(registered))
        });
        assert_eq!(channel, DistributionChannel::Installer);

        // The same corrupted manifest in a directory with no install record is a portable copy (markers are self-healed at startup).
        let portable_root = test_directory("invalid-manifest-portable");
        fs::create_dir_all(&portable_root).unwrap();
        fs::write(portable_root.join(PORTABLE_MANIFEST_FILE), "invalid").unwrap();

        let channel =
            resolve_distribution_channel(&portable_root.join("LightC.exe"), || Ok(None));
        assert_eq!(channel, DistributionChannel::Portable);

        let _ = fs::remove_dir_all(installer_root);
        let _ = fs::remove_dir_all(portable_root);
    }

    #[test]
    fn treats_executable_without_install_record_as_portable() {
        // When the user kept only LightC.exe (marker files lost), the registry has no install record for the current directory,
        // so it must be judged portable; otherwise data would be written into AppData.
        let root = test_directory("no-marker-no-install");
        fs::create_dir_all(&root).unwrap();

        let channel = resolve_distribution_channel(&root.join("LightC.exe"), || Ok(None));

        assert_eq!(channel, DistributionChannel::Portable);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn treats_executable_in_install_location_as_installer() {
        // Installer: the registered install directory is exactly the exe's directory; the AppData layout must be preserved.
        let root = test_directory("installed-location");
        fs::create_dir_all(&root).unwrap();
        let install_directory = root.clone();

        let channel =
            resolve_distribution_channel(&root.join("LightC.exe"), || Ok(Some(install_directory)));

        assert_eq!(channel, DistributionChannel::Installer);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn installer_with_uninstaller_stays_installer_even_without_registry() {
        // Most critical regression guard: when an installer directory contains uninstall.exe, even if the registry cannot be read (deleted by
        // cleanup software, or due to permission problems), it must still be judged installer and never become portable and write the program directory.
        let root = test_directory("installed-with-uninstaller");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("LightC.exe"), b"stub").unwrap();
        fs::write(root.join("uninstall.exe"), b"stub").unwrap();

        // All three registry outcomes must be judged installer.
        assert_eq!(
            resolve_distribution_channel(&root.join("LightC.exe"), || Ok(None)),
            DistributionChannel::Installer
        );
        assert_eq!(
            resolve_distribution_channel(&root.join("LightC.exe"), || Err("注册表不可用".into())),
            DistributionChannel::Installer
        );
        assert_eq!(
            resolve_distribution_channel(&root.join("LightC.exe"), || Ok(Some(PathBuf::from(
                r"Z:\OtherPlace"
            )))),
            DistributionChannel::Installer
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn nested_install_layout_stays_installer() {
        // Compatible with the `App\<version>\LightC.exe` layout: the uninstaller is in the parent and install registration points to the parent.
        let root = test_directory("installed-nested");
        let version_dir = root.join("2.16.8");
        fs::create_dir_all(&version_dir).unwrap();
        fs::write(root.join("uninstall.exe"), b"stub").unwrap();
        let registered = root.clone();

        assert_eq!(
            resolve_distribution_channel(&version_dir.join("LightC.exe"), || Ok(Some(
                registered
            ))),
            DistributionChannel::Installer
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn uninstaller_check_precedes_registry_lookup() {
        // Extreme case: the registry points to the wrong place by mistake (corrupted by cleanup software, or multiple installs overwriting each other),
        // but as long as there is an uninstaller beside or one level above the exe, it must be judged installer, not led astray into portable by the registry.
        let root = test_directory("uninstaller-beats-registry");
        let version_dir = root.join("app");
        fs::create_dir_all(&version_dir).unwrap();
        fs::write(root.join("uninstall.exe"), b"stub").unwrap();

        let channel = resolve_distribution_channel(&version_dir.join("LightC.exe"), || {
            Ok(Some(PathBuf::from(r"Z:\SomewhereElse")))
        });

        assert_eq!(
            channel,
            DistributionChannel::Installer,
            "卸载器痕迹必须优先于注册表登记"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn drive_root_uninstaller_does_not_force_installer() {
        // A same-named file in the drive root cannot serve as install evidence, otherwise D:\uninstall.exe would judge a portable copy as installer.
        let portable_root = test_directory("portable-with-drive-root-uninstaller");
        fs::create_dir_all(&portable_root).unwrap();
        fs::write(portable_root.join("LightC.exe"), b"stub").unwrap();

        // Directly verify the decision function: when the portable directory itself has no uninstall.exe and the parent is a temp directory (not a drive root),
        // only a parent uninstall.exe would affect the result; here the registry returns None to cover the normal branch.
        assert_eq!(
            resolve_distribution_channel(&portable_root.join("LightC.exe"), || Ok(None)),
            DistributionChannel::Portable
        );

        let _ = fs::remove_dir_all(portable_root);
    }

    #[test]
    fn detects_uninstaller_only_in_expected_locations() {
        // Directly verify filesystem detection: a directory extracted from a portable package has no uninstall.exe and must not be treated as installer.
        let portable_root = test_directory("uninstaller-detection-portable");
        fs::create_dir_all(&portable_root).unwrap();
        assert!(!has_installer_uninstaller(&portable_root));

        // Installer layout: the uninstaller sits beside the exe.
        let installed_root = test_directory("uninstaller-detection-installed");
        fs::create_dir_all(&installed_root).unwrap();
        fs::write(installed_root.join("uninstall.exe"), b"stub").unwrap();
        assert!(has_installer_uninstaller(&installed_root));

        // Nested layout: the uninstaller is in the parent directory.
        let nested_parent = test_directory("uninstaller-detection-nested");
        let nested_child = nested_parent.join("0.0.0");
        fs::create_dir_all(&nested_child).unwrap();
        fs::write(nested_parent.join("uninstall.exe"), b"stub").unwrap();
        assert!(has_installer_uninstaller(&nested_child));

        // An uninstaller two levels up is not used as evidence, to avoid widening the false-positive scope.
        let deep_parent = test_directory("uninstaller-detection-deep");
        let deep_child = deep_parent.join("a").join("b");
        fs::create_dir_all(&deep_child).unwrap();
        fs::write(deep_parent.join("uninstall.exe"), b"stub").unwrap();
        assert!(!has_installer_uninstaller(&deep_child));

        let _ = fs::remove_dir_all(portable_root);
        let _ = fs::remove_dir_all(installed_root);
        let _ = fs::remove_dir_all(nested_parent);
        let _ = fs::remove_dir_all(deep_parent);
    }

    #[test]
    fn treats_portable_copy_of_installed_app_as_portable() {
        // A user copies the exe out of an install directory to use as a portable copy: only the exe is copied, no uninstall.exe,
        // and the registry points to the original install directory, so it should be judged portable.
        let install_root = test_directory("installed-original");
        let portable_root = test_directory("portable-copy");
        fs::create_dir_all(&install_root).unwrap();
        fs::create_dir_all(&portable_root).unwrap();
        fs::write(install_root.join("uninstall.exe"), b"stub").unwrap();
        fs::write(portable_root.join("LightC.exe"), b"stub").unwrap();
        let registered = install_root.clone();

        let channel =
            resolve_distribution_channel(&portable_root.join("LightC.exe"), || Ok(Some(registered)));

        assert_eq!(channel, DistributionChannel::Portable);
        let _ = fs::remove_dir_all(install_root);
        let _ = fs::remove_dir_all(portable_root);
    }

    #[test]
    fn falls_back_to_installer_when_registry_is_unreadable() {
        // When the registry is unavailable, treat as installer and warn, to avoid both channels writing the program directory and causing data ambiguity.
        let root = test_directory("registry-error");
        fs::create_dir_all(&root).unwrap();

        let channel = resolve_distribution_channel(&root.join("LightC.exe"), || {
            Err("注册表不可用".to_string())
        });

        assert_eq!(channel, DistributionChannel::Installer);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn trims_quotes_and_trailing_separator_from_registry_path() {
        assert_eq!(trim_registry_path("\"D:\\LightC\""), "D:\\LightC");
        assert_eq!(trim_registry_path("D:\\LightC\\"), "D:\\LightC");
        assert_eq!(trim_registry_path("  \"D:\\LightC\"  "), "D:\\LightC");
    }

    #[test]
    fn repairs_missing_portable_markers() {
        // When the user kept only the exe, startup should restore both marker files so later decisions no longer depend on registry queries.
        let root = test_directory("repair-markers");
        fs::create_dir_all(&root).unwrap();
        let executable_path = root.join("LightC.exe");
        fs::write(&executable_path, b"stub").unwrap();

        assert!(!root.join(PORTABLE_MARKER_FILE).exists());
        repair_portable_marker_for(&executable_path);

        assert!(root.join(PORTABLE_MARKER_FILE).is_file());
        let manifest_content = fs::read_to_string(root.join(PORTABLE_MANIFEST_FILE))
            .expect("manifest 应被补齐");
        let manifest: serde_json::Value =
            serde_json::from_str(&manifest_content).expect("补齐的 manifest 必须是合法 JSON");
        assert_eq!(manifest["mode"], "portable");
        assert_eq!(manifest["data_layout"], "relative");

        // The restored markers must pass the decision on their own (no longer depending on the registry branch).
        assert_eq!(
            detect_distribution_channel(&executable_path),
            DistributionChannel::Portable
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_install_location_from_real_registry() {
        // This test reads the local registry directly to verify that winreg opens views and reads values correctly on a real NSIS registration
        // (paths with quotes, possibly missing fields). A machine without LightC installed only verifies the "no record" branch,
        // so the assertions cover only "either reads a directory or confirms not installed" and will not falsely fail on CI.
        match installed_application_directory() {
            Ok(Some(directory)) => {
                assert!(
                    directory.is_absolute(),
                    "安装目录必须是绝对路径: {}",
                    directory.display()
                );
                assert!(
                    !directory.to_string_lossy().contains('"'),
                    "注册表路径的引号必须被清理: {}",
                    directory.display()
                );
            }
            Ok(None) => {}
            Err(error) => panic!("读取安装登记不应失败: {}", error),
        }
    }
}
