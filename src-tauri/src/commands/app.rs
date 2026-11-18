// ============================================================================
// App runtime environment commands
// ============================================================================

pub use crate::runtime::DistributionChannel;

/// Get the current release channel.
///
/// The release mode is determined by the unified runtime resolver, ensuring updates, integrity checks, and the data directory use the same result.
#[tauri::command]
pub fn get_distribution_channel() -> DistributionChannel {
    std::env::current_exe()
        .map(|exe_path| crate::runtime::detect_distribution_channel(&exe_path))
        .unwrap_or(DistributionChannel::Installer)
}
