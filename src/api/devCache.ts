/**
 * Invocation layer for dev-space cleanup.
 *
 * Only forwards invoke calls; fields correspond to `src/types/devCache.ts`.
 */
import { invoke } from '@tauri-apps/api/core';
import type { DevCacheDeleteResult, DevCacheScanResult } from '../types/devCache';

/**
 * Scan toolchain caches and multi-version leftovers.
 *
 * The backend traverses in parallel internally with rayon and returns all groups in one call - there are no progress events,
 * so the UI should use a "scanning" skeleton rather than a progress bar (a progress bar needs a total, which is unavailable here).
 */
export function scanDevCache(): Promise<DevCacheScanResult> {
  return invoke('scan_dev_cache');
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
