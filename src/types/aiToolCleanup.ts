/**
 * Data structures for AI tool cleanup.
 *
 * Fields correspond one-to-one with the Serialize output of backend `ai_tool_cleaner.rs` (camelCase).
 * Grouping is by tool: the user first sees "how much each tool occupies", then decides which item to delete.
 */

import type { LinkInfo } from './devCache';

/** A single cache directory */
export interface AiCacheEntry {
  /** Directory name, e.g. "Cache" / "code cache" */
  name: string;
  path: string;
  size: number;
  fileCount: number;
  /**
   * Has a value only when the path goes through a junction / symlink.
   *
   * After using FolderMove to relocate a directory like `.workbuddy` to another drive, a link is left in place,
   * so the scan counts the real files but the path still reads `C:\...` — when present, the UI should show `link.resolved`.
   */
  link?: LinkInfo;
}

/** Summary for one tool */
export interface AiToolGroup {
  toolId: string;
  toolName: string;
  totalSize: number;
  fileCount: number;
  /** Cache directories hit by this tool, sorted from largest to smallest */
  entries: AiCacheEntry[];
}

export interface AiToolScanResult {
  groups: AiToolGroup[];
  totalSize: number;
  totalFiles: number;
}

/** A single file in a cache directory (details are fetched on demand by the details modal) */
export interface AiCacheFile {
  path: string;
  size: number;
  /** Last modified time (Unix seconds); 0 means it wasn't retrieved and sorts last */
  modifiedTime: number;
}

/** File list for the details modal */
export interface AiCacheFileList {
  files: AiCacheFile[];
  /** Directory file count exceeded the cap and was truncated: the UI must state that only the first N are shown */
  truncated: boolean;
}

/** A single directory that failed to delete */
export interface AiCacheDeleteFailure {
  path: string;
  /** Failure reason (raw system error), for troubleshooting only, not shown directly in the UI */
  reason: string;
}

/** Batch deletion result */
export interface AiCacheDeleteResult {
  /** Number of directories successfully deleted */
  deleted: number;
  /** Directories that weren't deleted and why (files being occupied while the tool is running is the most common case) */
  failed: AiCacheDeleteFailure[];
}
