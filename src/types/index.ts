// ============================================================================
// Type definitions - correspond to Rust backend data structures
// ============================================================================

import type { ErrorCode } from '../api/errors';

/** Junk file categories */
export type JunkCategory =
  | 'WindowsTemp'
  | 'SystemCache'
  | 'BrowserCache'
  | 'RecycleBin'
  | 'WindowsUpdate'
  | 'DeliveryOptimization'
  | 'WindowsDefenderCache'
  | 'ThumbnailCache'
  | 'LogFiles'
  | 'MemoryDump'
  | 'OldWindowsInstallation'
  | 'AppCache'
  | 'FontCache'
  | 'WindowsErrorReports'
  | 'InstallerTemp'
  | 'ClipboardCache'
  | 'ShaderCache';

/** Info for a single file */
export interface FileInfo {
  /** Full file path */
  path: string;
  /** File name */
  name: string;
  /** File size (bytes) */
  size: number;
  /** Original path before the file was deleted (only recycle bin entries have a value) */
  original_path?: string | null;
  /** Last modification time (Unix timestamp) */
  modified_time: number;
  /** Whether it is a directory */
  is_dir: boolean;
  /** Category it belongs to */
  category: JunkCategory;
}

/** Category scan result */
export interface CategoryScanResult {
  /** Category */
  category: JunkCategory;
  /** Category display name */
  display_name: string;
  /** Category description */
  description: string;
  /** Risk level */
  risk_level: number;
  /** Whether managed by the system: when true, no direct deletion is offered; the user is guided to use system Disk Cleanup */
  system_managed: boolean;
  /** All files in this category */
  files: FileInfo[];
  /** Total size (bytes) */
  total_size: number;
  /** Number of files */
  file_count: number;
  /** Whether the deep scan still has unloaded files */
  has_more?: boolean;
}

/** Full scan result */
export interface ScanResult {
  /** Scan results for each category */
  categories: CategoryScanResult[];
  /** Total size (bytes) */
  total_size: number;
  /** Total number of files */
  total_file_count: number;
  /** Scan duration (milliseconds) */
  scan_duration_ms: number;
  /** Scan timestamp */
  scan_timestamp: number;
}

/** Partition summary of a deep junk scan. */
export interface DeepJunkDriveSummary {
  drive_letter: string;
  file_system: string;
  backend: 'mft' | 'walkdir';
  matched_file_count: number;
  matched_size: number;
  warning?: string | null;
  /** Duration of each stage, used to locate which part of a scan is slow */
  stage_durations?: { stage: string; duration_ms: number }[];
  /** Total duration for this drive */
  total_duration_ms?: number;
  /** Number of data runs in $MFT; more segments mean a more fragmented table */
  mft_fragments?: number;
  /** Time waiting on disk while reading $MFT */
  mft_io_ms?: number;
  /** Time spent parsing $MFT records */
  mft_parse_ms?: number;
}

/** Deep junk scan result; the category file structure stays consistent with a normal scan. */
export interface DeepJunkScanResult extends ScanResult {
  scan_mode: 'deep';
  scan_id: string;
  drives: DeepJunkDriveSummary[];
  /** Overhead outside the drives: partition discovery / recycle bin / parallel waits / aggregation */
  outer_stages?: { stage: string; duration_ms: number }[];
  /** Cumulative wait for progress reporting (summed across threads) */
  emit_wait_ms?: number;
}

/** Deep junk scan stage progress. */
export interface DeepJunkScanProgress {
  stage: string;
  drive_letter: string;
  message: string;
  processed: number;
  matched_count: number;
  elapsed_ms: number;
}

/** Batch progress event for the junk cleanup deletion phase. */
export interface EnhancedDeleteProgress {
  /** Current phase: preparing or cleaning. Verification is shown separately by the frontend after the command completes. */
  phase: 'preparing' | 'cleaning';
  /** Number of files processed. */
  processed_count: number;
  /** Total number of deduplicated files to process. */
  total_count: number;
  /** Number of files successfully deleted. */
  success_count: number;
  /** Number of files that failed. */
  failed_count: number;
  /** Number of files marked for deletion on reboot. */
  reboot_pending_count: number;
  /** Physical space confirmed freed. */
  freed_physical_size: number;
  /** Backend deletion duration (milliseconds). */
  elapsed_ms: number;
}

/** Deletion result */
export interface DeleteResult {
  /** Number of files successfully deleted */
  success_count: number;
  /** Number of files that failed to delete */
  failed_count: number;
  /** Number of files marked for deletion on reboot */
  reboot_pending_count: number;
  /** Size of space freed (bytes) */
  freed_size: number;
  /** Whether a reboot is needed to complete cleanup */
  needs_reboot: boolean;
  /** List of failed files and reasons */
  failed_files: DeleteError[];
}

/** Deletion error info */
export interface DeleteError {
  /** File path */
  path: string;
  /**
   * English diagnostic from the backend.
   *
   * Only a fallback: localize with {@link failureText} using `failure_code`, so a
   * refused path does not surface as an English sentence to a zh/ja user.
   */
  reason: string;
  /** Machine-readable category; absent when the backend could not classify it. */
  failure_code?: ErrorCode | null;
}

/** Disk info */
export interface DiskInfo {
  /** Total disk capacity (bytes) */
  total_space: number;
  /** Used space (bytes) */
  used_space: number;
  /** Free space (bytes) */
  free_space: number;
  /** Usage percentage */
  usage_percent: number;
  /** Drive letter */
  drive_letter: string;
}

/** Category info */
export interface CategoryInfo {
  /** Category name */
  name: string;
  /** Category description */
  description: string;
  /** Risk level */
  risk_level: number;
}

/** Scan request parameters */
export interface ScanRequest {
  /** List of categories to scan */
  categories?: string[];
}

/** Delete request parameters */
export interface DeleteRequest {
  /** List of file paths to delete */
  paths: string[];
}

/** Big file scan result entry */
export interface LargeFileEntry {
  /** File path */
  path: string;
  /** File size (bytes) */
  size: number;
  /** Last modification time (Unix timestamp, seconds) */
  modified: number;
  /** Risk level (1-5), calculated by backend path rules */
  risk_level: number;
  /** Source label (e.g. "WeChat files", "virtual machine disk", "system temp files") */
  source_label: string;
}

/** Big file scan progress event payload */
export interface LargeFileScanProgress {
  /** File path currently being scanned */
  current_path: string;
  /** Number of files scanned */
  scanned_count: number;
  /** Number of Top N files collected so far */
  found_count: number;
  /** Scan engine: "mft" | "walkdir" */
  backend: string;
  /** MFT scan stage, used to locate timing bottlenecks */
  stage?: string;
  /** Backend stage description */
  message?: string;
  /** Time elapsed on the backend (milliseconds) */
  elapsed_ms?: number;
  /** Key numbers for the current stage (segments/disk read/parsing, record count, candidate count…), used to locate bottlenecks */
  detail?: string;
}

/** Application state */
export type AppStatus = 'idle' | 'scanning' | 'deleting';
