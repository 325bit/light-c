// ============================================================================
// Cleaner module - responsible for deleting junk files
// ============================================================================

mod delete_engine;
mod enhanced_delete;
mod permanent_delete;
pub(crate) mod safety_constants;

pub use delete_engine::*;
pub use enhanced_delete::*;
pub use permanent_delete::*;
