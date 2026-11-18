//! AI tool cleanup.
//!
//! The directory structure matches `social_scanner`:
//! - `types`   pure data structures (serialized to the frontend)
//! - `tools`   tool inventory; adding a tool only changes this file
//! - `core`    scanning, traversal, path validation and deletion
//! - `tests`   whitelist and edge cases

mod core;
#[cfg(test)]
mod tests;
mod tools;
mod types;

pub use core::AiToolScanner;
pub use types::{AiCacheDeleteResult, AiCacheFileList, AiToolScanResult};
