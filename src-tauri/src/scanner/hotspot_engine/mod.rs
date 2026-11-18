// ============================================================================
// Collection of large-directory scanning engines
// - mft_scanner:      uses direct MFT reads when running as admin for second-level full-disk scans
// - fallback_scanner: fallback for non-admin runs, using jwalk to traverse the filesystem
// - engine_selector:  auto-detects privileges and filesystem type to choose the appropriate engine
// ============================================================================

pub mod engine_selector;
pub mod fallback_scanner;
pub mod mft_scanner;
