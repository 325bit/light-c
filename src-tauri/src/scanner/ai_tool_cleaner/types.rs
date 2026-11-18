//! Data structures for AI tool cleanup.
//!
//! Only serde structs, no logic — the frontend `src/types/aiToolCleanup.ts` corresponds one-to-one with this file.

use serde::Serialize;

use crate::scanner::reparse::LinkInfo;

/// Statistics for a single cache directory.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiCacheEntry {
    /// Directory name (for UI display, e.g. "Cache" / "media_cache")
    pub name: String,
    pub path: String,
    pub size: u64,
    pub file_count: usize,
    /// Whether this path has a junction / symlink (after moving a directory to another drive with a tool like FolderMove,
    /// a link is left in place). When present, the UI must also show the **real landing location**, otherwise users seeing `C:\...`
    /// would think their C drive is occupied that much. When there is no link, do not serialize it, so the frontend does not have to check for empty objects everywhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkInfo>,
}

/// A single file in a cache directory, fetched on demand by the detail modal.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiCacheFile {
    pub path: String,
    pub size: u64,
    /// Last modification time (Unix seconds). Used by the detail modal's "sort by modification time";
    /// pass 0 if unreadable, so it sorts last.
    pub modified_time: u64,
}

/// File list for the detail modal.
///
/// `truncated` is required: directory size is unpredictable, and if truncated without telling the user,
/// they would think "that is all the files", which would not match the size.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiCacheFileList {
    pub files: Vec<AiCacheFile>,
    pub truncated: bool,
}

/// Summary for one tool. The UI groups by tool so users first see "how much each tool occupies".
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiToolGroup {
    pub tool_id: String,
    pub tool_name: String,
    pub total_size: u64,
    pub file_count: usize,
    /// Matched cache directories for this tool, sorted by size descending
    pub entries: Vec<AiCacheEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiToolScanResult {
    pub groups: Vec<AiToolGroup>,
    pub total_size: u64,
    pub total_files: usize,
}

/// A single directory that failed to delete.
///
/// Must include a reason: cache files being in use while a tool is running is the most common failure scenario,
/// so the UI must make clear "it was not deleted, not that there was nothing to delete".
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiCacheDeleteFailure {
    pub path: String,
    pub reason: String,
}

/// Result of a batch deletion.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiCacheDeleteResult {
    /// Number of directories successfully deleted
    pub deleted: usize,
    /// Directories not deleted and their reasons
    pub failed: Vec<AiCacheDeleteFailure>,
}
