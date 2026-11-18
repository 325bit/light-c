// ============================================================================
// Engine selector — automatically picks the best scanning engine based on runtime environment and filesystem type
//
// Decision tree:
//   is_elevated() && is_ntfs(drive) → direct MFT read (full disk in seconds)
//   otherwise                       → jwalk fallback (traditional traversal)
//
// Automatically falls back to jwalk when MFT fails, so the feature always works
// ============================================================================

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use log::{info, warn};

use crate::scanner::hotspot::FolderStats;

use super::mft_scanner::MftScanProgress;

/// Scan backend type (used by the frontend to show the engine currently in use)
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HotspotBackend {
    /// Direct MFT read (admin + NTFS)
    Mft,
    /// jwalk traversal (fallback)
    Walkdir,
}

impl HotspotBackend {
    /// Label for frontend display
    pub fn label(&self) -> &'static str {
        match self {
            HotspotBackend::Mft => "MFT 直读",
            HotspotBackend::Walkdir => "常规遍历",
        }
    }
}

/// Detect the backend to use in the current environment
pub fn detect_backend(drive_letter: char) -> HotspotBackend {
    #[cfg(windows)]
    {
        let elevated = super::mft_scanner::is_elevated();
        let ntfs = super::mft_scanner::is_ntfs(drive_letter);
        info!(
            "[引擎选择] 管理员: {}, NTFS: {} → {}",
            elevated,
            ntfs,
            if elevated && ntfs {
                "MFT 直读"
            } else {
                "常规遍历"
            }
        );
        if elevated && ntfs {
            return HotspotBackend::Mft;
        }
    }
    HotspotBackend::Walkdir
}

/// Scan the given drive, automatically selecting the engine
///
/// On MFT failure, automatically degrades to jwalk to keep the feature working.
///
/// # Returns
/// - `HashMap<PathBuf, FolderStats>` — full directory path → aggregated stats
/// - `HotspotBackend` — the engine actually used
pub fn scan_full_drive(
    drive_letter: char,
    root: &Path,
    max_depth: u8,
    track_modified: bool,
    cancel_flag: &AtomicBool,
    progress_cb: impl Fn(MftScanProgress) + Sync,
) -> (HashMap<PathBuf, FolderStats>, HotspotBackend) {
    let backend = detect_backend(drive_letter);

    match backend {
        HotspotBackend::Mft => {
            info!("[引擎选择] 使用 MFT 直读引擎扫描 {}:", drive_letter);
            match super::mft_scanner::scan_via_mft(drive_letter, &progress_cb) {
                Ok(map) => {
                    info!("[引擎选择] MFT 扫描成功，{} 个目录", map.len());
                    (map, HotspotBackend::Mft)
                }
                Err(e) => {
                    warn!("[引擎选择] MFT 扫描失败，自动降级到 jwalk: {}", e);
                    let (_, map) = super::fallback_scanner::aggregate_ancestor_stats(
                        root,
                        max_depth,
                        track_modified,
                        cancel_flag,
                        true, // ignore system directories when falling back, to keep performance
                    );
                    (map, HotspotBackend::Walkdir)
                }
            }
        }
        HotspotBackend::Walkdir => {
            info!("[引擎选择] 使用 jwalk 常规遍历扫描 {}:", drive_letter);
            let (_, map) = super::fallback_scanner::aggregate_ancestor_stats(
                root,
                max_depth,
                track_modified,
                cancel_flag,
                true,
            );
            (map, HotspotBackend::Walkdir)
        }
    }
}
