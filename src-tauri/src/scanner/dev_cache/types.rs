//! Data structures for dev-space cleanup.
//!
//! Only serde structs, no logic - the frontend `src/types/devCache.ts` corresponds one-to-one with this file.
//!
//! ⚠️ All paths are assembled at **runtime** from environment variables; no personal directory literals appear in the source,
//! so pushing the repo to a remote will not leak local machine information.

use serde::Serialize;

use crate::scanner::reparse::LinkInfo;

/// Risk level of a single item. The frontend uses this to decide default selection and whether to show a warning bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DevRisk {
    /// Pure cache; regenerated on the next run/build with no noticeable effect.
    Cache,
    /// Would lose config or data (emulator images, workspace state, models); requires user confirmation.
    Confirm,
    /// Display only, never given a checkbox.
    Readonly,
}

/// A cleanable item (a specific directory).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevCacheEntry {
    /// Stable id, used by both the frontend and deletion validation
    pub id: String,
    pub name: String,
    pub path: String,
    pub size: u64,
    pub file_count: usize,
    pub risk: DevRisk,
    /// A one-line explanation of "what happens after deletion", always shown in the UI
    pub note: String,
    /// Whether the directory currently exists (if not, it is not a failure - the tool is just not installed)
    pub exists: bool,
    /// Whether there is a junction / symlink on the path (left by relocation tools).
    /// When set, the UI must also show the real landing point, otherwise `C:\...` would make users think the space is on drive C.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkInfo>,
}

/// A group (classified by toolchain), shown as a collapsible card in the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevCacheGroup {
    pub group_id: String,
    pub group_name: String,
    pub total_size: u64,
    pub file_count: usize,
    pub entries: Vec<DevCacheEntry>,
}

/// One version among multi-version directories.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionEntry {
    pub path: String,
    /// Version number parsed from the directory name; if it cannot be parsed, keep the directory name as-is
    pub version: String,
    pub size: u64,
    pub file_count: usize,
    /// The version currently in use: marked in the UI and cannot be checked
    pub is_current: bool,
    /// How the current version was determined, shown to users so that "guessing by version number" is not taken as fact
    pub current_reason: String,
}

/// A set of same-product multi-version directories (e.g. 5 version directories under Edge's EdgeCore).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionGroup {
    pub id: String,
    pub name: String,
    pub base_path: String,
    pub total_size: u64,
    /// Total that can be deleted excluding the current version
    pub reclaimable_size: u64,
    pub entries: Vec<VersionEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevCacheScanResult {
    pub groups: Vec<DevCacheGroup>,
    /// Total usage of tool caches only. Multi-version directories are **not** included:
    /// they are reported by `scan_version_leftovers` and shown on the "leftover & cache" page.
    pub total_size: u64,
    pub total_files: usize,
    /// Total of items with risk Cache that can be cleaned directly (excluding Confirm / Readonly)
    pub safe_size: u64,
    pub elapsed_ms: u64,
    /// Number of paths skipped during scanning (insufficient permissions / in use), used to explain "why it is smaller than expected"
    pub skipped: usize,
}

/// Result of the standalone multi-version leftover scan.
///
/// Split out of `DevCacheScanResult` because the two live on different pages: tool caches stay in "dev cache cleanup",
/// while multi-version directories moved to "leftover & cache". Keeping one merged command would force each page
/// to traverse the other's directories just to render its own list.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionLeftoverScanResult {
    pub groups: Vec<VersionGroup>,
    /// Total of every version directory (including the one in use), so the UI can show "X in total, Y reclaimable"
    pub total_size: u64,
    pub total_files: usize,
    /// Total excluding the version currently in use
    pub reclaimable_size: u64,
    pub elapsed_ms: u64,
    /// Version directories skipped while measuring (insufficient permissions / in use)
    pub skipped: usize,
}

/// A single deletion failure item. It must carry a reason: files being in use is the most common case,
/// and returning only a success count would make the UI show "all deleted" while the actual usage has not dropped.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevCacheDeleteFailure {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevCacheDeleteResult {
    pub deleted: usize,
    pub freed: u64,
    pub failed: Vec<DevCacheDeleteFailure>,
}
