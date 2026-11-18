/**
 * Invocation layer for dev caches, other software caches and multi-version leftovers.
 *
 * Only forwards invoke calls; fields correspond to `src/types/devCache.ts`.
 */
import { invoke } from '@tauri-apps/api/core';
import type {
  DevCacheDeleteResult,
  DevCacheScanResult,
  VersionLeftoverScanResult,
} from '../types/devCache';

/**
 * Scan toolchain caches (the "dev cache cleanup" page).
 *
 * The backend traverses in parallel internally with rayon and returns all groups in one call - there are no progress events,
 * so the UI should use a "scanning" skeleton rather than a progress bar (a progress bar needs a total, which is unavailable here).
 */
export function scanDevCache(): Promise<DevCacheScanResult> {
  return invoke('scan_dev_cache');
}

/**
 * Scan multi-version leftovers only (the "leftover & cache" page).
 *
 * Same shape as `scanDevCache`: one call, no progress events. The two scans are independent on the backend,
 * so a page only pays for the directories it actually displays.
 */
export function scanVersionLeftovers(): Promise<VersionLeftoverScanResult> {
  return invoke('scan_version_leftovers');
}

/**
 * Scan the "other software caches" group only (the "leftover & cache" page).
 *
 * Downloaded driver installers / launcher web caches (NVIDIA, Epic, Steam, DaVinci…): garbage cleanup and driver
 * cleanup both leave them alone. Same result shape as `scanDevCache`, so the page reuses one renderer — but the
 * backend measures **only this group**, so neither page pays for the other's directories.
 */
export function scanOtherAppCaches(): Promise<DevCacheScanResult> {
  return invoke('scan_other_app_caches');
}

/**
 * Delete the selected directories, returning the success count, space freed, and failure details.
 *
 * Paths from the frontend are **untrusted**: the backend revalidates ownership (must exactly equal a target directory,
 * or be a version directory under a version root); readonly items and the version currently in use are always rejected.
 *
 * You must check `failed` in the return value: when an IDE / build process is running, files are often locked and deletion partially fails;
 * looking only at `deleted` would make the UI show "all deleted" while the actual usage has not dropped.
 */
export function deleteDevCachePaths(paths: string[]): Promise<DevCacheDeleteResult> {
  return invoke('delete_dev_cache_paths', { paths });
}
