// ============================================================================
// Full-disk growth analysis module
//
// Centralizes C drive scanning, snapshots, and diff analysis in a standalone module, avoiding continuing to cram full-disk logic into
// ProgramData-specific rules; future maintenance of scan and comparison strategies will be clearer.
// ============================================================================

pub mod growth;
pub mod mft_scan;
pub mod snapshot;

pub use growth::*;
pub use mft_scan::{cancel_disk_growth_scan, reset_disk_growth_cancelled};
