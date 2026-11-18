// ============================================================================
// Unified data directory management module
//
// Manages the storage directory for all of LightC's local persistent data, including:
//   - Cleanup logs (logs/)
//   - Install history cache (install_history.json)
//   - ProgramData snapshots
//
// Installer config is always stored at %LOCALAPPDATA%/LightC/config/config.json,
// portable config and default data follow the exe, so the portable package no longer depends on AppData.
// Users can customize the data directory path via the UI. Existing data is migrated automatically on change.
// ============================================================================

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::RwLock;

use log;

use crate::runtime::{
    current_application_root, current_executable_path, detect_distribution_channel,
    portable_webview_data_directory, DistributionChannel,
};

// ============================================================================
// Constants
// ============================================================================

/// App root directory name based on LOCALAPPDATA; config and default data are partitioned under this directory.
const APP_ROOT_DIR_NAME: &str = "LightC";

/// Default data subdirectory name, to avoid putting config.json and runtime data like logs/snapshots at the same level.
const DEFAULT_DATA_DIR_NAME: &str = "data";

/// Config subdirectory name; both installer and portable keep a separate config/data structure.
const CONFIG_DIR_NAME: &str = "config";

/// Config file name relative to the default directory
const CONFIG_FILE: &str = "config.json";

/// Migration state lives in the data directory, to avoid writing migration metadata into user config and affecting older versions' reads.
const PORTABLE_MIGRATION_DIR: &str = ".migration";
const PORTABLE_MIGRATION_STATE_FILE: &str = "legacy_appdata_v1.json";

/// When migrating the data directory, copy only data explicitly owned by LightC, so a user mistakenly picking a disk root won't carry unrelated files along.
const MIGRATABLE_DATA_ENTRIES: [&str; 6] = [
    "install_history.json",
    // False-positive protection rules the user explicitly confirmed must migrate along with the custom data directory.
    "leftover_whitelist.json",
    "logs",
    "reg_backups",
    "disk_growth_snapshots",
    "driver_backups",
];

// ============================================================================
// Runtime cache
// ============================================================================

/// Global data directory path cache, to avoid reading the disk every time
static DATA_DIR_CACHE: std::sync::LazyLock<RwLock<PathBuf>> = std::sync::LazyLock::new(|| {
    let path = load_or_create();
    RwLock::new(path)
});

// ============================================================================
// Data Structures
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DataDirConfig {
    data_dir: String,
    /// New portable default directory uses a relative value, so it still resolves to the current exe directory after the program is moved.
    #[serde(default)]
    data_dir_mode: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageLocationInfo {
    pub distribution_channel: DistributionChannel,
    pub config_directory: String,
    pub config_file: String,
    pub default_data_directory: String,
    pub current_data_directory: String,
    pub data_directory_is_custom: bool,
    pub portable_root: Option<String>,
    pub webview_data_directory: Option<String>,
    pub legacy_data_directory: Option<String>,
    pub can_write: bool,
    pub migration_available: bool,
    pub migration_completed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PortableMigrationState {
    schema_version: u32,
    completed: bool,
    source_directory: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClearableDataItem {
    pub id: String,
    pub label: String,
    pub description: String,
    pub path: String,
    pub item_type: String,
    pub exists: bool,
    pub file_count: usize,
    pub size: u64,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClearLocalDataResult {
    pub deleted_files: usize,
    pub freed_bytes: u64,
    /// Number of items successfully cleaned
    pub cleared_items: usize,
    /// Items that failed to clean and the reasons
    ///
    /// A single item's failure does not abort the whole batch: if the user checks three items and one has a locked log file,
    /// the other two are still cleaned, and only the failed one is returned here for the UI to report.
    pub failed_items: Vec<ClearLocalDataFailure>,
}

/// Reason a single cleanup item failed
#[derive(Debug, Clone, Serialize)]
pub struct ClearLocalDataFailure {
    /// Cleanup item identifier
    pub item: String,
    /// Failure reason (shown directly to the user)
    pub reason: String,
}

/// Portable write-capability diagnostic.
///
/// Note: the normal state for the portable edition is "all data follows the exe". When the program directory is not writable (extracted to
/// `C:\Program Files`, a read-only USB drive, antivirus/group-policy restrictions, etc.), the app can only put data in AppData,
/// which makes users think the portable edition is "writing to C drive" again. The result is returned structured here so the settings page can give clear guidance.
#[derive(Debug, Clone, Serialize)]
pub struct StorageWriteDiagnostic {
    /// Has a value only when determined to be portable: the directory containing the exe.
    pub portable_root: Option<String>,
    /// The data directory the app expects to write to (for portable, the data dir under the exe directory).
    pub preferred_data_directory: String,
    /// Whether the expected directory is really writable (verified by actually creating and deleting a probe file).
    pub preferred_writable: bool,
    /// Whether a "portable dir not writable → fell back to AppData" occurred.
    pub fell_back_to_app_data: bool,
    /// Whether the currently used data directory is writable.
    pub active_writable: bool,
    pub reason: Option<String>,
}

struct ClearableDataDefinition {
    id: &'static str,
    label: &'static str,
    description: &'static str,
    relative_path: &'static str,
    item_type: ClearableDataType,
    warning: Option<&'static str>,
    /// When computing parent-directory usage and cleaning, exclude independently managed child paths to avoid counting or deleting the same files twice.
    excluded_relative_paths: &'static [&'static str],
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClearableDataType {
    File,
    DirectoryContents,
}

const DISK_GROWTH_SNAPSHOT_DIR: &str = "disk_growth_snapshots";
const DISK_GROWTH_SNAPSHOT_ID_PREFIX: &str = "disk_growth_snapshots_";
const DISK_GROWTH_SNAPSHOT_BASE_ID: &str = "disk_growth_snapshots";
const DISK_GROWTH_SNAPSHOT_BASE_PREFIX: &str = "disk_growth_";
const DISK_GROWTH_SNAPSHOT_JSON_SUFFIX: &str = ".json";
const DISK_GROWTH_SNAPSHOT_SHARD_SUFFIX: &str = ".files";

// Snapshot entries expand dynamically by drive letter; the normal whitelist keeps only fixed data, so "Clear local data" won't delete all disk baselines.
const CLEARABLE_DATA_DEFINITIONS: [ClearableDataDefinition; 6] = [
    ClearableDataDefinition {
        id: "install_history",
        label: "安装历史缓存",
        description: "用于辅助卸载残留识别，删除后会重新学习历史安装路径。",
        relative_path: "install_history.json",
        item_type: ClearableDataType::File,
        warning: Some("可安全清理，但卸载残留模块的历史识别信号会重新建立。"),
        excluded_relative_paths: &[],
    },
    ClearableDataDefinition {
        id: "logs",
        label: "清理日志",
        description: "记录历史清理明细，仅用于回看操作记录。",
        relative_path: "logs",
        item_type: ClearableDataType::DirectoryContents,
        warning: None,
        excluded_relative_paths: &["shell_icons.log"],
    },
    ClearableDataDefinition {
        id: "reg_backups",
        label: "注册表备份",
        description: "右键菜单和注册表清理前生成的备份文件。",
        relative_path: "reg_backups",
        item_type: ClearableDataType::DirectoryContents,
        warning: Some("删除后无法再通过这些备份回溯旧注册表清理操作。"),
        excluded_relative_paths: &["shell_icons"],
    },
    ClearableDataDefinition {
        id: "shell_icon_logs",
        label: "虚拟磁盘操作记录",
        description: "记录虚拟磁盘删除、恢复和防复活操作，便于确认处理过哪些入口。",
        relative_path: "logs/shell_icons.log",
        item_type: ClearableDataType::File,
        warning: None,
        excluded_relative_paths: &[],
    },
    ClearableDataDefinition {
        id: "shell_icon_backups",
        label: "虚拟磁盘注册表备份",
        description: "外壳图标清理操作前保存的注册表和权限备份，可用于恢复入口。",
        relative_path: "reg_backups/shell_icons",
        item_type: ClearableDataType::DirectoryContents,
        warning: Some("删除后无法再通过备份恢复已移除的虚拟磁盘入口。"),
        excluded_relative_paths: &[],
    },
    ClearableDataDefinition {
        id: "driver_backups",
        label: "驱动备份",
        description: "旧驱动清理前导出的驱动包备份，文件可能较大。",
        relative_path: "driver_backups",
        item_type: ClearableDataType::DirectoryContents,
        warning: Some("删除后将无法使用这些备份手动恢复已清理的驱动包。默认不会勾选。"),
        excluded_relative_paths: &[],
    },
];

// ============================================================================
// Internal functions
// ============================================================================

/// Installer app local root directory (%LOCALAPPDATA%/LightC), used as the migration source for older data.
fn app_local_root_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join(APP_ROOT_DIR_NAME))
}

/// Storage root directory of the current distribution; portable uses the exe's directory as root, installer uses AppData.
fn storage_root_dir() -> Option<PathBuf> {
    current_application_root()
}

/// Default data directory path.
fn default_data_dir() -> Option<PathBuf> {
    storage_root_dir().map(|dir| dir.join(DEFAULT_DATA_DIR_NAME))
}

/// Config file storage path; portable config must also follow the exe and remain valid after moving the whole directory.
fn config_file_path() -> Option<PathBuf> {
    storage_root_dir().map(|dir| dir.join(CONFIG_DIR_NAME).join(CONFIG_FILE))
}

/// Older versions put config at %LOCALAPPDATA%/LightC/config.json; initialization must read it for compatibility.
fn legacy_config_file_path() -> Option<PathBuf> {
    app_local_root_dir().map(|dir| dir.join(CONFIG_FILE))
}

/// Load the config or create a default one
fn load_or_create() -> PathBuf {
    let default = default_data_dir().unwrap_or_else(|| PathBuf::from("."));
    let channel = current_distribution_channel();

    // First copy the old config to the portable directory, then resolve the data directory from the config, avoiding old config pointing straight to AppData.
    // No migration in dev builds: a debug instance shouldn't inherit the release package's custom data directory (otherwise dev data would be written to
    // a directory the user previously selected, looking as if "the portable config was read").
    if channel == DistributionChannel::Portable && !cfg!(debug_assertions) {
        migrate_legacy_portable_config_if_needed();
    }

    if let Some((config, from_legacy_config)) = load_existing_config() {
        let (configured_data_dir, is_legacy_default) =
            resolve_configured_data_dir(&config, &default, channel);
        if is_legacy_default {
            migrate_legacy_data_to_default(&default, channel);
        }
        // A custom data directory is explicitly chosen by the user; if not writable, only warn, don't move the user's data without permission.
        let allow_fallback = !is_legacy_default
            || path_compare_key(&configured_data_dir) == path_compare_key(&default);
        let resolved = ensure_writable_data_dir(
            configured_data_dir,
            channel,
            allow_fallback,
        );
        if let Err(error) = save_config_inner(&resolved) {
            log::warn!("保存数据目录配置失败: {}", error);
        }
        log::info!(
            "数据目录 ({}): {}",
            if from_legacy_config {
                "旧配置迁移"
            } else {
                "配置"
            },
            resolved.display()
        );
        return resolved;
    }

    // When config is missing, migrate old default data; copy only whitelist contents to avoid bringing unrelated user files into the portable package.
    // Skip in dev builds: a debug instance shouldn't move the release package's historical data into the target directory.
    if !cfg!(debug_assertions) {
        migrate_legacy_data_to_default(&default, channel);
    }

    let resolved = ensure_writable_data_dir(default, channel, true);
    if let Err(error) = save_config_inner(&resolved) {
        log::warn!("保存默认数据目录配置失败: {}", error);
    }

    log::info!("数据目录 (默认): {}", resolved.display());
    resolved
}

/// Before landing the portable data directory, confirm it is writable: if not, explicitly fall back to AppData and record a readable reason.
///
/// Note: previously we relied only on `create_dir_all`, which returns success immediately when the directory already exists,
/// so logs/snapshots would fail only at actual write time, or even silently land in AppData making users think the portable edition writes to C drive again.
/// Here we verify with a real probe and write the fallback result into logs and config for the settings page to display.
fn ensure_writable_data_dir(
    preferred: PathBuf,
    channel: DistributionChannel,
    allow_fallback: bool,
) -> PathBuf {
    if probe_writable_directory(&preferred).is_ok() {
        return preferred;
    }
    if channel != DistributionChannel::Portable || !allow_fallback {
        log::warn!("数据目录不可写: {}", preferred.display());
        return preferred;
    }

    // In the extreme case where even AppData isn't writable, keep the preferred directory so later write failures expose the real reason.
    let Some(fallback) = app_data_fallback_dir()
        .filter(|candidate| probe_writable_directory(candidate).is_ok())
    else {
        log::warn!(
            "便携目录与备用目录均不可写，继续使用: {}",
            preferred.display()
        );
        return preferred;
    };

    log::warn!(
        "便携目录不可写，数据改用 {}（原目录: {}）",
        fallback.display(),
        preferred.display()
    );
    fallback
}

fn load_existing_config() -> Option<(DataDirConfig, bool)> {
    if let Some(config_path) = config_file_path() {
        if let Some(config) = read_config_file(&config_path) {
            return Some((config, false));
        }
    }

    if current_distribution_channel() == DistributionChannel::Portable {
        return None;
    }

    legacy_config_file_path()
        .and_then(|config_path| read_config_file(&config_path))
        .map(|config| (config, true))
}

fn read_config_file(path: &Path) -> Option<DataDirConfig> {
    let json = fs::read_to_string(path).ok()?;
    match serde_json::from_str::<DataDirConfig>(json.trim_start_matches('\u{feff}')) {
        Ok(config) => Some(config),
        Err(error) => {
            // Don't keep using stale values when config is corrupted, to avoid writing a bad path back and worsening the data directory problem.
            log::warn!("读取配置文件失败 {}: {}", path.display(), error);
            None
        }
    }
}

/// Convert an old default path to the current distribution's default path.
///
/// Only the AppData root or AppData/data count as old defaults; a custom path the user actively chose must be preserved.
fn resolve_configured_data_dir(
    config: &DataDirConfig,
    default_data_dir: &Path,
    channel: DistributionChannel,
) -> (PathBuf, bool) {
    if channel == DistributionChannel::Portable
        && config.data_dir_mode.as_deref() == Some("relative")
        && config.data_dir == DEFAULT_DATA_DIR_NAME
    {
        return (default_data_dir.to_path_buf(), true);
    }

    let configured_path = PathBuf::from(&config.data_dir);
    if !configured_path.is_absolute() {
        // External config may have been hand-edited; forbid relative paths from following the current working directory and causing data write drift.
        log::warn!(
            "配置中的数据目录不是绝对路径，将回退到默认目录: {}",
            config.data_dir
        );
        return (default_data_dir.to_path_buf(), true);
    }

    let Some(legacy_root) = app_local_root_dir() else {
        return (configured_path, false);
    };

    let is_legacy_default = if channel == DistributionChannel::Portable {
        let legacy_default = legacy_root.join(DEFAULT_DATA_DIR_NAME);
        path_compare_key(&configured_path) == path_compare_key(&legacy_root)
            || path_compare_key(&configured_path) == path_compare_key(&legacy_default)
    } else {
        path_compare_key(&configured_path) == path_compare_key(&legacy_root)
    };

    if is_legacy_default {
        (default_data_dir.to_path_buf(), true)
    } else {
        (configured_path, false)
    }
}

#[cfg(test)]
fn normalize_legacy_default_data_dir_inner(
    configured_path: &Path,
    legacy_root: &Path,
    default_data_dir: &Path,
) -> PathBuf {
    if path_compare_key(configured_path) == path_compare_key(legacy_root) {
        return default_data_dir.to_path_buf();
    }

    configured_path.to_path_buf()
}

fn migrate_legacy_default_data_if_needed(default_data_dir: &Path) {
    let Some(legacy_root) = app_local_root_dir() else {
        return;
    };

    if path_compare_key(&legacy_root) == path_compare_key(default_data_dir) {
        return;
    }

    // Migrate only data items explicitly owned by LightC here, to avoid copying config, the config directory, or stray user files into the default data dir.
    if legacy_root.is_dir() {
        if let Err(error) = migrate_owned_data_entries(&legacy_root, default_data_dir) {
            log::warn!(
                "迁移旧版默认数据目录失败 {} -> {}: {}",
                legacy_root.display(),
                default_data_dir.display(),
                error
            );
        }
    }
}

/// Copy whitelisted data from old AppData to the current default directory; source files are always kept.
fn migrate_legacy_data_to_default(default_data_dir: &Path, channel: DistributionChannel) {
    let Some(legacy_root) = app_local_root_dir() else {
        return;
    };

    if channel == DistributionChannel::Portable {
        if let Err(error) = migrate_legacy_portable_data_inner(&legacy_root, default_data_dir) {
            log::warn!(
                "迁移旧版便携数据失败 {} -> {}: {}",
                legacy_root.display(),
                default_data_dir.display(),
                error
            );
        }
    } else {
        migrate_legacy_default_data_if_needed(default_data_dir);
    }
}

/// Older portable programs actually wrote data to AppData; the new version copies only content explicitly belonging to LightC.
fn migrate_legacy_portable_data_inner(
    legacy_root: &Path,
    portable_data_dir: &Path,
) -> Result<(), String> {
    let migration_state_path = portable_data_dir
        .join(PORTABLE_MIGRATION_DIR)
        .join(PORTABLE_MIGRATION_STATE_FILE);
    if read_portable_migration_state(&migration_state_path)
        .is_some_and(|state| state.schema_version == 1 && state.completed)
    {
        return Ok(());
    }

    if !legacy_root.is_dir() {
        return Ok(());
    }

    // Check both the old root and the new AppData/data, covering several historical layout versions.
    let legacy_data_dir = legacy_root.join(DEFAULT_DATA_DIR_NAME);
    migrate_owned_data_entries(legacy_root, portable_data_dir)?;
    if legacy_data_dir.is_dir() {
        migrate_owned_data_entries(&legacy_data_dir, portable_data_dir)?;
    }

    write_portable_migration_state(
        &migration_state_path,
        &PortableMigrationState {
            schema_version: 1,
            completed: true,
            source_directory: legacy_root.to_string_lossy().to_string(),
        },
    )
}

/// When the portable edition has no local config, first copy the old config, then later rewrite its default data path to a relative directory.
fn migrate_legacy_portable_config_if_needed() {
    let Some(target_path) = config_file_path() else {
        return;
    };
    if target_path.exists() {
        return;
    }

    let Some(legacy_root) = app_local_root_dir() else {
        return;
    };
    let candidates = [
        legacy_root.join(CONFIG_DIR_NAME).join(CONFIG_FILE),
        legacy_root.join(CONFIG_FILE),
    ];

    for source_path in candidates {
        if !source_path.is_file() || read_config_file(&source_path).is_none() {
            continue;
        }

        let Some(parent) = target_path.parent() else {
            return;
        };
        if let Err(error) = fs::create_dir_all(parent) {
            log::warn!("创建便携版配置目录失败 {}: {}", parent.display(), error);
            return;
        }
        if let Err(error) = fs::copy(&source_path, &target_path) {
            log::warn!(
                "复制旧版配置失败 {} -> {}: {}",
                source_path.display(),
                target_path.display(),
                error
            );
        } else {
            log::info!(
                "已复制旧版便携配置 {} -> {}",
                source_path.display(),
                target_path.display()
            );
        }
        return;
    }
}

fn current_distribution_channel() -> DistributionChannel {
    current_executable_path()
        .map(|path| detect_distribution_channel(&path))
        .unwrap_or(DistributionChannel::Installer)
}

fn read_portable_migration_state(path: &Path) -> Option<PortableMigrationState> {
    let json = fs::read_to_string(path).ok()?;
    serde_json::from_str(&json).ok()
}

fn write_portable_migration_state(
    path: &Path,
    state: &PortableMigrationState,
) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err(format!("迁移状态路径无效: {}", path.display()));
    };
    fs::create_dir_all(parent)
        .map_err(|error| format!("创建迁移状态目录失败 {}: {}", parent.display(), error))?;
    let json = serde_json::to_string_pretty(state)
        .map_err(|error| format!("序列化迁移状态失败: {}", error))?;
    fs::write(path, json).map_err(|error| format!("写入迁移状态失败 {}: {}", path.display(), error))
}

/// Persist the config to disk
fn save_config_inner(path: &PathBuf) -> Result<(), String> {
    let cfg_path = config_file_path().ok_or_else(|| "无法确定配置文件路径".to_string())?;
    let parent = cfg_path
        .parent()
        .ok_or_else(|| format!("配置文件路径无效: {}", cfg_path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("创建配置目录失败 {}: {}", parent.display(), error))?;
    let is_portable_default = current_distribution_channel() == DistributionChannel::Portable
        && default_data_dir()
            .is_some_and(|default| path_compare_key(&default) == path_compare_key(path));
    let config = DataDirConfig {
        data_dir: if is_portable_default {
            DEFAULT_DATA_DIR_NAME.to_string()
        } else {
            path.to_string_lossy().to_string()
        },
        data_dir_mode: is_portable_default.then_some("relative".to_string()),
    };
    let json = serde_json::to_string_pretty(&config)
        .map_err(|error| format!("序列化数据目录配置失败: {}", error))?;
    fs::write(&cfg_path, json)
        .map_err(|error| format!("写入配置文件失败 {}: {}", cfg_path.display(), error))
}

fn canonical_or_absolute(path: &Path) -> Result<PathBuf, String> {
    if path.exists() {
        return path
            .canonicalize()
            .map_err(|e| format!("解析路径 {} 失败: {}", path.display(), e));
    }

    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    std::env::current_dir()
        .map(|current_dir| current_dir.join(path))
        .map_err(|e| format!("解析当前目录失败: {}", e))
}

fn path_compare_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

fn is_same_or_child_path(path: &str, parent: &str) -> bool {
    path == parent
        || path
            .strip_prefix(parent)
            .is_some_and(|remaining| remaining.starts_with('\\'))
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    let left_key = path_compare_key(left);
    let right_key = path_compare_key(right);

    is_same_or_child_path(&left_key, &right_key) || is_same_or_child_path(&right_key, &left_key)
}

fn ensure_migration_target_is_safe(old_path: &Path, new_path: &Path) -> Result<(), String> {
    let old_key = canonical_or_absolute(old_path)?;
    let new_key = canonical_or_absolute(new_path)?;

    if paths_overlap(&old_key, &new_key) {
        return Err(
            "新的数据目录不能选择当前数据目录本身、其子目录或父目录，请选择一个独立的空文件夹。"
                .to_string(),
        );
    }

    if new_path.exists() && !new_path.is_dir() {
        return Err(format!("新的数据目录不是文件夹: {}", new_path.display()));
    }

    if new_path.exists() && !is_directory_empty(new_path)? {
        return Err("新的数据目录必须是空文件夹，避免把非 LightC 数据混入迁移流程。".to_string());
    }

    Ok(())
}

fn is_directory_empty(path: &Path) -> Result<bool, String> {
    Ok(fs::read_dir(path)
        .map_err(|e| format!("读取目标目录失败 {}: {}", path.display(), e))?
        .next()
        .is_none())
}

fn migrate_owned_data_entries(src: &Path, dest: &Path) -> Result<(), String> {
    fs::create_dir_all(dest).map_err(|e| format!("创建目标目录失败: {}", e))?;

    for entry_name in MIGRATABLE_DATA_ENTRIES {
        let src_path = src.join(entry_name);
        if !src_path.exists() {
            continue;
        }

        let dest_path = dest.join(entry_name);
        if src_path.is_dir() {
            copy_dir_contents(&src_path, &dest_path)?;
        } else if src_path.is_file() && !dest_path.exists() {
            fs::copy(&src_path, &dest_path)
                .map_err(|e| format!("复制文件 {} 失败: {}", src_path.display(), e))?;
        }
    }

    Ok(())
}

/// Recursively copy directory contents, only used for whitelisted data directory migration.
fn copy_dir_contents(src: &Path, dest: &Path) -> Result<(), String> {
    fs::create_dir_all(dest).map_err(|e| format!("创建目标目录失败: {}", e))?;

    for entry_res in fs::read_dir(src).map_err(|e| format!("读取源目录失败: {}", e))? {
        let entry = entry_res.map_err(|e| format!("读取目录条目失败: {}", e))?;
        let src_path = entry.path();
        let dest_path = dest.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_contents(&src_path, &dest_path)?;
        } else if src_path.is_file() && !dest_path.exists() {
            // Both initialization and migration can run repeatedly; when the target already has the file, keep the version in the new directory, avoiding old data overwriting new.
            fs::copy(&src_path, &dest_path)
                .map_err(|e| format!("复制文件 {} 失败: {}", src_path.display(), e))?;
        }
    }

    Ok(())
}

// ============================================================================
// Public API
// ============================================================================

/// Print this run's storage paths and decision basis (debug builds only).
///
/// Note: portable and installer data directories are jointly determined by "distribution mode + config file + data dir config";
/// when investigating "where data actually went", looking only at the config file is misleading. This logs all key paths and their sources at once,
/// so launching in a dev environment (cargo tauri dev) shows them in the terminal.
#[cfg(debug_assertions)]
pub fn log_storage_diagnostics() {
    let channel = current_distribution_channel();
    let config_file = config_file_path()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|| "(无法确定)".to_string());
    let default_directory = default_data_dir()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|| "(无法确定)".to_string());
    let active_directory = get_data_dir();
    let executable = current_executable_path()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|_| "(无法确定)".to_string());
    // Explain whether the current data directory "follows the program directory" or is "a custom directory the user set in settings".
    let directory_source = if path_compare_key(&active_directory) == path_compare_key(Path::new(&default_directory)) {
        "默认（跟随发行模式）"
    } else {
        "自定义（配置文件中记录，可能在别的磁盘）"
    };

    log::info!(
        "存储诊断: 发行模式={:?} | 程序={} | 配置文件={} | 数据目录={} [{}] | 界面数据目录={}",
        channel,
        executable,
        config_file,
        active_directory.display(),
        directory_source,
        portable_webview_data_directory()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| "(安装版默认 AppData)".to_string())
    );
}

/// Get the current data directory path
pub fn get_data_dir() -> PathBuf {
    DATA_DIR_CACHE.read().unwrap().clone()
}

/// Get the default data directory path (for UI display)
pub fn get_default_dir() -> PathBuf {
    default_data_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// Get current config, default data and migration state; the frontend does not infer portable paths itself.
pub fn get_storage_location_info() -> StorageLocationInfo {
    let channel = current_distribution_channel();
    let storage_root = storage_root_dir().unwrap_or_else(|| PathBuf::from("."));
    let config_directory = storage_root.join(CONFIG_DIR_NAME);
    let config_file = config_directory.join(CONFIG_FILE);
    let default_data_directory = get_default_dir();
    let current_data_directory = get_data_dir();
    let legacy_root = app_local_root_dir();
    let migration_state_path = default_data_directory
        .join(PORTABLE_MIGRATION_DIR)
        .join(PORTABLE_MIGRATION_STATE_FILE);
    let migration_completed = read_portable_migration_state(&migration_state_path)
        .is_some_and(|state| state.schema_version == 1 && state.completed);
    let migration_available = channel == DistributionChannel::Portable
        && !migration_completed
        && legacy_root
            .as_deref()
            .is_some_and(has_migratable_data_entries);

    StorageLocationInfo {
        distribution_channel: channel,
        config_directory: config_directory.to_string_lossy().to_string(),
        config_file: config_file.to_string_lossy().to_string(),
        default_data_directory: default_data_directory.to_string_lossy().to_string(),
        current_data_directory: current_data_directory.to_string_lossy().to_string(),
        data_directory_is_custom: path_compare_key(&default_data_directory)
            != path_compare_key(&current_data_directory),
        portable_root: (channel == DistributionChannel::Portable)
            .then(|| storage_root.to_string_lossy().to_string()),
        webview_data_directory: portable_webview_data_directory()
            .map(|path| path.to_string_lossy().to_string()),
        legacy_data_directory: (channel == DistributionChannel::Portable)
            .then(|| {
                legacy_root
                    .as_ref()
                    .map(|path| path.to_string_lossy().to_string())
            })
            .flatten(),
        can_write: can_write_storage(&config_directory, &current_data_directory),
        migration_available,
        migration_completed,
    }
}

/// Manually retry migration of old portable data, for the settings page to handle automatic migration failures or old data added later.
pub fn migrate_legacy_portable_data() -> Result<StorageLocationInfo, String> {
    if current_distribution_channel() != DistributionChannel::Portable {
        return Err("只有便携版支持迁移旧版 AppData 数据。".to_string());
    }

    let legacy_root =
        app_local_root_dir().ok_or_else(|| "无法确定旧版 AppData 数据目录。".to_string())?;
    let default_data_directory = get_default_dir();
    migrate_legacy_portable_data_inner(&legacy_root, &default_data_directory)?;
    Ok(get_storage_location_info())
}

fn has_migratable_data_entries(root: &Path) -> bool {
    let candidate_roots = [root.to_path_buf(), root.join(DEFAULT_DATA_DIR_NAME)];
    candidate_roots.iter().any(|candidate_root| {
        MIGRATABLE_DATA_ENTRIES
            .iter()
            .map(|entry| candidate_root.join(entry))
            .any(|path| path.exists())
    })
}

fn can_write_storage(config_directory: &Path, data_directory: &Path) -> bool {
    // Directories were already created during initialization; here only confirm the current runtime config can still write, without creating test files that pollute user dirs.
    fs::create_dir_all(config_directory).is_ok() && fs::create_dir_all(data_directory).is_ok()
}

/// Real write probe: create the directory, write a 0-byte file and delete it immediately.
///
/// Note: relying only on `create_dir_all` is unreliable — if the directory already exists, it returns success even without write permission,
/// so logs, snapshots and backups fail only at actual write time. The probe file is deleted right after use, leaving no garbage.
fn probe_writable_directory(directory: &Path) -> Result<(), String> {
    fs::create_dir_all(directory)
        .map_err(|error| format!("无法创建目录 {}: {}", directory.display(), error))?;

    let probe_path = directory.join(format!(".lightc-write-probe-{}", std::process::id()));
    match fs::write(&probe_path, b"") {
        Ok(()) => {
            let _ = fs::remove_file(&probe_path);
            Ok(())
        }
        Err(error) => Err(format!("目录不可写 {}: {}", directory.display(), error)),
    }
}

/// Portable write diagnostic.
///
/// When the portable package is extracted to `C:\Program Files`, a read-only USB drive, or a directory protected by antivirus/group policy, the program dir is not writable,
/// so data can only land in AppData. The result is returned structured here, and the settings page gives clear guidance to "move the program to a writable directory".
pub fn get_storage_write_diagnostic() -> StorageWriteDiagnostic {
    let channel = current_distribution_channel();
    let preferred_directory = default_data_dir().unwrap_or_else(|| PathBuf::from("."));
    let active_directory = get_data_dir();
    let portable_root = (channel == DistributionChannel::Portable)
        .then(|| storage_root_dir())
        .flatten();

    if channel != DistributionChannel::Portable {
        // The installer is supposed to write AppData anyway; just confirm the current directory is still writable.
        return StorageWriteDiagnostic {
            portable_root: None,
            preferred_data_directory: preferred_directory.to_string_lossy().to_string(),
            preferred_writable: true,
            fell_back_to_app_data: false,
            active_writable: probe_writable_directory(&active_directory).is_ok(),
            reason: None,
        };
    }

    let preferred_error = probe_writable_directory(&preferred_directory).err();
    let active_writable = probe_writable_directory(&active_directory).is_ok();
    // Only "expected dir not writable + actually using the AppData dir" counts as a fallback. A custom data directory
    // or other paths in old config must not be misreported as "writing to C drive again".
    let active_is_app_data = app_local_root_dir()
        .is_some_and(|root| is_same_or_child_path(&path_compare_key(&active_directory), &path_compare_key(&root)));
    let fell_back = preferred_error.is_some()
        && !path_compare_key(&active_directory).eq(&path_compare_key(&preferred_directory))
        && active_is_app_data;

    StorageWriteDiagnostic {
        portable_root: portable_root.map(|path| path.to_string_lossy().to_string()),
        preferred_data_directory: preferred_directory.to_string_lossy().to_string(),
        preferred_writable: preferred_error.is_none(),
        fell_back_to_app_data: fell_back,
        active_writable,
        // reason is only given when a real fallback occurred, to avoid misreporting cases like "custom directory" as writing to C drive.
        reason: fell_back.then(|| {
            format!(
                "便携目录不可写，数据已临时保存到 {}",
                active_directory.display()
            )
        }),
    }
}

/// Fallback data directory when the portable dir isn't writable: the installer location (%LOCALAPPDATA%\LightC\data).
fn app_data_fallback_dir() -> Option<PathBuf> {
    app_local_root_dir().map(|root| root.join(DEFAULT_DATA_DIR_NAME))
}

/// Set a new data directory and migrate existing data
///
/// [Note]
/// 1. Create the new directory
/// 2. Copy all data from the old directory to the new one
/// 3. Update the runtime cache and persisted config file
///
/// Note: data in the old directory is not deleted; clean it up manually if needed.
pub fn set_data_dir(new_path: &Path) -> Result<(), String> {
    let old_path = get_data_dir();
    let old_key = canonical_or_absolute(&old_path)?;
    let new_key = canonical_or_absolute(new_path)?;

    // Skip if the path is the same
    if old_key == new_key {
        return Ok(());
    }

    ensure_migration_target_is_safe(&old_path, new_path)?;

    // Create the new directory
    fs::create_dir_all(new_path)
        .map_err(|e| format!("无法创建数据目录 {}: {}", new_path.display(), e))?;

    // Migrate existing data
    if old_path.exists() && old_path.is_dir() {
        log::info!(
            "正在迁移数据: {} -> {}",
            old_path.display(),
            new_path.display()
        );
        migrate_owned_data_entries(&old_path, new_path)?;
        log::info!("数据迁移完成");
    }

    // Update the cache and persist
    let path_buf = new_path.to_path_buf();
    save_config_inner(&path_buf)?;
    *DATA_DIR_CACHE.write().unwrap() = path_buf;

    log::info!("数据目录已更改为: {}", new_path.display());
    Ok(())
}

/// Cleanup item id for AI chat records.
pub const AI_SESSION_ITEM_ID: &str = "ai_sessions";

/// Build the "AI chat records" cleanup item.
///
/// Not in the static whitelist above: its path depends on the "AI data directory" setting and may be outside the main data dir,
/// while whitelist entries are fixed paths relative to the main data dir. Handled as a dynamic item, same as disk snapshots —
/// the key is to **use the same resolution path as persistence** (ai::session::sessions_root),
/// otherwise you'd get "the item shows up but clicking it deletes nothing".
fn build_ai_session_item() -> ClearableDataItem {
    let root = crate::ai::sessions_root();
    let (file_count, size) = measure_directory(&root);
    ClearableDataItem {
        id: AI_SESSION_ITEM_ID.to_string(),
        label: "AI 对话记录".to_string(),
        description: "AI 助手在各模块里的对话历史，按模块与日期保存。".to_string(),
        path: root.to_string_lossy().to_string(),
        item_type: "directory".to_string(),
        exists: root.is_dir(),
        file_count,
        size,
        // This is content the user chatted themselves, not a cache; the warning must make clear it's "unrecoverable"
        warning: Some("对话记录属于个人数据，删除后无法恢复。服务商与密钥等配置不受影响。".to_string()),
    }
}

/// Recursively count files and total bytes under a directory. Unreadable directories count as 0, so a stats failure won't block the list.
fn measure_directory(path: &Path) -> (usize, u64) {
    let mut files = 0usize;
    let mut bytes = 0u64;
    let Ok(entries) = fs::read_dir(path) else {
        return (0, 0);
    };
    for entry in entries.filter_map(|item| item.ok()) {
        let child = entry.path();
        if child.is_dir() {
            let (child_files, child_bytes) = measure_directory(&child);
            files += child_files;
            bytes += child_bytes;
        } else if let Ok(metadata) = entry.metadata() {
            files += 1;
            bytes += metadata.len();
        }
    }
    (files, bytes)
}

/// Delete AI chat records (the whole sessions directory).
fn clear_ai_sessions() -> Result<(usize, u64), String> {
    let root = crate::ai::sessions_root();
    if !root.is_dir() {
        return Ok((0, 0));
    }
    let (file_count, total_size) = measure_directory(&root);
    fs::remove_dir_all(&root)
        .map_err(|error| format!("删除 AI 对话记录失败 {}: {}", root.display(), error))?;
    Ok((file_count, total_size))
}

pub fn list_clearable_data_items() -> Result<Vec<ClearableDataItem>, String> {
    let data_dir = get_data_dir();
    let mut items = CLEARABLE_DATA_DEFINITIONS
        .iter()
        .map(|definition| build_clearable_data_item(&data_dir, definition))
        .collect::<Result<Vec<_>, _>>()?;
    items.extend(build_disk_growth_snapshot_items(&data_dir)?);
    // AI chat records: the path may be outside the main data dir, so add it as a dynamic item
    items.push(build_ai_session_item());
    Ok(items)
}

pub fn clear_selected_local_data(item_ids: &[String]) -> Result<ClearLocalDataResult, String> {
    let data_dir = get_data_dir();
    let mut result = ClearLocalDataResult {
        deleted_files: 0,
        freed_bytes: 0,
        cleared_items: 0,
        failed_items: Vec::new(),
    };

    // Isolate each item: one failure (locked file, insufficient permission, abnormal dir structure) only records the reason and does not abort later items.
    // Previously every item used `?` to propagate, so if the user checked 5 items and the 1st was locked, the other 4 were effectively wasted.
    for item_id in item_ids {
        match clear_single_item(item_id, &data_dir) {
            Ok((deleted_files, deleted_bytes)) => {
                result.deleted_files += deleted_files;
                result.freed_bytes += deleted_bytes;
                result.cleared_items += 1;
            }
            Err(reason) => {
                log::warn!("清理本地数据项失败 {}: {}", item_id, reason);
                result.failed_items.push(ClearLocalDataFailure {
                    item: item_id.clone(),
                    reason,
                });
            }
        }
    }

    Ok(result)
}

/// Clean a single data item, returning (file count, bytes).
///
/// The three kinds of items come from different places: AI sessions use a dynamic path, disk snapshots are sharded by drive letter, the rest use the whitelist.
/// Only after unifying to "one call per item" can the upper layer isolate failures and aggregate statistics per item.
fn clear_single_item(item_id: &str, data_dir: &Path) -> Result<(usize, u64), String> {
    // AI chat records are a dynamic item (path depends on the AI data directory setting), handled before the whitelist
    if item_id == AI_SESSION_ITEM_ID {
        return clear_ai_sessions();
    }

    if let Some(drive_letter) = parse_disk_growth_snapshot_item_id(item_id) {
        let snapshot_dir = data_dir.join(DISK_GROWTH_SNAPSHOT_DIR);
        return clear_disk_growth_snapshots_for_drive(&snapshot_dir, drive_letter);
    }

    let definition = CLEARABLE_DATA_DEFINITIONS
        .iter()
        .find(|definition| definition.id == item_id)
        .ok_or_else(|| format!("未知的本地数据清理项: {}", item_id))?;

    let target_path = data_dir.join(definition.relative_path);
    clear_data_item(definition, &target_path)
}

/// Clear local data: kept for old-command compatibility, cleans all whitelist items at once.
pub fn clear_local_data() -> Result<(usize, u64), String> {
    let item_ids = list_clearable_data_items()?
        .into_iter()
        .map(|item| item.id)
        .collect::<Vec<_>>();
    let result = clear_selected_local_data(&item_ids)?;
    Ok((result.deleted_files, result.freed_bytes))
}

fn build_clearable_data_item(
    data_dir: &Path,
    definition: &ClearableDataDefinition,
) -> Result<ClearableDataItem, String> {
    let target_path = data_dir.join(definition.relative_path);
    let (file_count, size) = match definition.item_type {
        ClearableDataType::File => file_usage(&target_path),
        ClearableDataType::DirectoryContents => {
            directory_contents_usage(&target_path, definition.excluded_relative_paths)?
        }
    };

    Ok(ClearableDataItem {
        id: definition.id.to_string(),
        label: definition.label.to_string(),
        description: definition.description.to_string(),
        path: target_path.to_string_lossy().to_string(),
        item_type: match definition.item_type {
            ClearableDataType::File => "file",
            ClearableDataType::DirectoryContents => "directory",
        }
        .to_string(),
        exists: target_path.exists(),
        file_count,
        size,
        warning: definition.warning.map(str::to_string),
    })
}

fn build_disk_growth_snapshot_items(data_dir: &Path) -> Result<Vec<ClearableDataItem>, String> {
    let snapshot_dir = data_dir.join(DISK_GROWTH_SNAPSHOT_DIR);
    if !snapshot_dir.is_dir() {
        return Ok(vec![empty_disk_growth_snapshot_item(&snapshot_dir)]);
    }

    let mut drives = std::collections::BTreeSet::new();
    for entry_res in fs::read_dir(&snapshot_dir).map_err(|e| {
        format!(
            "读取磁盘变化分析快照目录失败 {}: {}",
            snapshot_dir.display(),
            e
        )
    })? {
        let entry = entry_res.map_err(|e| {
            format!(
                "读取磁盘变化分析快照条目失败 {}: {}",
                snapshot_dir.display(),
                e
            )
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if let Some(letter) = disk_growth_snapshot_drive_from_name(&name) {
            drives.insert(letter);
        }
    }

    if drives.is_empty() {
        return Ok(vec![empty_disk_growth_snapshot_item(&snapshot_dir)]);
    }

    // Only show drive letters that actually have snapshots, so selecting all by default won't conjure a batch of empty drive entries.
    drives
        .into_iter()
        .map(|drive_letter| build_disk_growth_snapshot_item(&snapshot_dir, drive_letter))
        .collect()
}

fn empty_disk_growth_snapshot_item(snapshot_dir: &Path) -> ClearableDataItem {
    ClearableDataItem {
        id: format!("{}c", DISK_GROWTH_SNAPSHOT_ID_PREFIX),
        label: "C 盘磁盘变化分析快照".to_string(),
        description: "用于 C 盘磁盘变化分析的增长对比基线和分片明细。".to_string(),
        path: snapshot_dir.to_string_lossy().to_string(),
        item_type: "directory".to_string(),
        exists: snapshot_dir.exists(),
        file_count: 0,
        size: 0,
        warning: Some(
            "可安全清理；下次对应磁盘分析会重新建立基线，第二次扫描后才会重新显示变化对比。"
                .to_string(),
        ),
    }
}

fn build_disk_growth_snapshot_item(
    snapshot_dir: &Path,
    drive_letter: char,
) -> Result<ClearableDataItem, String> {
    let (file_count, size) = disk_growth_snapshot_usage_for_drive(snapshot_dir, drive_letter)?;
    let drive_label = format!("{} 盘", drive_letter.to_ascii_uppercase());
    Ok(ClearableDataItem {
        id: format!(
            "{}{}",
            DISK_GROWTH_SNAPSHOT_ID_PREFIX,
            drive_letter.to_ascii_lowercase()
        ),
        label: format!("{}磁盘变化分析快照", drive_label),
        description: format!("用于 {}磁盘变化分析的增长对比基线和分片明细。", drive_label),
        path: snapshot_dir.to_string_lossy().to_string(),
        item_type: "directory".to_string(),
        exists: snapshot_dir.is_dir(),
        file_count,
        size,
        warning: Some(
            "可安全清理；下次对应磁盘分析会重新建立基线，第二次扫描后才会重新显示变化对比。"
                .to_string(),
        ),
    })
}

fn clear_data_item(
    definition: &ClearableDataDefinition,
    target_path: &Path,
) -> Result<(usize, u64), String> {
    match definition.item_type {
        ClearableDataType::File => clear_file(target_path),
        ClearableDataType::DirectoryContents => {
            // The data directory entry itself is reused by the app; only clear its contents, so later log/snapshot writes don't need to recreate the parent dir.
            if !target_path.exists() {
                return Ok((0, 0));
            }
            if !target_path.is_dir() {
                return Err(format!("清理项不是目录: {}", target_path.display()));
            }
            let result = clear_directory_contents(target_path, definition.excluded_relative_paths)?;
            log::info!("已清空本地数据目录: {}", definition.relative_path);
            Ok(result)
        }
    }
}

fn clear_file(path: &Path) -> Result<(usize, u64), String> {
    if !path.exists() {
        return Ok((0, 0));
    }
    if !path.is_file() {
        return Err(format!("清理项不是文件: {}", path.display()));
    }

    let size = fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    fs::remove_file(path).map_err(|e| format!("删除文件 {} 失败: {}", path.display(), e))?;
    Ok((1, size))
}

fn clear_disk_growth_snapshots_for_drive(
    snapshot_dir: &Path,
    drive_letter: char,
) -> Result<(usize, u64), String> {
    if !snapshot_dir.is_dir() {
        return Ok((0, 0));
    }

    let mut file_count = 0usize;
    let mut total_size = 0u64;
    for path in disk_growth_snapshot_paths_for_drive(snapshot_dir, drive_letter)? {
        // Shard directories share the name and drive letter with the main snapshot file; they must be deleted as a group, otherwise later detail queries see partial data.
        if path.is_dir() {
            let (child_files, child_bytes) = directory_usage(&path, &[])?;
            file_count += child_files;
            total_size += child_bytes;
            fs::remove_dir_all(&path)
                .map_err(|e| format!("删除快照分片目录 {} 失败: {}", path.display(), e))?;
        } else if path.is_file() {
            if let Ok(meta) = fs::metadata(&path) {
                total_size += meta.len();
            }
            fs::remove_file(&path)
                .map_err(|e| format!("删除快照文件 {} 失败: {}", path.display(), e))?;
            file_count += 1;
        }
    }

    log::info!(
        "已清空 {} 盘磁盘变化分析快照",
        drive_letter.to_ascii_uppercase()
    );
    Ok((file_count, total_size))
}

fn disk_growth_snapshot_usage_for_drive(
    snapshot_dir: &Path,
    drive_letter: char,
) -> Result<(usize, u64), String> {
    let mut file_count = 0usize;
    let mut total_size = 0u64;
    for path in disk_growth_snapshot_paths_for_drive(snapshot_dir, drive_letter)? {
        if path.is_dir() {
            let (child_files, child_bytes) = directory_usage(&path, &[])?;
            file_count += child_files;
            total_size += child_bytes;
        } else if path.is_file() {
            if let Ok(meta) = fs::metadata(&path) {
                total_size += meta.len();
            }
            file_count += 1;
        }
    }
    Ok((file_count, total_size))
}

fn disk_growth_snapshot_paths_for_drive(
    snapshot_dir: &Path,
    drive_letter: char,
) -> Result<Vec<PathBuf>, String> {
    if !snapshot_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut paths = Vec::new();
    for entry_res in fs::read_dir(snapshot_dir).map_err(|e| {
        format!(
            "读取磁盘变化分析快照目录失败 {}: {}",
            snapshot_dir.display(),
            e
        )
    })? {
        let entry = entry_res.map_err(|e| {
            format!(
                "读取磁盘变化分析快照条目失败 {}: {}",
                snapshot_dir.display(),
                e
            )
        })?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if disk_growth_snapshot_drive_from_name(name) == Some(drive_letter.to_ascii_uppercase()) {
            paths.push(path);
        }
    }
    Ok(paths)
}

fn disk_growth_snapshot_drive_from_name(name: &str) -> Option<char> {
    let lower_name = name.to_ascii_lowercase();
    // The C drive keeps its historical file name without a drive prefix; other drives use prefixes like d_/e_.
    if lower_name.starts_with(DISK_GROWTH_SNAPSHOT_BASE_PREFIX)
        && (lower_name.ends_with(DISK_GROWTH_SNAPSHOT_JSON_SUFFIX)
            || lower_name.ends_with(DISK_GROWTH_SNAPSHOT_SHARD_SUFFIX))
    {
        return Some('C');
    }

    let mut chars = lower_name.chars();
    let letter = chars.next()?;
    if !letter.is_ascii_lowercase() || chars.next()? != '_' {
        return None;
    }

    let rest = &lower_name[2..];
    if rest.starts_with(DISK_GROWTH_SNAPSHOT_BASE_PREFIX)
        && (rest.ends_with(DISK_GROWTH_SNAPSHOT_JSON_SUFFIX)
            || rest.ends_with(DISK_GROWTH_SNAPSHOT_SHARD_SUFFIX))
    {
        Some(letter.to_ascii_uppercase())
    } else {
        None
    }
}

fn parse_disk_growth_snapshot_item_id(item_id: &str) -> Option<char> {
    if item_id == DISK_GROWTH_SNAPSHOT_BASE_ID {
        return Some('C');
    }

    let suffix = item_id.strip_prefix(DISK_GROWTH_SNAPSHOT_ID_PREFIX)?;
    let mut chars = suffix.chars();
    let letter = chars.next()?;
    if chars.next().is_none() && letter.is_ascii_alphabetic() {
        Some(letter.to_ascii_uppercase())
    } else {
        None
    }
}

fn file_usage(path: &Path) -> (usize, u64) {
    if !path.is_file() {
        return (0, 0);
    }

    let size = fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    (1, size)
}

fn directory_contents_usage(
    dir: &Path,
    excluded_relative_paths: &[&str],
) -> Result<(usize, u64), String> {
    if !dir.exists() {
        return Ok((0, 0));
    }
    if !dir.is_dir() {
        return Ok((0, 0));
    }

    directory_usage(dir, excluded_relative_paths)
}

/// Clear all contents of a directory but keep the directory itself, so fixed entries like the log dir don't need recreating after deletion.
fn clear_directory_contents(
    dir: &Path,
    excluded_relative_paths: &[&str],
) -> Result<(usize, u64), String> {
    let mut file_count = 0usize;
    let mut total_size = 0u64;

    for entry_res in
        fs::read_dir(dir).map_err(|e| format!("读取目录失败 {}: {}", dir.display(), e))?
    {
        let entry = entry_res.map_err(|e| format!("读取目录条目失败 {}: {}", dir.display(), e))?;
        let path = entry.path();
        // Virtual disk data shown as its own item is handled by a dedicated cleanup item; the parent only counts the rest.
        if entry.file_name().to_str().is_some_and(|name| {
            excluded_relative_paths
                .iter()
                .any(|excluded| *excluded == name)
        }) {
            continue;
        }
        // One metadata call gets both "is it a directory" and "file size":
        // previously is_dir() + is_file() + metadata() were three syscalls, pure waste on lots of tiny files.
        // Semantics unchanged — metadata follows symlinks, consistent with the old is_dir()/is_file() handling.
        let Ok(metadata) = fs::metadata(&path) else {
            // Deleted or permission-denied during stats: skip this entry without affecting others in the same directory
            continue;
        };
        if metadata.is_dir() {
            let (child_files, child_bytes) = directory_usage(&path, &[])?;
            file_count += child_files;
            total_size += child_bytes;
            fs::remove_dir_all(&path)
                .map_err(|e| format!("删除目录 {} 失败: {}", path.display(), e))?;
        } else if metadata.is_file() {
            total_size += metadata.len();
            fs::remove_file(&path)
                .map_err(|e| format!("删除文件 {} 失败: {}", path.display(), e))?;
            file_count += 1;
        }
    }

    Ok((file_count, total_size))
}

/// Before deleting a directory, count files and space first so the UI's freed amount includes snapshot shards in nested dirs.
fn directory_usage(dir: &Path, excluded_relative_paths: &[&str]) -> Result<(usize, u64), String> {
    let mut file_count = 0usize;
    let mut total_size = 0u64;

    for entry_res in
        fs::read_dir(dir).map_err(|e| format!("统计目录失败 {}: {}", dir.display(), e))?
    {
        let entry = entry_res.map_err(|e| format!("统计目录条目失败 {}: {}", dir.display(), e))?;
        let path = entry.path();
        // Here we only need to filter this directory's own independently-managed children; nested dirs are handled wholesale by their own category.
        if entry.file_name().to_str().is_some_and(|name| {
            excluded_relative_paths
                .iter()
                .any(|excluded| *excluded == name)
        }) {
            continue;
        }
        // Same as clear_directory_contents: one metadata call replaces is_dir + is_file + metadata
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            let (child_files, child_bytes) = directory_usage(&path, &[])?;
            file_count += child_files;
            total_size += child_bytes;
        } else if metadata.is_file() {
            total_size += metadata.len();
            file_count += 1;
        }
    }

    Ok((file_count, total_size))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{installer_root_dir, portable_root_dir_for};

    #[test]
    fn test_default_dir_exists() {
        let dir = get_data_dir();
        // If the directory doesn't exist, at least the path should be creatable
        assert!(!dir.as_os_str().is_empty());
    }

    #[test]
    fn test_get_default_dir_not_empty() {
        let dir = get_default_dir();
        assert!(!dir.as_os_str().is_empty());
    }

    #[test]
    fn excludes_virtual_disk_entries_from_parent_usage() {
        let unique_suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("获取测试时间失败")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "lightc-data-dir-test-{}-{}",
            std::process::id(),
            unique_suffix
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("shell_icons")).expect("创建测试目录失败");
        fs::write(root.join("cleanup.json"), "cleanup").expect("创建通用日志失败");
        fs::write(root.join("shell_icons.log"), "shell icon").expect("创建专属日志失败");
        fs::write(root.join("shell_icons").join("backup.reg"), "backup").expect("创建专属备份失败");

        // General log stats exclude the dedicated log but not other unrelated subdirectories.
        let usage = directory_usage(&root, &["shell_icons.log"]).expect("统计测试目录失败");
        assert_eq!(usage.0, 2);
        assert_eq!(usage.1, "cleanup".len() as u64 + "backup".len() as u64);

        // General backup stats exclude the virtual-disk-specific directory to avoid double counting with the dedicated backup item.
        let backup_usage = directory_usage(&root, &["shell_icons"]).expect("统计备份测试目录失败");
        assert_eq!(backup_usage.0, 2);
        assert_eq!(
            backup_usage.1,
            "cleanup".len() as u64 + "shell icon".len() as u64
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn probe_detects_writable_directory() {
        let root = std::env::temp_dir().join(format!(
            "lightc-write-probe-ok-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("获取测试时间失败")
                .as_nanos()
        ));

        // A non-existent directory should also pass (the probe creates it first), and no probe file may be left behind.
        assert!(probe_writable_directory(&root.join("nested")).is_ok());
        let leftovers = fs::read_dir(&root)
            .expect("读取测试目录失败")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".lightc-write-probe-"))
            .count();
        assert_eq!(leftovers, 0, "写入探针必须在检测后删除自身");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn probe_rejects_file_as_directory() {
        // A path occupied by a file of the same name must error, otherwise later log/snapshot writes would fail at runtime.
        let root = std::env::temp_dir().join(format!(
            "lightc-write-probe-file-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("获取测试时间失败")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("创建测试目录失败");
        let blocked = root.join("blocked");
        fs::write(&blocked, b"not a directory").expect("创建占位文件失败");

        assert!(probe_writable_directory(&blocked).is_err());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn installer_channel_never_reports_portable_fallback() {
        // The installer writes AppData by design; the diagnostic must not mark it as "portable fallback".
        let diagnostic = get_storage_write_diagnostic();
        let channel = current_distribution_channel();
        if channel == DistributionChannel::Installer {
            assert!(diagnostic.portable_root.is_none());
            assert!(!diagnostic.fell_back_to_app_data);
            assert!(diagnostic.preferred_writable);
        } else {
            // Portable: the expected directory must be writable; only "not writable + actually using AppData" counts as a fallback,
            // old config or user-custom directories must not be misreported.
            assert_eq!(
                path_compare_key(Path::new(&diagnostic.preferred_data_directory)),
                path_compare_key(&get_default_dir())
            );
            assert_eq!(diagnostic.preferred_writable, !diagnostic.fell_back_to_app_data);
            if diagnostic.fell_back_to_app_data {
                assert!(diagnostic.reason.is_some(), "回落时必须给出可展示原因");
            }
        }
    }

    #[test]
    fn separates_config_and_default_data_paths() {
        // Portable: both config and default data must follow the exe directory, landing under config/ and data/ respectively.
        let portable_root = PathBuf::from(r"D:\LightCPortable");
        let config_path = portable_root.join(CONFIG_DIR_NAME).join(CONFIG_FILE);
        let default_dir = portable_root.join(DEFAULT_DATA_DIR_NAME);

        assert_eq!(
            path_compare_key(&config_path),
            path_compare_key(&PathBuf::from(r"D:\LightCPortable\config\config.json"))
        );
        assert_eq!(
            path_compare_key(&default_dir),
            path_compare_key(&PathBuf::from(r"D:\LightCPortable\data"))
        );

        // Installer: both are under %LOCALAPPDATA%\LightC, so NSIS update/uninstall won't touch data.
        let installer_root = installer_root_dir().expect("安装版根目录应可用");
        assert_eq!(
            path_compare_key(&installer_root.join(CONFIG_DIR_NAME).join(CONFIG_FILE)),
            path_compare_key(&installer_root.join("config").join("config.json"))
        );
        assert!(
            path_compare_key(&installer_root).ends_with("\\lightc"),
            "安装版根目录应位于 LocalAppData\\LightC: {}",
            installer_root.display()
        );
    }

    #[test]
    fn portable_root_follows_executable_directory() {
        // The portable root must equal the exe's directory — a prerequisite for "data travels with the package".
        let executable = PathBuf::from(r"E:\USB\LightC\LightC.exe");
        assert_eq!(
            path_compare_key(&portable_root_dir_for(&executable).expect("便携版根目录应可用")),
            path_compare_key(&PathBuf::from(r"E:\USB\LightC"))
        );
    }

    #[test]
    fn normalizes_legacy_default_root_to_data_subdir() {
        let legacy_root = app_local_root_dir().expect("app local root should be available");
        let default_dir = default_data_dir().expect("default data dir should be available");

        let normalized =
            normalize_legacy_default_data_dir_inner(&legacy_root, &legacy_root, &default_dir);

        assert_eq!(
            path_compare_key(&normalized),
            path_compare_key(&default_dir)
        );
    }

    #[test]
    fn portable_relative_config_follows_current_default_directory() {
        let default_dir = PathBuf::from(r"D:\LightC\data");
        let config = DataDirConfig {
            data_dir: DEFAULT_DATA_DIR_NAME.to_string(),
            data_dir_mode: Some("relative".to_string()),
        };

        let (resolved, is_legacy_default) =
            resolve_configured_data_dir(&config, &default_dir, DistributionChannel::Portable);

        assert_eq!(path_compare_key(&resolved), path_compare_key(&default_dir));
        assert!(is_legacy_default);
    }

    #[test]
    fn portable_custom_config_remains_absolute() {
        let default_dir = PathBuf::from(r"D:\LightC\data");
        let custom_dir = PathBuf::from(r"E:\LightCData");
        let config = DataDirConfig {
            data_dir: custom_dir.to_string_lossy().to_string(),
            data_dir_mode: None,
        };

        let (resolved, is_legacy_default) =
            resolve_configured_data_dir(&config, &default_dir, DistributionChannel::Portable);

        assert_eq!(path_compare_key(&resolved), path_compare_key(&custom_dir));
        assert!(!is_legacy_default);
    }

    #[test]
    fn rejects_nested_migration_target() {
        let old_path = Path::new(r"C:\Users\tester\AppData\Local\LightC");
        let new_path = old_path.join("LightC_Data");

        let result = ensure_migration_target_is_safe(old_path, &new_path);

        assert!(result.is_err());
    }

    #[test]
    fn rejects_non_empty_migration_target() {
        let root =
            std::env::temp_dir().join(format!("lightc-data-dir-test-{}", std::process::id()));
        let old_path = root.join("old");
        let new_path = root.join("new");
        fs::create_dir_all(&old_path).unwrap();
        fs::create_dir_all(&new_path).unwrap();
        fs::write(new_path.join("other.txt"), "not lightc data").unwrap();

        let result = ensure_migration_target_is_safe(&old_path, &new_path);

        let _ = fs::remove_dir_all(&root);
        assert!(result.is_err());
    }

    #[test]
    fn migrates_only_owned_data_entries() {
        let root = std::env::temp_dir().join(format!(
            "lightc-owned-migration-test-{}",
            std::process::id()
        ));
        let old_path = root.join("old");
        let new_path = root.join("new");
        fs::create_dir_all(old_path.join("logs")).unwrap();
        fs::write(old_path.join("logs").join("cleanup.json"), "{}").unwrap();
        fs::write(old_path.join("install_history.json"), "[]").unwrap();
        fs::write(old_path.join("unrelated.txt"), "keep out").unwrap();

        let result = migrate_owned_data_entries(&old_path, &new_path);

        assert!(result.is_ok());
        assert!(new_path.join("logs").join("cleanup.json").is_file());
        assert!(new_path.join("install_history.json").is_file());
        assert!(!new_path.join("unrelated.txt").exists());

        let _ = fs::remove_dir_all(&root);
    }

    /// The counting rule must cover nested directories and exclude excluded items.
    ///
    /// This guards the "one metadata call for type and size" change: after switching to metadata, the way directories and files are judged changed,
    /// but the counting result must not change with it.
    #[test]
    fn counts_nested_files_and_skips_excluded_entries() {
        let root = std::env::temp_dir().join(format!(
            "lightc-directory-usage-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("获取测试时间失败")
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("nested").join("deep")).expect("创建测试目录失败");
        fs::write(root.join("top.bin"), [0u8; 10]).expect("写入测试文件失败");
        fs::write(root.join("nested").join("mid.bin"), [0u8; 20]).expect("写入测试文件失败");
        fs::write(root.join("nested").join("deep").join("leaf.bin"), [0u8; 30])
            .expect("写入测试文件失败");

        let (files, bytes) = directory_usage(&root, &[]).expect("统计失败");
        assert_eq!(files, 3, "嵌套三层的文件都要统计到");
        assert_eq!(bytes, 60, "字节数应与实际大小一致");

        // When a demo directory is a separate cleanup item, parent stats must exclude it
        let (files, bytes) = directory_usage(&root, &["nested"]).expect("统计失败");
        assert_eq!(files, 1);
        assert_eq!(bytes, 10);

        let _ = fs::remove_dir_all(&root);
    }

    /// An unknown cleanup item must return an error rather than panic — per-item cleanup relies on this error to continue to the next item.
    #[test]
    fn unknown_clear_item_returns_error() {
        let data_dir = std::env::temp_dir();
        let error = clear_single_item("not-a-real-item", &data_dir).expect_err("未知项必须报错");
        assert!(error.contains("not-a-real-item"), "错误信息要带上出错的项");
    }
}
