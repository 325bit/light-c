// ============================================================================
// Scanner module - responsible for scanning junk files in the Windows system
// ============================================================================

pub(crate) mod big_files;
pub(crate) mod big_files_engine;
mod categories;
mod context_menu;
pub(crate) mod deep_junk;
mod file_info;
mod hotspot;
pub(crate) mod hotspot_engine;
pub(crate) mod leftover_whitelist;
mod leftovers;
mod recycle_bin;
mod registry;
mod registry_scoring;
pub(crate) mod reparse;
mod scan_engine;
pub(crate) mod shell_icons;
mod ai_tool_cleaner;
mod social_scanner;
// [dev-cache] Dev cache cleanup + the multi-version leftover scan (shown on the "leftover & cache" page)
pub(crate) mod dev_cache;

pub use categories::*;
pub use context_menu::*;
pub use file_info::*;
pub use hotspot::*;
pub use leftovers::*;
pub use registry::*;
pub use scan_engine::*;
pub use shell_icons::*;
pub use ai_tool_cleaner::*;
pub use social_scanner::*;
// [dev-cache]
pub use dev_cache::*;
