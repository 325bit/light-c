// ============================================================================
// Tauri command invocation wrapper
// Wraps all communication interfaces with the Rust backend
// ============================================================================

import { invoke } from '@tauri-apps/api/core';
import type {
  DiskInfo,
  ScanResult,
  DeepJunkScanResult,
  CategoryScanResult,
  DeleteResult,
  CategoryInfo,
  ScanRequest,
  DeleteRequest,
  LargeFileEntry,
} from '../types';
import { errorHint, errorText, type ErrorCode } from './errors';

export type DistributionChannel = 'installer' | 'portable';
export type VerifyIntegrityStatus =
  | 'verified'
  | 'failed'
  | 'network_error'
  | 'release_unavailable'
  | 'signature_error';

export interface VerifyIntegrityResult {
  verified: boolean;
  status: VerifyIntegrityStatus;
  version: string;
  channel: string;
  message: string;
  official_url: string;
}

/** Gets the current distribution channel; the backend is compatible with both the legacy marker and the new manifest. */
export async function getDistributionChannel(): Promise<DistributionChannel> {
  return invoke<DistributionChannel>('get_distribution_channel');
}

export interface StorageLocationInfo {
  distribution_channel: DistributionChannel;
  config_directory: string;
  config_file: string;
  default_data_directory: string;
  current_data_directory: string;
  data_directory_is_custom: boolean;
  portable_root?: string | null;
  webview_data_directory?: string | null;
  legacy_data_directory?: string | null;
  can_write: boolean;
  migration_available: boolean;
  migration_completed: boolean;
}

/** Gets the unified config, data directory, and portable-version migration status. */
export async function getStorageLocationInfo(): Promise<StorageLocationInfo> {
  return invoke<StorageLocationInfo>('get_storage_location_info');
}

/** Portable directory write-capability diagnostics: when not writable, data can only be temporarily placed in AppData. */
export interface StorageWriteDiagnostic {
  /** Directory containing the portable package (exe); null for the installed version. */
  portable_root?: string | null;
  /** Expected data directory to write to (for portable builds, the data directory under the exe). */
  preferred_data_directory: string;
  /** Whether the expected directory is actually writable. */
  preferred_writable: boolean;
  /** Whether a "portable directory not writable → fell back to AppData" event occurred. */
  fell_back_to_app_data: boolean;
  /** Whether the data directory currently in use is writable. */
  active_writable: boolean;
  reason?: string | null;
}

export async function getStorageWriteDiagnostic(): Promise<StorageWriteDiagnostic> {
  return invoke<StorageWriteDiagnostic>('get_storage_write_diagnostic');
}

/** Retries legacy portable data migration; the backend only copies LightC's own data and preserves the source files. */
export async function migrateLegacyPortableData(): Promise<StorageLocationInfo> {
  return invoke<StorageLocationInfo>('migrate_legacy_portable_data');
}

/**
 * Validates whether the current LightC.exe passes official minisign signature verification.
 * Network failures are converted by the backend to network_error, so the frontend can give a hint distinct from a tampering risk.
 */
export async function verifyIntegrity(): Promise<VerifyIntegrityResult> {
  return invoke<VerifyIntegrityResult>('verify_integrity');
}

/**
 * Gets C drive disk information. */
export async function getDiskInfo(): Promise<DiskInfo> {
  return invoke<DiskInfo>('get_disk_info');
}

/** Local fixed disk partition information, reused by the multi-disk analysis module. */
export interface LocalDriveInfo {
  /** Drive letter, e.g. C: */
  drive_letter: string;
  /** Root path, e.g. C:\ */
  root_path: string;
  /** Volume label, may be empty */
  volume_name: string;
  /** File system, e.g. NTFS */
  file_system: string;
  /** Total disk capacity */
  total_space: number;
  /** Used space */
  used_space: number;
  /** Free space */
  free_space: number;
  /** Usage percentage */
  usage_percent: number;
  /** Whether it is the system drive */
  is_system: boolean;
  /** MFT scanning currently supports NTFS only */
  is_ntfs: boolean;
}

/** Gets the list of local fixed disk partitions. */
export async function getLocalDrives(): Promise<LocalDriveInfo[]> {
  return invoke<LocalDriveInfo[]>('get_local_drives');
}

export interface DiskVolumeInfo {
  drive_letter: string;
  volume_name: string;
  file_system: string;
  total_space: number;
  used_space: number;
  free_space: number;
  usage_percent: number;
}

export interface DiskHealthInfo {
  number: number | null;
  model: string;
  serial_number: string;
  firmware_version: string;
  media_type: string;
  bus_type: string;
  health_status: 'Healthy' | 'Warning' | 'Unhealthy' | 'Unknown';
  operational_status: string;
  size: number;
  drive_letters: string[];
  volumes: DiskVolumeInfo[];
}

/** Gets basic physical disk information and the health status reported by Windows Storage. */
export async function getDiskHealth(): Promise<DiskHealthInfo[]> {
  return invoke<DiskHealthInfo[]>('get_disk_health');
}

/**
 * Performs a junk file scan
 * @param request Scan request parameters (optional)
 */
export async function scanJunkFiles(request?: ScanRequest): Promise<ScanResult> {
  return invoke<ScanResult>('scan_junk_files', { request });
}

/** Scans deep junk across all fixed partitions, preferring MFT for NTFS partitions. */
/**
 * Deep-scans junk files.
 * @param maxDepth Maximum directory recursion depth, from the settings page's "Deep Discovery Scan Depth".
 *                 The backend clamps it once more (fallback within 1-24); passing an abnormal value won't let the scan run out of control.
 */
export async function scanDeepJunkFiles(maxDepth: number): Promise<DeepJunkScanResult> {
  return invoke<DeepJunkScanResult>('scan_deep_junk_files', { maxDepth });
}

/** Cancels the deep junk scan. */
export async function cancelDeepJunkScan(): Promise<void> {
  return invoke<void>('cancel_deep_junk_scan');
}

/** Reads the next page of files for a deep-scan category on demand. */
export async function getDeepJunkCategoryPage(
  scanId: string,
  categoryName: string,
  offset: number,
  limit = 500,
): Promise<CategoryScanResult> {
  return invoke<CategoryScanResult>('get_deep_junk_category_page', {
    scanId,
    categoryName,
    offset,
    limit,
  });
}

/**
 * Scans a single category
 * @param categoryName Category name
 */
export async function scanCategory(categoryName: string): Promise<CategoryScanResult> {
  return invoke<CategoryScanResult>('scan_category', { categoryName });
}

/**
 * Deletes the specified files
 * @param paths List of file paths to delete
 */
export async function deleteFiles(paths: string[]): Promise<DeleteResult> {
  const request: DeleteRequest = { paths };
  return invoke<DeleteResult>('delete_files', { request });
}

/**
 * Gets all available cleanup categories
 */
export async function getCategories(): Promise<CategoryInfo[]> {
  return invoke<CategoryInfo[]>('get_categories');
}

/**
 * Formats a file size (calls the Rust side)
 * @param bytes Number of bytes */
export async function formatSizeFromRust(bytes: number): Promise<string> {
  return invoke<string>('format_size', { bytes });
}

/**
 * Opens the Windows Disk Cleanup tool
 */
export async function openDiskCleanup(): Promise<void> {
  return invoke<void>('open_disk_cleanup');
}

/**
 * Scans large files on the specified disk
 * @param topN Return the top N largest files (10-500, default 50)
 * @param driveLetter Target drive letter, e.g. C: / D:
 */
export async function scanLargeFiles(topN?: number, driveLetter?: string): Promise<LargeFileEntry[]> {
  return invoke<LargeFileEntry[]>('scan_large_files', { topN, driveLetter });
}

/**
 * Cancels the large file scan. */
export async function cancelLargeFileScan(): Promise<void> {
  return invoke<void>('cancel_large_file_scan');
}

/**
 * Opens the folder containing a file in File Explorer. */
export async function openInFolder(path: string): Promise<void> {
  return invoke<void>('open_in_folder', { path });
}

/**
 * Opens a file directly (using the system default program)
 */
export async function openFile(path: string): Promise<void> {
  return invoke<void>('open_file', { path });
}

/** Opens the Windows system Recycle Bin view, avoiding exposing internal SID directories. */
export async function openRecycleBin(): Promise<void> {
  return invoke<void>('open_recycle_bin');
}

// ============================================================================
// System slimming related
// ============================================================================

/** System slimming item status */
export interface SlimItemStatus {
  id: string;
  name: string;
  description: string;
  warning: string;
  status_text: string;
  enabled: boolean;
  size: number;
  actionable: boolean;
  action_text: string;
  /** This item is still being analyzed in the background (currently only the two WinSxS items can be true); pushed via an event once analysis finishes */
  analyzing: boolean;
  /**
   * Reclaimable component-store breakdown (only the winsxs item carries this).
   *
   * Chinese note: `size` gives only a total, so you can't tell how much of the 6.9 GB is superseded old components,
   * and how much is cached temporary data; these two come directly from DISM analysis and are used to break it down in the UI.
   */
  breakdown?: WinsxsBreakdown;
}

/** Reclaimable component breakdown reported by DISM */
export interface WinsxsBreakdown {
  /** Backups and disabled features: superseded old component copies temporarily kept for rollback */
  backups: number;
  /** Cache and temporary data */
  cache: number;
  /** Number of reclaimable component packages */
  packages: number;
}

/** System slimming status summary */
export interface SystemSlimStatus {
  is_admin: boolean;
  items: SlimItemStatus[];
  total_reclaimable: number;
}

/**
 * Checks whether running with administrator privileges. */
export async function checkAdminPrivilege(): Promise<boolean> {
  return invoke<boolean>('check_admin_privilege');
}

/**
 * Restarts the current program with administrator privileges.
 *
 * Goes through the system UAC; the user must confirm in the popup.
 * @returns true = admin instance launched (current process will exit automatically); false = user cancelled UAC
 */
export async function relaunchAsAdmin(): Promise<boolean> {
  return invoke<boolean>('relaunch_as_admin');
}

/**
 * Gets the system slimming status. */
export async function getSystemSlimStatus(forceRefresh = false): Promise<SystemSlimStatus> {
  // forceRefresh: when manually "re-checking", skip the 10-minute component-store analysis cache; otherwise clicking seems to have no effect
  return invoke<SystemSlimStatus>('get_system_slim_status', { forceRefresh });
}

/**
 * System slimming operation progress (backend event system-slim:progress).
 */
export interface SlimOperationProgress {
  /** Operation item id (hibernation / winsxs / winsxs_resetbase). */
  item_id: string;
  /** Phase: preparing / running / waiting / done / error. */
  phase: string;
  /** User-facing phase description. */
  message: string;
  /** 0-100, 0 means it cannot be estimated yet. */
  percent: number;
  /** running / done / error。 */
  status: 'running' | 'done' | 'error';
  /** Elapsed time (milliseconds). */
  elapsed_ms: number;
}

/**
 * Disables hibernation (runs asynchronously, progress pushed via the system-slim:progress event)
 */
export async function disableHibernation(): Promise<string> {
  return invoke<string>('disable_hibernation');
}

/**
 * Enables hibernation (runs asynchronously, progress pushed via the system-slim:progress event)
 */
export async function enableHibernation(): Promise<string> {
  return invoke<string>('enable_hibernation');
}

/**
 * Cleans the WinSxS component store
 */
export async function cleanupWinsxs(): Promise<string> {
  return invoke<string>('cleanup_winsxs');
}

/**
 * Deep-cleans the WinSxS component baseline
 */
export async function cleanupWinsxsResetbase(): Promise<string> {
  return invoke<string>('cleanup_winsxs_resetbase');
}

/**
 * Opens the system virtual memory settings
 */
export async function openVirtualMemorySettings(): Promise<void> {
  return invoke<void>('open_virtual_memory_settings');
}

export async function rebuildSearchIndex(): Promise<string> {
  return invoke<string>('rebuild_search_index');
}

// ============================================================================
// Legacy driver cleanup API
// ============================================================================

export interface DriverPackageInfo {
  published_name: string;
  original_name: string;
  provider_name: string;
  class_name: string;
  driver_version: string;
  family_id: string;
  signer_name: string;
  driver_store_path: string;
  device_count: number;
  active_device_count: number;
  installed_device_count: number;
  outranked_device_count: number;
  file_count: number;
  /** Estimated footprint of the driver package directory (bytes), 0 means unreadable */
  total_size: number;
  status: 'old_confirmed' | 'recommended' | 'in_use' | 'no_newer_version' | 'unknown';
  actionable: boolean;
  reason: string;
}

export interface DriverScanResult {
  is_admin: boolean;
  packages: DriverPackageInfo[];
  total_count: number;
  candidate_count: number;
  high_confidence_count: number;
  device_match_data_available: boolean;
}

export interface DriverDeleteDetail {
  published_name: string;
  success: boolean;
  verified_removed: boolean;
  error_message: string | null;
}

export interface DriverDeleteResult {
  backup_directory: string;
  success_count: number;
  failed_count: number;
  needs_reboot: boolean;
  details: DriverDeleteDetail[];
}

export interface DriverRestoreResult {
  backup_directory: string;
  success: boolean;
  needs_reboot: boolean;
  message: string;
}

/** Detects third-party driver packages; the backend only returns safety-classified results. */
export async function scanOldDrivers(): Promise<DriverScanResult> {
  return invoke<DriverScanResult>('scan_old_drivers');
}

/** Re-validates, backs up, and deletes the selected driver packages on the backend. */
export async function deleteOldDrivers(publishedNames: string[]): Promise<DriverDeleteResult> {
  return invoke<DriverDeleteResult>('delete_old_drivers', { publishedNames });
}

/** Recursively restores all saved driver package backups in the current data directory. */
export async function restoreAllDriverBackups(): Promise<DriverRestoreResult> {
  return invoke<DriverRestoreResult>('restore_all_driver_backups');
}

/** Opens the standalone driver backup directory under the current data directory. */
export async function openDriverBackupDir(): Promise<void> {
  return invoke<void>('open_driver_backup_dir');
}

// ============================================================================
// Health score related
// ============================================================================

/** System health score result */
export interface HealthScoreResult {
  score: number;
  disk_score: number;
  hibernation_score: number;
  junk_score: number;
  disk_free_percent: number;
  has_hibernation: boolean;
  hibernation_size: number;
  junk_size: number;
}

/**
 * Gets the system health score
 */
export async function getHealthScore(driveLetter?: string): Promise<HealthScoreResult> {
  // When no drive letter is passed, the backend calculates against the system drive; after switching drives, pass the selected letter,
  // so the disk-space dimension follows the top bar (hibernation / junk remain system-level metrics).
  return driveLetter
    ? invoke<HealthScoreResult>('get_health_score', { driveLetter })
    : invoke<HealthScoreResult>('get_health_score');
}

// ============================================================================
// Social software scanning - with risk grading// ============================================================================

/** Risk level */
export type RiskLevel = 'critical' | 'medium' | 'low' | 'none';

/** File category */
export type FileCategory = 'chat_database' | 'image_video' | 'file_transfer' | 'temp_cache' | 'moments_cache';

/** Social software file entry */
export interface SocialFileEntry {
  /** Full file path */
  path: string;
  /** File size (bytes) */
  size: number;
  /** Owning application name */
  app_name: string;
  /** File category */
  category: FileCategory;
  /** Risk level */
  risk_level: RiskLevel;
  /** Whether it can be deleted (forced to false for Critical level) */
  deletable: boolean;
  /** Last modified time (Unix seconds); 0 means it wasn't retrieved and sorts last */
  modified_time: number;
}

/** Social software category statistics */
export interface SocialCategoryStats {
  /** Category ID */
  id: string;
  /** Category name */
  name: string;
  /** Category description */
  description: string;
  /** Number of files */
  file_count: number;
  /** Total size (bytes) */
  total_size: number;
  /** Number of deletable files */
  deletable_count: number;
  /** Size of deletable files */
  deletable_size: number;
  /** File list */
  files: SocialFileEntry[];
}

/** Social software scan result V2 */
export interface SocialScanResult {
  /** Statistics by category */
  categories: SocialCategoryStats[];
  /** Total number of files */
  total_files: number;
  /** Total size */
  total_size: number;
  /** Number of deletable files */
  deletable_files: number;
  /** Size of deletable files */
  deletable_size: number;
  /** List of detected social software */
  detected_apps: string[];
  /** Per-phase timing breakdown (for performance troubleshooting, shown in the "Timing Details" popover in the frontend) */
  phase_durations: SocialPhaseDuration[];
  /** The slowest scan paths (up to 5), in descending order of elapsed time */
  slow_paths: SocialSlowPath[];
}

/** Timing of a single phase of the social scan */
export interface SocialPhaseDuration {
  /** Phase id: detect (path detection) / categorize (traverse and grade) / total (total) */
  stage: string;
  /** Phase elapsed time, in milliseconds */
  duration_ms: number;
}

/**
 * Timing breakdown for a single scan path.
 *
 * `directory_count` is the key to troubleshooting: this module's cost is mainly "listing directories" rather than "reading files",
 * when the number of directories far exceeds the number of files (e.g. QQ's Image: 4388 directories but only 1803 files),
 * the slowness comes from the number of readdir calls — this single metric tells you.
 */
export interface SocialSlowPath {
  /** Scan path */
  path: string;
  /** Owning application */
  app_name: string;
  /** Elapsed time, in milliseconds */
  duration_ms: number;
  /** Number of files under this path */
  file_count: number;
  /** Number of directories under this path (including itself) */
  directory_count: number;
}

/**
 * Scans social software caches (with risk grading).
 *
 * The Chinese explanation is kept here so the frontend risk labels and the backend classification semantics stay consistent, avoiding accidental changes to the deletion policy during later maintenance.
 */
export async function scanSocialCache(): Promise<SocialScanResult> {
  return invoke<SocialScanResult>('scan_social_cache');
}

/** Gets the Chinese description of a risk level, used for the risk labels in the social cleanup list. */
export function getRiskLevelDescription(level: RiskLevel): string {
  switch (level) {
    case 'critical': return '危险（聊天记录）';
    case 'medium': return '谨慎清理';
    case 'low': return '建议清理';
    case 'none': return '安全清理';
  }
}

/** Gets the hint for a risk level, used to explain on hover why it was graded that way. */
export function getRiskLevelTooltip(level: RiskLevel): string {
  switch (level) {
    case 'critical': return '聊天记录数据库，删除后会永久丢失，强烈建议保留';
    case 'medium': return '可能包含重要文档或附件，请确认后再删除';
    case 'low': return '图片或视频缓存，删除后通常可重新下载';
    case 'none': return '临时缓存文件，通常可以安全删除';
  }
}

// ============================================================================
// Uninstall leftover scan related
// ============================================================================

/** Uninstall leftover scan result */
export interface LeftoverScanResult {
  /** List of leftover folders found */
  leftovers: LeftoverEntry[];
  /** Total size (bytes) */
  total_size: number;
  /** Scan elapsed time (milliseconds) */
  scan_duration_ms: number;
  /** Number of first-level directories actually checked (including those blocked by the whitelist / size threshold) */
  scanned_directories: number;
  /** Number of entries that entered scoring but were discarded for falling below the threshold */
  below_threshold: number;
  /** Highest confidence among the scored directories (0~1) */
  highest_confidence: number;
  /** Current decision threshold (0~1), same source as the backend — don't hard-code a separate value in the frontend */
  confidence_threshold: number;
}

/** Leftover type */
export type LeftoverType = 'Normal' | 'Emulator' | 'VirtualDisk' | 'RegistryOrphan';

/** Detection category (confidence grading) */
export type DetectionCategory = 'HighConfidenceLeftover' | 'Suspicious' | 'LikelyAppData' | 'SystemShared' | 'UpdaterCache' | 'CacheDir';

/** Single leftover entry */
export interface LeftoverEntry {
  /** Folder path */
  path: string;
  /** Folder size (bytes) */
  size: number;
  /** Possible software name */
  app_name: string;
  /** Source type */
  source: 'LocalAppData' | 'RoamingAppData' | 'LocalLowAppData' | 'ProgramData' | 'VirtualDiskFile';
  /** Last modified time (Unix timestamp) */
  last_modified: number;
  /** Number of files contained */
  file_count: number;
  /** Whether it is an emulator leftover */
  is_emulator: boolean;
  /** Whether it is a virtual disk file */
  is_virtual_disk: boolean;
  /** Leftover type */
  leftover_type: LeftoverType;
  /** Confidence score (0.0 ~ 1.0); higher means more likely to be a leftover */
  confidence: number;
  /** Detection category */
  detection_category: DetectionCategory;
  /** List of scoring reasons (Chinese) */
  reasons: string[];
}

/** Uninstall leftover deletion result */
export interface LeftoverDeleteResult {
  /** Number of folders successfully deleted */
  deleted_count: number;
  /** Amount of space released (bytes) */
  deleted_size: number;
  /** Paths that failed to delete */
  failed_paths: string[];
  /** List of error messages */
  errors: string[];
  /** Paths skipped for containing executable files (must be handled via deep cleanup) */
  skipped_executables: string[];
}

/** Uninstall leftover paths the user chose to keep */
export interface LeftoverWhitelistEntry {
  path: string;
  addedAt: string;
}

/**
 * Scans uninstall leftovers
 * Scans orphaned folders left by uninstalled software in AppData and ProgramData
 * @param deepScan Whether to enable deep scan mode (scan emulator leftovers, virtual disk files, etc.) */
export async function scanUninstallLeftovers(deepScan?: boolean): Promise<LeftoverScanResult> {
  return invoke<LeftoverScanResult>('scan_uninstall_leftovers', { deepScan });
}

/**
 * Deletes uninstall leftover folders. * @param paths List of folder paths to delete */
export async function deleteLeftoverFolders(paths: string[]): Promise<LeftoverDeleteResult> {
  return invoke<LeftoverDeleteResult>('delete_leftover_folders', { paths });
}

/** Gets the uninstall leftover user whitelist */
export async function listLeftoverWhitelist(): Promise<LeftoverWhitelistEntry[]> {
  return invoke<LeftoverWhitelistEntry[]>('list_leftover_whitelist');
}

/** Adds a scan-result path to the uninstall leftover whitelist */
export async function addLeftoverWhitelistEntry(path: string): Promise<LeftoverWhitelistEntry> {
  return invoke<LeftoverWhitelistEntry>('add_leftover_whitelist_entry', { path });
}

/** Removes protection from an uninstall leftover path */
export async function removeLeftoverWhitelistEntry(path: string): Promise<void> {
  return invoke<void>('remove_leftover_whitelist_entry', { path });
}

// ============================================================================
// Registry redundancy scan related (v3 - hard-filter convergence)
// ============================================================================

/** Registry scan result */
export interface RegistryScanResult {
  entries: RegistryEntry[];
  total_count: number;
  scan_duration_ms: number;
}

/** Single registry entry */
export interface RegistryEntry {
  /** Full path under HKCR\Applications */
  path: string;
  /** Application name */
  name: string;
  /** Associated non-existent executable path */
  associated_path: string;
  /** Problem description */
  issue: string;
}

/** Registry deletion result */
export interface RegistryDeleteResult {
  backup_path: string;
  deleted_count: number;
  failed_entries: string[];
  errors: string[];
}

/**
 * Scans registry redundancy. * Only scans MUI cache and HKCR\Applications, filtered by hard-evidence conditions
 */
export async function scanRegistryRedundancy(): Promise<RegistryScanResult> {
  return invoke<RegistryScanResult>('scan_registry_redundancy');
}

/**
 * Backs up and deletes registry entries
 * @param entries List of registry entries to delete */
export async function deleteRegistryEntries(entries: RegistryEntry[]): Promise<RegistryDeleteResult> {
  return invoke<RegistryDeleteResult>('delete_registry_entries', { entries });
}

/**
 * Opens the registry backup directory */
export async function openRegistryBackupDir(): Promise<void> {
  return invoke<void>('open_registry_backup_dir');
}

// ============================================================================
// Enhanced delete API - supports locked-file handling and physical-size calculation// ============================================================================

/** Deletion failure reason */
export type DeleteFailureReason = 
  | 'NotFound'
  | 'PermissionDenied'
  | 'FileLocked'
  | 'SystemProtected'
  | 'OutOfScope'
  | 'MarkedForReboot'
  | { Other: string };

/** Single file deletion result */
export interface FileDeleteResult {
  /** File path */
  path: string;
  /** Whether it was successfully deleted */
  success: boolean;
  /** Logical size (file content size) */
  logical_size: number;
  /** Physical size (actual disk usage) */
  physical_size: number;
  /** Failure reason */
  failure_reason: DeleteFailureReason | null;
  /** Whether it is marked for deletion on reboot */
  marked_for_reboot: boolean;
}

/** Enhanced deletion result */
export interface EnhancedDeleteResult {
  /** Number of files successfully deleted */
  success_count: number;
  /** Number of files that failed */
  failed_count: number;
  /** Number of files marked for deletion on reboot */
  reboot_pending_count: number;
  /** Actual physical space released (bytes) */
  freed_physical_size: number;
  /** Total logical size */
  freed_logical_size: number;
  /** Size of skipped files */
  skipped_size: number;
  /** Detailed per-file deletion results */
  file_results: FileDeleteResult[];
  /** Whether a reboot is required to complete the cleanup */
  needs_reboot: boolean;
  /** Wall-clock time the batch took, measured inside the engine (ms); 0 when not reported */
  duration_ms: number;
  /** Summary message (WeChat style) */
  summary_message: string;
  /** Warning hints during cleanup (e.g. downgrade due to an expired deep-scan session), null on normal deletion */
  warning?: string | null;
}

/**
 * Enhanced file deletion
 * Supports physical-size calculation, locked-file handling, and detailed failure-reason feedback. * @param paths List of file paths to delete
 */
export async function enhancedDeleteFiles(paths: string[]): Promise<EnhancedDeleteResult> {
  return invoke<EnhancedDeleteResult>('enhanced_delete_files', { paths });
}

export interface DeepJunkDeleteOptions {
  /** Deep-scan session ID, used to retrieve the full uncategorized-paged category files on the backend. */
  scanId?: string;
  /** When all items on the current page are selected and the category still has more pages, delete by full-category scan result. */
  categoryNames?: string[];
  /** Paths the user deselected in the full category; kept to avoid accidental deletion. */
  excludedPaths?: string[];
}

/** Deletes deep-scan results; the backend re-validates path rules and expands the full category. */
export async function deleteDeepJunkFiles(
  paths: string[],
  options: DeepJunkDeleteOptions = {},
): Promise<EnhancedDeleteResult> {
  return invoke<EnhancedDeleteResult>('delete_deep_junk_files', {
    paths,
    // Tauri command arguments default to camelCase; passing snake_case is ignored by the command layer, causing only the current page to be deleted.
    scanId: options.scanId,
    categoryNames: options.categoryNames,
    excludedPaths: options.excludedPaths,
  });
}

/**
 * Gets a file's physical size (aligned to clusters). * @param logicalSize Logical size (bytes)
 */
export async function getPhysicalSize(logicalSize: number): Promise<number> {
  return invoke<number>('get_physical_size', { logicalSize });
}

/**
 * Checks whether a path requires administrator privileges
 * @param path File path
 */
export async function checkAdminForPath(path: string): Promise<boolean> {
  return invoke<boolean>('check_admin_for_path', { path });
}

/**
 * Gets a user-friendly description of the failure reason.
 *
 * Text comes from the `errors` namespace, keyed by the code the backend sent —
 * this module must not keep its own copy of the copy. It did before: the texts
 * drifted from the Rust side, and the safety-check helper ended up mojibake.
 */
export function getFailureReasonMessage(reason: DeleteFailureReason | null): string {
  if (!reason) return '';
  return errorText({ code: failureReasonCode(reason), message: '' });
}

/**
 * Gets the detailed hint for a failure reason (used in tooltips)
 *
 * `Other` carries the raw OS error from the backend. It is untranslatable, so it
 * is shown verbatim instead of being mapped to a generic sentence.
 */
export function getFailureReasonTooltip(reason: DeleteFailureReason | null): string {
  if (!reason) return '';
  if (typeof reason === 'object' && 'Other' in reason) return reason.Other;
  const code = failureReasonCode(reason);
  return errorHint({ code, message: '' }) ?? errorText({ code, message: '' });
}

/** Per-file failure reason → closed error code. Never branch on the text. */
function failureReasonCode(reason: DeleteFailureReason): ErrorCode {
  switch (reason) {
    case 'NotFound':
      return 'not_found';
    case 'PermissionDenied':
      return 'permission_denied';
    case 'FileLocked':
      return 'target_in_use';
    case 'SystemProtected':
      return 'protected';
    case 'OutOfScope':
      return 'invalid_path';
    case 'MarkedForReboot':
      return 'target_in_use';
    default:
      return 'internal';
  }
}

// ============================================================================
// Permanent delete API - deep cleanup of uninstall leftovers
// ============================================================================

/** Safety check result type */
export type SafetyCheckResult = 
  | 'Safe'  // Passed all checks, safe to delete
  | { FoundInRegistry: { matched_field: string; matched_value: string } }
  | { ContainsExecutables: { files: string[] } }
  | { InProtectedPath: { reason: string } };

/** Permanent deletion result for a single leftover */
export interface LeftoverPermanentDeleteDetail {
  /** Folder path */
  path: string;
  /** Whether it was successfully deleted */
  success: boolean;
  /** Number of files deleted */
  deleted_files: number;
  /** Space released (bytes) */
  freed_size: number;
  /** Failure reason (raw text; show only in tooltips) */
  failure_reason: string | null;
  /** Machine-readable failure category; switch on this, never on `failure_reason` */
  failure_code?: ErrorCode | null;
  /** Whether it is marked for deletion on reboot */
  marked_for_reboot: boolean;
  /** Whether manual review is required */
  needs_manual_review: boolean;
  /** Safety check result */
  safety_check: SafetyCheckResult;
}

/** Overall result of permanent deletion */
export interface PermanentDeleteResult {
  /** Number of folders successfully deleted */
  success_count: number;
  /** Number of folders that failed */
  failed_count: number;
  /** Number requiring manual review */
  manual_review_count: number;
  /** Number marked for deletion on reboot */
  reboot_pending_count: number;
  /** Actual space released (bytes) */
  freed_size: number;
  /** Detailed result of each folder */
  details: LeftoverPermanentDeleteDetail[];
  /** Deletion elapsed time (milliseconds) */
  duration_ms: number;
}

/**
 * Permanently deletes uninstall leftovers (deep cleanup)
 * 
 * ⚠️ Warning: this operation permanently deletes files from disk and cannot be undone!
 * 
 * Three safety checks are performed before deletion:
 * 1. Registry check - confirm the directory is not part of any installed program
 * 2. Executable check - scan for .exe/.dll/.sys files, skip if found
 * 3. Core whitelist check - ensure the path is not in a critical system directory *
 * @param paths List of folder paths to permanently delete */
export async function deleteLeftoversPermanent(
  paths: string[],
  /**
   * Whether to allow "directories containing executable files". Default false.
   *
   * True only when the user explicitly clicked "Force Delete" in the result popup: whitelist and system-directory
   * protection always apply; only the rule "reject if .exe/.dll/.sys is present" is relaxed.
   */
  allowExecutables = false,
): Promise<PermanentDeleteResult> {
  return invoke<PermanentDeleteResult>('delete_leftovers_permanent', { paths, allowExecutables });
}

/**
 * Performs a safety check on a single path. * You can call this API to check whether a path is safe before the user confirms deletion. * @param path Folder path to check */
export async function checkLeftoverSafety(path: string): Promise<SafetyCheckResult> {
  return invoke<SafetyCheckResult>('check_leftover_safety', { path });
}

/**
 * Checks whether the safety check result is safe. */
export function isSafetyCheckPassed(result: SafetyCheckResult): boolean {
  return result === 'Safe';
}

// ============================================================================
// System information API
// ============================================================================

/** System information */
export interface SystemInfo {
  /** Operating system name */
  os_name: string;
  /** Operating system version */
  os_version: string;
  /** System architecture */
  os_arch: string;
  /** Computer name */
  computer_name: string;
  /** User name */
  user_name: string;
  /** CPU information */
  cpu_info: string;
  /** Number of CPU cores */
  cpu_cores: number;
  /** Total memory (bytes) */
  total_memory: number;
  /** Available memory (bytes) */
  available_memory: number;
  /** System boot time (seconds) */
  uptime_seconds: number;
}

/**
 * Gets system information
 */
export async function getSystemInfo(): Promise<SystemInfo> {
  return invoke<SystemInfo>('get_system_info');
}

// ============================================================================
// Cleanup log related API
// ============================================================================

/**
 * Cleanup log entry input
 */
export interface CleanupLogEntryInput {
  /** Cleanup module category */
  category: string;
  /** File path */
  path: string;
  /** File size (bytes) */
  size: number;
  /** Whether it succeeded */
  success: boolean;
  /** Error message (optional) */
  error_message?: string;
}

/**
 * Cleanup history summary
 */
export interface CleanupHistorySummary {
  /** Log file name */
  filename: string;
  /** Session start time */
  session_start: string;
  /** Session end time */
  session_end: string;
  /** Total number of files */
  total_files: number;
  /** Number of successes */
  success_count: number;
  /** Number of failures */
  failed_count: number;
  /** Total space released (bytes) */
  total_freed_bytes: number;
}

const APP_SETTINGS_STORAGE_KEY = 'c-cleanup-settings';
const DEFAULT_CLEANUP_LOG_RETENTION = 10;

function getCleanupLogRetentionSetting(): number {
  if (typeof window === 'undefined') {
    return DEFAULT_CLEANUP_LOG_RETENTION;
  }

  try {
    const raw = window.localStorage.getItem(APP_SETTINGS_STORAGE_KEY);
    if (!raw) return DEFAULT_CLEANUP_LOG_RETENTION;
    const parsed = JSON.parse(raw) as { cleanupLogRetention?: unknown };
    const count = Math.floor(Number(parsed.cleanupLogRetention) || DEFAULT_CLEANUP_LOG_RETENTION);
    // The frontend also clamps local settings on read; the backend validates again to prevent manual tampering with localStorage.
    return Math.min(100, Math.max(1, count));
  } catch {
    return DEFAULT_CLEANUP_LOG_RETENTION;
  }
}

/**
 * Records a cleanup operation to the log file. * @param entries Cleanup record array
 */
export async function recordCleanupAction(entries: CleanupLogEntryInput[]): Promise<string> {
  return invoke<string>('record_cleanup_action', {
    entries,
    maxLogFiles: getCleanupLogRetentionSetting(),
  });
}

/**
 * Opens the log file directory. */
export async function openLogsFolder(): Promise<void> {
  return invoke<void>('open_logs_folder');
}

/**
 * Gets the cleanup history list
 */
export async function getCleanupHistory(): Promise<CleanupHistorySummary[]> {
  return invoke<CleanupHistorySummary[]>('get_cleanup_history');
}

// ============================================================================
// Big directory analysis related API
// ============================================================================

/**
 * Big directory entry information */
export interface HotspotEntry {
  /** Full folder path */
  path: string;
  /** Folder name */
  name: string;
  /** Total size (bytes) */
  total_size: number;
  /** Number of files */
  file_count: number;
  /** Last modified time (Unix timestamp, milliseconds) */
  last_modified: number;
  /** Parent directory type (Local/Roaming/LocalLow/System/Program, etc.) */
  parent_type: string;
  /** Whether it is a cache directory */
  is_cache: boolean;
  /** Whether it is a program directory */
  is_program: boolean;
  /** Whether it is safe to clean (forced to false in deep scan mode) */
  is_safe_to_clean: boolean;
  /** Whether it is a system-protected directory (blacklisted directory) */
  is_protected: boolean;
  /** Child directory list (smart drill-down: when a directory is >5GB and has >1000 files, show the 3 largest subdirectories) */
  children: HotspotEntry[];
  /** Drill-down depth of the current directory (0 = top-level directory) */
  depth: number;
}

/**
 * Big directory scan result */
export interface HotspotScanResult {
  /** Big directory list (already sorted by size descending) */
  entries: HotspotEntry[];
  /** Number of folders scanned */
  total_folders_scanned: number;
  /** Scan elapsed time (milliseconds) */
  scan_duration_ms: number;
  /** Total size of the scan scope (AppData or C drive total) */
  scanned_total_size: number;
  /** Whether it is deep scan mode */
  is_full_scan: boolean;
}

/**
 * Scan progress event (pushed only during deep scans)
 */
export interface HotspotScanProgress {
  /** Directory currently being scanned */
  current_dir: string;
  /** Number of folders scanned */
  scanned_dirs: number;
  /** Number of big directories found (>=100MB) */
  found_entries: number;
  /** Total size of the scanned scope (bytes) */
  total_size: number;
  /** Total number of first-level directories (used for progress percentage) */
  total_first_level_dirs: number;
  /** Number of completed first-level directories (used for precise progress percentage) */
  completed_roots: number;
  /** Current scan backend: mft / walkdir */
  backend?: string;
  /** Current phase: mft / index / metadata / aggregate / result / walkdir */
  stage?: string;
  /** Current phase description, used by the frontend to show the bottleneck */
  message?: string;
  /** Total scan elapsed time, in milliseconds */
  elapsed_ms?: number;
  /** Current phase elapsed time, in milliseconds */
  stage_elapsed_ms?: number;
  /** Key numbers for the current phase (segments/disk reads/parsing, directory count, fallback stat count...), used to locate bottlenecks */
  detail?: string;
}

/**
 * Scans big directories. * @param topN Return Top N results, default 20
 * @param fullScan Whether to enable full-disk deep scan, default false (only scans AppData) *
 * [Safety] In deep scan mode, is_safe_to_clean is false for all results. * The frontend should disable the cleanup button, allowing only "Open Location" and "Search" actions.
 *
 * [Progress events] During deep scans, listen to `hotspot-scan:progress` for real-time progress. * `hotspot-scan:cancelled` means the scan was cancelled. */
export async function scanHotspot(
  topN?: number,
  fullScan?: boolean,
  maxDepth?: number,
  sizeThresholdMb?: number,
  ignoreSystemDirs?: boolean,
  driveLetter?: string,
): Promise<HotspotScanResult> {
  return invoke<HotspotScanResult>('scan_hotspot', { topN, fullScan, maxDepth, sizeThresholdMb, ignoreSystemDirs, driveLetter });
}

/**
 * Cancels the running big directory scan
 */
export async function cancelHotspotScan(): Promise<void> {
  return invoke<void>('cancel_hotspot_scan');
}

/**
 * Single-level path drill-down scan (dynamic drill-down feature)
 * Scans the direct child folders of a specified path, used to expand deep directory structures level by level
 * @param path Absolute path of the target directory to scan
 */
export async function scanPathDirect(path: string): Promise<HotspotScanResult> {
  return invoke<HotspotScanResult>('scan_path_direct', { path });
}

/**
 * Directory cleanup result
 */
export interface CleanupDirectoryResult {
  /** Number of files/directories successfully deleted */
  deleted_count: number;
  /** Number of deletion failures */
  failed_count: number;
  /** Amount of space released (bytes) */
  freed_size: number;
  /** List of error messages */
  errors: string[];
}

/**
 * Cleans directory contents (keeps the root directory). * @param path Directory path
 */
export async function cleanupDirectoryContents(path: string): Promise<CleanupDirectoryResult> {
  return invoke<CleanupDirectoryResult>('cleanup_directory_contents', { path });
}

// ============================================================================
// Context menu cleanup related API
// ============================================================================

/** Single context menu entry */
export interface ContextMenuEntry {
  /** Unique ID (reg_root + "||" + reg_subpath) */
  id: string;
  /** Menu display name (MUIVerb indirect string resolved) */
  display_name: string;
  /** Registry subkey name */
  key_name: string;
  /** Full registry path (for UI display) */
  registry_path: string;
  /** Registry root ("HKCU" | "HKLM") */
  reg_root: 'HKCU' | 'HKLM';
  /** Child path relative to the root */
  reg_subpath: string;
  /** Scope ("Any File", "Folder", "Desktop Background", "Disk Drive", "Library Folder") */
  scope: string;
  /** Icon path (raw, may have an index suffix) */
  icon_path: string | null;
  /** Raw command string */
  command: string | null;
  /** exe path extracted from the command */
  exe_path: string | null;
  /** Target program tri-state: ok = file exists / missing = file no longer exists (the only case counted as "invalid") / unknown = no path could be extracted */
  exe_status: 'ok' | 'missing' | 'unknown';
  /** Whether administrator privileges are required to delete */
  needs_admin: boolean;
  /** Whether it is a system-protected entry (cannot be selected for deletion) */
  is_system_protected: boolean;
  /** Risk level ("safe" | "caution" | "danger") */
  risk_level: string;
}

/** Context menu scan result */
export interface ContextMenuScanResult {
  /** All scanned entries */
  entries: ContextMenuEntry[];
  /** Number of invalid entries (exe does not exist) */
  invalid_count: number;
  /** Scan elapsed time (milliseconds) */
  scan_duration_ms: number;
}

/** Context menu entry deletion request */
export interface ContextMenuDeleteRequest {
  /** Unique entry ID */
  id: string;
  /** Registry root */
  reg_root: 'HKCU' | 'HKLM';
  /** Child path relative to the root */
  reg_subpath: string;
}

/** Deletion details of a single entry */
export interface ContextMenuDeleteDetail {
  /** Entry ID */
  id: string;
  /** Whether it succeeded */
  success: boolean;
  /** Failure reason */
  error: string | null;
}

/** Context menu deletion result */
export interface ContextMenuDeleteResult {
  /** Number of entries successfully deleted */
  deleted_count: number;
  /** Number of entries that failed to delete */
  failed_count: number;
  /** Detailed result of each entry */
  details: ContextMenuDeleteDetail[];
}

/**
 * Scans context menu entries in the Windows registry. *
 * Covers the core paths under HKCU and HKLM: *\shell, Directory\shell,
 * Directory\Background\shell, Drive\shell, etc. */
export async function scanContextMenu(): Promise<ContextMenuScanResult> {
  return invoke<ContextMenuScanResult>('scan_context_menu');
}

/**
 * Deletes the selected context menu registry entries
 * @param entries List of entries to delete
 */
export async function deleteContextMenuEntries(
  entries: ContextMenuDeleteRequest[]
): Promise<ContextMenuDeleteResult> {
  return invoke<ContextMenuDeleteResult>('delete_context_menu_entries', { entries });
}

/** Opens the .reg backup directory for context menu entries (for manual recovery after accidental deletion) */
export async function openContextMenuBackupDir(): Promise<void> {
  return invoke<void>('open_context_menu_backup_dir');
}

// ============================================================================
// System quick tools
// ============================================================================

/**
 * Opens the Task Manager Startup apps page. */
export async function openStartupManager(): Promise<void> {
  return invoke<void>('open_startup_manager');
}

/**
 * Opens the Windows Storage Sense settings page
 */
export async function openStorageSettings(): Promise<void> {
  return invoke<void>('open_storage_settings');
}

// ============================================================================
// Multi-disk full-disk change analysis API
// ============================================================================

/** Full-disk change entry */
export interface DiskGrowthDetailEntry {
  /** Path of the changed direct child directory */
  path: string;
  /** Child directory name, used for display in the popup list */
  name: string;
  /** Size in the previous snapshot */
  old_size: number;
  /** Size in this snapshot */
  new_size: number;
  /** Change amount vs the previous snapshot; positive means increase, negative means decrease */
  diff: number;
  /** Latest file modification time within this directory subtree, Unix seconds; 0 if missing from the old snapshot */
  modified: number;
  /** Detail change level */
  level: 'significant' | 'fast' | 'minor' | 'stable' | 'decreased' | 'new';
}

export interface DiskGrowthFileDetailEntry {
  /** Path of the changed file */
  path: string;
  /** File name, used for display in the popup list */
  name: string;
  /** Size in the previous snapshot */
  old_size: number;
  /** Size in this snapshot */
  new_size: number;
  /** Change amount vs the previous snapshot; positive means increase, negative means decrease */
  diff: number;
  /** File last modified time, Unix seconds; 0 if missing from the old snapshot */
  modified: number;
  /** File change level */
  level: 'significant' | 'fast' | 'minor' | 'stable' | 'decreased' | 'new';
}

export interface DiskGrowthFileDetailsResponse {
  /** Query directory */
  path: string;
  /** Previous scan time */
  previous_scan_time: string;
  /** Current scan time */
  current_scan_time: string;
  /** File-level change details */
  entries: DiskGrowthFileDetailEntry[];
  /** Actual number of changed files */
  total_changed_files: number;
  /** Number of files returned this time */
  returned_files: number;
  /** Pagination offset */
  offset: number;
  /** Whether there are more files */
  has_more: boolean;
}

export interface DiskGrowthDirectoryDetailsResponse {
  /** Query directory */
  path: string;
  /** Previous scan time */
  previous_scan_time: string;
  /** Current scan time */
  current_scan_time: string;
  /** Directory-level change details */
  entries: DiskGrowthDetailEntry[];
  /** Actual number of changed directories */
  total_changed_dirs: number;
  /** Number of directories returned this time */
  returned_dirs: number;
  /** Pagination offset */
  offset: number;
  /** Whether there are more directories */
  has_more: boolean;
}

export interface DiskGrowthEntry {
  /** Directory path */
  path: string;
  /** Size in the previous snapshot, 0 on first appearance */
  old_size: number;
  /** Size in this snapshot */
  new_size: number;
  /** Change amount vs the previous snapshot; positive means increase, negative means decrease */
  diff: number;
  /** Latest file modification time within this directory subtree, Unix seconds; 0 if missing from the old snapshot */
  modified: number;
  /** Percentage change relative to the previous snapshot */
  diff_percent: number;
  /** Used to indicate hint strength in the frontend, avoiding rendering small fluctuations as high risk */
  level: 'significant' | 'fast' | 'minor' | 'stable' | 'decreased' | 'new';
  /** Description generated by the backend from the path and change direction */
  explanation: string;
  /** Troubleshooting suggestion, not a claim that the directory can be cleaned directly */
  suggestion: string;
  /** Change details of the first-level subdirectories under this directory; the backend returns at most 50 by default */
  details: DiskGrowthDetailEntry[];
}

/** Multi-level directory change tree for HTML export, with at most three levels of subdirectories returned by the backend. */
export interface DiskGrowthExportNode {
  path: string;
  name: string;
  old_size: number;
  new_size: number;
  diff: number;
  modified: number;
  level: 'significant' | 'fast' | 'minor' | 'stable' | 'decreased' | 'new';
  children: DiskGrowthExportNode[];
}

/** HTML export directory tree response, explicitly stating whether the extreme-data-volume protection limit was triggered. */
export interface DiskGrowthExportTreeResponse {
  nodes: DiskGrowthExportNode[];
  total_nodes: number;
  truncated: boolean;
}

/** Full-disk change report */
export interface DiskGrowthReport {
  /** Changed directories sorted by absolute change amount, at most 300 returned by the backend */
  entries: DiskGrowthEntry[];
  /** Net change of the selected disk; positive means increase, negative means decrease */
  total_growth: number;
  /** Number of significantly grown directories */
  significant_count: number;
  /** Number of rapidly growing directories */
  fast_count: number;
  /** Number of newly added directories */
  new_count: number;
  /** Number of decreased directories */
  decreased_count: number;
  /** Time span between the two snapshots */
  time_span: string;
  /** Human-readable change summary */
  summary: string;
}

/** Full-disk directory analysis entry */
export interface DiskGrowthAnalyzeEntry {
  /** Directory path */
  path: string;
  /** Aggregated size of the current directory */
  size: number;
  /** Aggregation depth of the full-disk scan */
  depth?: number;
  /** Directory category, used only to help the user locate the source */
  category: string;
  /** Latest file modification time within this directory subtree, Unix seconds; 0 if missing from the old snapshot */
  modified: number;
  /** Description of the change or usage reason */
  reason: string;
  /** Troubleshooting suggestion, no promise that it can be deleted */
  suggestion: string;
  /** Display fields compatible with the ModuleCard row component; full-disk analysis makes no risk judgment */
  risk?: 'safe' | 'warning' | 'dangerous';
  /** Compatible with the old row data structure; full-disk analysis provides no cleanup action */
  action?: 'delete' | 'suggest' | 'ignore' | 'protect';
  /** Full-disk analysis matches no cleanup rules; empty values are kept so old display logic can be reused */
  matched_rule_id?: string | null;
  /** Full-disk analysis has no rule tags yet */
  tags?: string[];
}

/** Full-disk directory analysis result */
export interface DiskGrowthAnalyzeResult {
  /** Changed directories; falls back to the currently largest directories by usage on the first scan */
  entries: DiskGrowthAnalyzeEntry[];
  /** Absolute value of this change */
  changed_size: number;
  /** Space added this time */
  increased_size: number;
  /** Space reduced this time */
  decreased_size: number;
}

/** Full-disk change scan response for the selected disk */
export interface DiskGrowthScanResponse {
  /** Drive letter of the disk scanned this time */
  drive_letter: string;
  /** Total size of files on the selected disk aggregated via MFT this time */
  total_size: number;
  /** Number of files whose size was read successfully this time */
  total_files_scanned: number;
  /** Scan elapsed time, in milliseconds */
  scan_duration_ms: number;
  /** Scan root path */
  root_path: string;
  /** Previous snapshot time, empty on the first scan */
  previous_scan_time?: string | null;
  /** Current snapshot time */
  current_scan_time: string;
  /** Scan backend, currently mft */
  backend: string;
  /** Per-phase backend timing, used to judge performance bottlenecks */
  phase_durations: DiskGrowthPhaseDuration[];
  /** Number of files whose size was parsed directly from MFT FILE records */
  mft_size_count: number;
  /** Number of files that fell back to metadata after MFT size parsing failed */
  metadata_fallback_count: number;
  /** Number of data run segments in $MFT */
  mft_fragments?: number;
  /** Time to read $MFT and other disk data */
  mft_io_ms?: number;
  /** Time to parse $MFT records */
  mft_parse_ms?: number;
  /** Analysis result */
  analyze: DiskGrowthAnalyzeResult;
  /** Change report vs the previous snapshot */
  growth: DiskGrowthReport;
}

/** Backend scan phase timing */
export interface DiskGrowthPhaseDuration {
  /** Phase id: mft/path/metadata/aggregate */
  stage: string;
  /** Phase elapsed time, in milliseconds */
  duration_ms: number;
}

/** Backend scan phase progress event */
export interface DiskGrowthScanProgress {
  /** Phase id: mft/path/metadata/aggregate */
  stage: string;
  /** Current phase description */
  message: string;
  /** Number processed */
  processed: number;
  /** Total number, empty when it cannot be estimated */
  total?: number | null;
  /** Total scan elapsed time, in milliseconds */
  elapsed_ms: number;
}

/**
 * Scans the selected disk and compares it with the previous snapshot.
 * Only the full-disk change analysis entry is kept here, to prevent the frontend from continuing to misuse the old ProgramData cleanup command.
 */
export async function scanDiskGrowth(
  maxChangeEntries?: number,
  driveLetter?: string
): Promise<DiskGrowthScanResponse> {
  return invoke<DiskGrowthScanResponse>('scan_disk_growth', { maxChangeEntries, driveLetter });
}

export async function cancelDiskGrowthScan(): Promise<void> {
  return invoke<void>('cancel_disk_growth_scan');
}

/**
 * Changed files of one directory, paged through one ordering.
 *
 * `sort` decides both the order and which files land on which page — the backend prunes deep
 * subtrees by the same key, so pass the active column here instead of sorting afterwards.
 * `desc` may be omitted to use the natural direction of `sort` (names A→Z, sizes largest first).
 */
export async function getDiskGrowthFileDetails(
  path: string,
  offset?: number,
  limit?: number,
  driveLetter?: string,
  sort?: 'diff' | 'size' | 'name',
  desc?: boolean
): Promise<DiskGrowthFileDetailsResponse> {
  return invoke<DiskGrowthFileDetailsResponse>('get_disk_growth_file_details', {
    path,
    offset,
    limit,
    driveLetter,
    sort,
    desc,
  });
}

export async function getDiskGrowthDirectoryDetails(
  path: string,
  offset?: number,
  limit?: number,
  driveLetter?: string
): Promise<DiskGrowthDirectoryDetailsResponse> {
  return invoke<DiskGrowthDirectoryDetailsResponse>('get_disk_growth_directory_details', { path, offset, limit, driveLetter });
}

/** Gets up to three levels of directory change trees for HTML export. */
export async function getDiskGrowthExportTree(
  paths: string[],
  maxDepth?: number,
  driveLetter?: string,
): Promise<DiskGrowthExportTreeResponse> {
  return invoke<DiskGrowthExportTreeResponse>('get_disk_growth_export_tree', { paths, maxDepth, driveLetter });
}

/** Exports the disk-space change HTML via the system save dialog. */
export async function saveDiskGrowthHtml(
  content: string,
  defaultFileName: string,
  dialogTitle: string,
): Promise<string | null> {
  return invoke<string | null>('save_disk_growth_html', { content, defaultFileName, dialogTitle });
}

// ============================================================================
// AI asset analysis API
// ============================================================================

/** A single model or cache entry in AI assets */
export interface AiModelItem {
  name: string;
  size: number;
  path: string;
}

/** AI asset source aggregated by platform or deep-discovery source */
export interface AiAssetSource {
  name: string;
  path: string;
  total_size: number;
  model_count: number;
  models: AiModelItem[];
}

/** AI asset scan result, from which the frontend derives home-page insights and the list */
export interface AiModelScanResult {
  total_size: number;
  total_model_count: number;
  source_count: number;
  sources: AiAssetSource[];
  warnings: string[];
  scan_duration_ms: number;
  discovery_mode: 'quick' | 'deep';
  phase_durations: AiModelPhaseDuration[];
}

/** AI model-space scan phase timing, used to explain the MFT fallback bottleneck */
export interface AiModelPhaseDuration {
  stage: string;
  label: string;
  duration_ms: number;
}

/** Real-time phase feedback for the AI model-space scan */
export interface AiModelScanProgress {
  stage: string;
  message: string;
  elapsed_ms: number;
  stage_elapsed_ms: number;
  /** Drive letters involved in the current phase (only in the MFT phase), used for phase-text interpolation */
  drive_label?: string | null;
}

/**
 * Quickly scans known AI platform directories.
 * Deep discovery is enabled explicitly by the user; only then does the backend add an MFT fallback scan, avoiding the full-disk IO pressure of default behavior.
 */
export async function scanAiModelAssets(enableDeepDiscovery: boolean): Promise<AiModelScanResult> {
  return invoke<AiModelScanResult>('scan_ai_model_assets', { enableDeepDiscovery });
}

/** Deletes a single AI model file; the backend re-validates the model format and file safety boundaries. */
export async function deleteAiModel(path: string): Promise<EnhancedDeleteResult> {
  return invoke<EnhancedDeleteResult>('delete_ai_model', { path });
}

// ============================================================================
// Shell icon cleanup API
// ============================================================================

export interface ShellIconInfo {
  entryKind: 'myComputer' | 'navigationPane' | string;
  clsid: string;
  name: string;
  applicationName?: string | null;
  regPath: string;
  hive: string;
  registryView: string;
  sourcePath?: string | null;
  riskLevel: 'safe' | 'protected' | 'unknown' | 'locked' | string;
  riskReason: string;
  isSystemProtected: boolean;
  isLocked: boolean;
}

export interface ShellIconTarget {
  clsid: string;
  hive: string;
  registryView: string;
  entryKind: 'myComputer' | 'navigationPane';
}

export interface ShellIconOperationResult {
  success: boolean;
  message: string;
  backupPath?: string | null;
  needsExplorerRefresh: boolean;
}

/** Scans third-party shell entries in Explorer and returns categories by mount location. */
export async function scanShellIcons(): Promise<ShellIconInfo[]> {
  return invoke<ShellIconInfo[]>('scan_shell_icons');
}

/** mode=1 deletes the node, mode=2 performs a thorough cleanup and prevents re-creation. */
export async function removeShellIcon(target: ShellIconTarget, mode: 1 | 2): Promise<ShellIconOperationResult> {
  return invoke<ShellIconOperationResult>('remove_shell_icon', { target, mode });
}

/** Restores only the original ACL, for scenarios where software is allowed to re-register but icons should not return immediately. */
export async function unlockShellIcon(target: ShellIconTarget): Promise<ShellIconOperationResult> {
  return invoke<ShellIconOperationResult>('unlock_shell_icon', { target });
}

/** Restores registry content and the original ACL from the most recent backup. */
export async function restoreShellIcon(target: ShellIconTarget): Promise<ShellIconOperationResult> {
  return invoke<ShellIconOperationResult>('restore_shell_icon', { target });
}

export async function restartExplorer(): Promise<void> {
  return invoke<void>('restart_explorer');
}

export async function openShellIconBackupDir(): Promise<void> {
  return invoke<void>('open_shell_icon_backup_dir');
}

/** Opens the virtual-disk operation log; the log file is under logs in the current config data directory. */
export async function openShellIconLog(): Promise<void> {
  return invoke<void>('open_shell_icon_log');
}

/** Locates the target shell node via the registry editor's LastKey. */
export async function openShellIconRegistry(target: ShellIconTarget): Promise<void> {
  return invoke<void>('open_shell_icon_registry', { target });
}

// ============================================================================
// Data directory management API
// ============================================================================

/**
 * Gets the current data directory path
 */
export async function getDataDirectory(): Promise<string> {
  return invoke<string>('get_data_directory');
}

/**
 * Sets the data directory and migrates existing data. * @param path New data directory path
 */
export async function setDataDirectory(path: string): Promise<string> {
  return invoke<string>('set_data_directory', { path });
}

export interface ClearableDataItem {
  id: string;
  label: string;
  description: string;
  path: string;
  item_type: 'file' | 'directory';
  exists: boolean;
  file_count: number;
  size: number;
  warning?: string | null;
}

export interface ClearLocalDataResult {
  deleted_files: number;
  freed_bytes: number;
  /** Number of items successfully cleaned */
  cleared_items: number;
  /**
   * Items that failed to clean.
   *
   * A single-item failure does not interrupt the whole batch: the rest are cleaned as usual, and only the failed ones are returned here,
   * so the UI can tell the user "which item wasn't deleted and why".
   */
  failed_items: ClearLocalDataFailure[];
}

/** Failure reason for a single cleanup item */
export interface ClearLocalDataFailure {
  /** Cleanup item identifier */
  item: string;
  /** Failure reason (can be displayed directly) */
  reason: string;
}

/**
 * Clears local data (installation history cache, cleanup logs, registry backups, and module-specific records).
 * @returns [number of files deleted, number of bytes released]
 */
export async function clearLocalData(): Promise<[number, number]> {
  return invoke<[number, number]>('clear_local_data');
}

export async function listClearableDataItems(): Promise<ClearableDataItem[]> {
  return invoke<ClearableDataItem[]>('list_clearable_data_items');
}

export async function clearSelectedLocalData(itemIds: string[]): Promise<ClearLocalDataResult> {
  return invoke<ClearLocalDataResult>('clear_selected_local_data', { itemIds });
}

/**
 * Opens the system folder-picker dialog. * @returns The folder path selected by the user, or null if cancelled
 */
export async function pickFolderDialog(): Promise<string | null> {
  return invoke<string | null>('pick_folder_dialog');
}

// ============================================================================
// Background wallpaper
// ============================================================================

/** Opens the system image picker; returns null on cancel. */
export async function pickWallpaperDialog(): Promise<string | null> {
  return invoke<string | null>('pick_wallpaper_dialog');
}

/** Installs the selected image into the data directory and returns the file name (used with the data directory to build the asset URL). */
export async function installWallpaper(source: string): Promise<string> {
  return invoke<string>('install_wallpaper', { source });
}

/** Clears the installed wallpaper. */
export async function clearWallpaper(): Promise<void> {
  return invoke<void>('clear_wallpaper');
}

/** Gets the absolute path of the current wallpaper; returns an empty string if none is installed. */
export async function getWallpaperPath(): Promise<string> {
  return invoke<string>('get_wallpaper_path');
}
