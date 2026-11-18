// ============================================================================
// Data directory management commands
// ============================================================================

use crate::error::{AppError, ErrorCode};
use std::fs;
use std::path::{Path, PathBuf};
use tauri::Manager;

/// Fixed file name of the wallpaper in the data directory (without extension).
/// Use a fixed name rather than the original: after selection it is copied into the data directory, so deleting/moving the original does not matter.
/// Keep the real extension — WebView relies on it to determine the MIME type of asset:// resources,
/// changing to a uniform .img would make the image fail to load.
const WALLPAPER_FILE_STEM: &str = "wallpaper";

/// Allowed wallpaper extensions. Judge by extension rather than reading the file header: WebView needs the correct extension to
/// decode an asset:// resource as an image; a renamed file will not load even if its content is valid.
const WALLPAPER_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp", "gif"];

/// Wallpaper size limit (20MB). A huge image makes WebView decode a giant picture on every startup,
/// and asset resources are loaded entirely into memory, a cost far exceeding a background image's value.
const WALLPAPER_MAX_BYTES: u64 = 20 * 1024 * 1024;

/// List installed wallpaper files in the data directory (clear the one with the old extension when changing images).
fn existing_wallpaper_paths() -> Vec<PathBuf> {
    let data_dir = crate::data_dir::get_data_dir();
    let Ok(entries) = fs::read_dir(&data_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_stem()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case(WALLPAPER_FILE_STEM))
        })
        .collect()
}

/// Absolute path of the current wallpaper; None if none installed.
fn current_wallpaper_path() -> Option<PathBuf> {
    existing_wallpaper_paths()
        .into_iter()
        .find(|path| path.is_file())
}

/// Get the current data directory path
#[tauri::command]
pub fn get_data_directory() -> Result<String, AppError> {
    Ok(crate::data_dir::get_data_dir()
        .to_string_lossy()
        .to_string())
}

/// Get the unified distribution mode, config path, data path and portable migration status.
#[tauri::command]
pub fn get_storage_location_info() -> crate::data_dir::StorageLocationInfo {
    crate::data_dir::get_storage_location_info()
}

/// Detect write capability of the portable directory, so the settings page can give clear guidance when "the portable package is in a non-writable directory".
#[tauri::command]
pub fn get_storage_write_diagnostic() -> crate::data_dir::StorageWriteDiagnostic {
    crate::data_dir::get_storage_write_diagnostic()
}

/// Retry migration of old AppData data for portable builds; source data is always preserved.
#[tauri::command]
pub fn migrate_legacy_portable_data() -> Result<crate::data_dir::StorageLocationInfo, AppError> {
    crate::data_dir::migrate_legacy_portable_data().map_err(AppError::from)
}

/// Set the data directory and migrate data
#[tauri::command]
pub fn set_data_directory(path: String) -> Result<String, AppError> {
    let new_path = std::path::Path::new(&path);
    crate::data_dir::set_data_dir(new_path)?;
    Ok(format!(
        "数据目录已更改为: {}",
        crate::data_dir::get_data_dir().display()
    ))
}

/// Clear local data whitelist entries; module-specific logs and backups are managed by the data directory's unified cleanup list.
#[tauri::command]
pub fn clear_local_data() -> Result<(usize, u64), AppError> {
    crate::data_dir::clear_local_data().map_err(AppError::from)
}

#[tauri::command]
pub fn list_clearable_data_items() -> Result<Vec<crate::data_dir::ClearableDataItem>, AppError> {
    crate::data_dir::list_clearable_data_items().map_err(AppError::from)
}

#[tauri::command]
pub fn clear_selected_local_data(
    item_ids: Vec<String>,
) -> Result<crate::data_dir::ClearLocalDataResult, AppError> {
    crate::data_dir::clear_selected_local_data(&item_ids).map_err(AppError::from)
}

/// At startup, add installed wallpapers to the asset protocol whitelist.
/// Asset scope does not persist across processes, so authorization must be redone each startup, otherwise wallpapers break after restart.
pub fn authorize_wallpaper(app: &tauri::AppHandle) {
    if let Some(path) = current_wallpaper_path() {
        if let Err(error) = app.asset_protocol_scope().allow_file(&path) {
            log::warn!("授权壁纸失败 {}: {}", path.display(), error);
        }
    }
}

/// Open the system image picker dialog.
/// Only allow common static image formats: gif is also accepted and WebView can decode it, but this is the user's own choice.
///
/// M10 fix: uses the dialog plugin's callback API instead of `blocking_pick_file`.
/// The blocking variant parks the async executor thread the command runs on until
/// the user closes the dialog; the callback wakes this command through a oneshot
/// channel instead, leaving the executor free while the dialog is open.
#[tauri::command]
pub async fn pick_wallpaper_dialog(app: tauri::AppHandle) -> Result<Option<String>, AppError> {
    use tauri_plugin_dialog::DialogExt;

    let (sender, receiver) = tokio::sync::oneshot::channel::<Option<String>>();
    app.dialog()
        .file()
        .set_title("选择背景壁纸")
        .add_filter("图片", WALLPAPER_EXTENSIONS)
        .pick_file(move |file| {
            // The dialog closes exactly once, so the receiver is always present.
            // A send error would mean the command future was dropped — nothing left to report to.
            let _ = sender.send(file.map(|path| path.to_string()));
        });

    receiver
        .await
        .map_err(|error| format!("文件对话框回调通道已关闭: {}", error))
        .map_err(AppError::from)
}

/// Install a wallpaper: validate file → copy to data directory → clear wallpaper with old extension.
/// Return an asset:// relative path for the frontend to build convertFileSrc.
#[tauri::command]
pub fn install_wallpaper(app: tauri::AppHandle, source: String) -> Result<String, AppError> {
    let source_path = Path::new(&source);
    if !source_path.is_file() {
        return Err(AppError::new(ErrorCode::NotFound).arg("path", &source));
    }

    let extension = source_path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    if !WALLPAPER_EXTENSIONS.contains(&extension.as_str()) {
        return Err(AppError::new(ErrorCode::Unsupported).arg("extension", extension));
    }

    let metadata = fs::metadata(source_path).map_err(|error| format!("读取图片失败：{}", error))?;
    if metadata.len() > WALLPAPER_MAX_BYTES {
        return Err(AppError::new(ErrorCode::TooLarge).detail("wallpaper must be <= 20 MB"));
    }

    let data_dir = crate::data_dir::get_data_dir();
    fs::create_dir_all(&data_dir).map_err(|error| format!("创建数据目录失败：{}", error))?;

    // Delete the old wallpaper before copying: when old/new extensions differ (e.g. png → jpg), the two will not coexist.
    for old_path in existing_wallpaper_paths() {
        if let Err(error) = fs::remove_file(&old_path) {
            log::warn!("删除旧壁纸失败 {}: {}", old_path.display(), error);
        }
    }

    let target = data_dir.join(format!("{}.{}", WALLPAPER_FILE_STEM, extension));
    fs::copy(source_path, &target).map_err(|error| format!("保存壁纸失败：{}", error))?;

    // The asset protocol is disabled by default; allow only this one file, do not open the whole data directory.
    app.asset_protocol_scope()
        .allow_file(&target)
        .map_err(|error| format!("授权壁纸文件失败：{}", error))?;

    Ok(target.to_string_lossy().to_string())
}

/// Clear the wallpaper: only delete the wallpaper file we installed in the data directory, do not touch other user data.
#[tauri::command]
pub fn clear_wallpaper() -> Result<(), AppError> {
    for path in existing_wallpaper_paths() {
        fs::remove_file(&path).map_err(|error| format!("删除壁纸失败：{}", error))?;
    }
    Ok(())
}

/// Read the absolute path of the current wallpaper; return an empty string if none.
#[tauri::command]
pub fn get_wallpaper_path() -> String {
    current_wallpaper_path()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Open the system folder picker dialog
///
/// M10 fix: callback API + oneshot channel instead of `blocking_pick_folder`,
/// for the same reason as `pick_wallpaper_dialog` — the blocking variant parks
/// an async executor thread for as long as the dialog stays open.
#[tauri::command]
pub async fn pick_folder_dialog(app: tauri::AppHandle) -> Result<Option<String>, AppError> {
    use tauri_plugin_dialog::DialogExt;

    let (sender, receiver) = tokio::sync::oneshot::channel::<Option<String>>();
    app.dialog()
        .file()
        .set_title("选择数据存储目录")
        .pick_folder(move |folder| {
            let _ = sender.send(folder.map(|path| path.to_string()));
        });

    receiver
        .await
        .map_err(|error| format!("文件夹对话框回调通道已关闭: {}", error))
        .map_err(AppError::from)
}
