// ============================================================================
// Command module - split by functional domain
//
// Each submodule corresponds to a group of related Tauri commands, avoiding an oversized single file.
// All commands and public types are re-exported uniformly through mod.rs,
// so lib.rs can register everything with just `use commands::*`.
// ============================================================================

mod ai;
mod ai_models;
mod app;
pub mod data;
mod delete;
mod dev_cache;
mod disk;
mod disk_growth;
mod disk_health;
pub(crate) mod driver;
mod hotspot;
mod leftovers;
mod logger_cmd;
mod registry;
mod scan;
mod shell_icons;
mod ai_tools;
mod social;
mod system;
pub(crate) mod tools;
mod verify;

// Public types (for the frontend and other modules)
pub use ai::*;
pub use ai_models::*;
pub use app::*;
pub use data::*;
pub use delete::*;
pub use dev_cache::*;
pub use disk::*;
pub use disk_growth::*;
pub use disk_health::*;
pub use driver::*;
pub use hotspot::*;
pub use leftovers::*;
pub use logger_cmd::*;
pub use registry::*;
pub use scan::*;
pub use shell_icons::*;
pub use ai_tools::*;
pub use social::*;
pub use system::*;
pub use tools::*;
pub use verify::*;
