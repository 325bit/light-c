// ============================================================================
// C drive cleanup tool - main entry point
// Windows-only smart disk cleanup tool
// ============================================================================

// Module declarations
mod ai;
mod ai_models;
mod cleaner;
mod commands;
mod data_dir;
mod disk_growth;
mod disk_health;
mod driver_cleanup;
mod health_score;
mod logger;
mod runtime;
mod scanner;
mod system_info;
mod system_slim;

// Export command modules
use commands::*;

// ============================================================================
/// Export layer for performance benchmarks
///
/// `benches/` is a separate crate and can only see `pub` items here. Keeping business modules
/// private is deliberate, so this module re-exports only the stable functions the benchmarks
/// actually need. Everything else stays closed; `#[doc(hidden)]` keeps this layer out of public docs.
pub mod bench_api {
    pub use crate::cleaner::DeleteEngine;
    pub use crate::scanner::deep_junk::{could_be_junk_path, normalize_into};
}

// ============================================================================
/// Application entry point
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialize logging
    env_logger::init();

    // The portable build must specify an absolute WebView2 data directory before Tauri auto-creates the window,
    // otherwise localStorage keeps landing in AppData and settings will not follow when the portable package is moved.
    let portable_webview_data_directory = runtime::prepare_portable_webview_data_directory();
    // If the last run was force-closed, a DISM process still modifying component storage may remain; kill it at startup.
    system_slim::cleanup_orphan_dism_process();
    // In dev builds, log the actually effective storage path to distinguish whether "where data went" is due to mode detection or a custom directory.
    #[cfg(debug_assertions)]
    data_dir::log_storage_diagnostics();
    let mut context = tauri::generate_context!();
    if portable_webview_data_directory.is_some() {
        for window_config in &mut context.config_mut().app.windows {
            window_config.create = false;
        }
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(move |app| {
            if let Some(webview_data_directory) = portable_webview_data_directory.clone() {
                let window_configs = app.config().app.windows.clone();
                for window_config in window_configs {
                    tauri::WebviewWindowBuilder::from_config(app.handle(), &window_config)?
                        .data_directory(webview_data_directory.join(&window_config.label))
                        .build()?;
                }
            }
            // Wallpapers saved to the data directory load via the asset protocol; scope does not persist across processes, so re-authorize at startup.
            commands::data::authorize_wallpaper(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Disk info
            get_disk_info,
            get_local_drives,
            get_disk_health,
            // Scan-related
            scan_junk_files,
            scan_deep_junk_files,
            cancel_deep_junk_scan,
            get_deep_junk_category_page,
            scan_category,
            scan_large_files,
            cancel_large_file_scan,
            scan_social_cache,
            scan_ai_tool_caches,
            delete_ai_tool_caches,
            list_ai_cache_files,
            get_categories,
            // Delete-related
            delete_files,
            // Utility functions
            format_size,
            open_disk_cleanup,
            open_in_folder,
            open_file,
            open_recycle_bin,
            // System slim
            check_admin_privilege,
            relaunch_as_admin,
            get_system_slim_status,
            disable_hibernation,
            enable_hibernation,
            cleanup_winsxs,
            cleanup_winsxs_resetbase,
            open_virtual_memory_settings,
            rebuild_search_index,
            // Old driver cleanup
            scan_old_drivers,
            delete_old_drivers,
            restore_all_driver_backups,
            open_driver_backup_dir,
            // Health scoring
            get_health_score,
            // Uninstall leftovers and registry cleanup
            scan_uninstall_leftovers,
            delete_leftover_folders,
            list_leftover_whitelist,
            add_leftover_whitelist_entry,
            remove_leftover_whitelist_entry,
            scan_registry_redundancy,
            delete_registry_entries,
            open_registry_backup_dir,
            // Enhanced deletion
            enhanced_delete_files,
            delete_deep_junk_files,
            get_physical_size,
            check_admin_for_path,
            // Permanent delete (deep cleanup)
            delete_leftovers_permanent,
            check_leftover_safety,
            // Dev caches, other software caches and version leftovers (aligned with cscan's scan coverage)
            scan_dev_cache,
            scan_version_leftovers,
            scan_other_app_caches,
            delete_dev_cache_paths,
            // System info
            get_system_info,
            get_distribution_channel,
            verify_integrity,
            // Cleanup log
            record_cleanup_action,
            open_logs_folder,
            get_cleanup_history,
            // C drive hotspot scan
            scan_hotspot,
            cancel_hotspot_scan,
            scan_path_direct,
            cleanup_directory_contents,
            // Context menu cleanup
            scan_context_menu,
            delete_context_menu_entries,
            open_context_menu_backup_dir,
            // System quick tools
            open_startup_manager,
            open_storage_settings,
            // C drive full-disk change analysis
            scan_disk_growth,
            cancel_disk_growth_scan,
            get_disk_growth_export_tree,
            save_disk_growth_html,
            get_disk_growth_file_details,
            get_disk_growth_directory_details,
            // Data directory management
            get_data_directory,
            get_storage_location_info,
            get_storage_write_diagnostic,
            migrate_legacy_portable_data,
            set_data_directory,
            clear_local_data,
            list_clearable_data_items,
            clear_selected_local_data,
            pick_folder_dialog,
            pick_wallpaper_dialog,
            install_wallpaper,
            clear_wallpaper,
            get_wallpaper_path,
            // AI asset analysis
            scan_ai_model_assets,
            delete_ai_model,
            // Shell icon cleanup
            scan_shell_icons,
            remove_shell_icon,
            unlock_shell_icon,
            restore_shell_icon,
            restart_explorer,
            open_shell_icon_backup_dir,
            open_shell_icon_log,
            open_shell_icon_registry,
            // AI assistant (cross-cutting addition: only adds commands, does not change existing module logic)
            list_ai_providers,
            get_ai_config,
            save_ai_profile,
            delete_ai_profile,
            clear_ai_profile_key,
            set_active_ai_profile,
            set_ai_data_dir,
            test_ai_connection,
            ai_chat,
            ai_chat_stream,
            save_ai_attachment,
            read_ai_attachment,
            append_ai_session_message,
            load_ai_session,
            load_ai_session_range,
            clear_ai_module_session,
            clear_ai_session,
        ])
        .run(context)
        .expect("启动应用程序时发生错误");
}
