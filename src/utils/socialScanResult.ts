// ============================================================================
// Local incremental update of social scan results
//
// If a full rescan is rerun after a successful deletion, the cost is re-traversing every app's data directory,
// and it would reset the user's selections wholesale (handleScan selects all deletable files by default).
// Here we remove deleted files from the result locally and recompute statistics so the UI reflects the change immediately.
//
// Recompute the top-level totals as "the sum of categories" - that is exactly how the backend core.rs aggregates them
// (total_files / total_size / deletable_* are all sums over categories),
// so the local recomputation matches a rescan and introduces no semantic difference.
// ============================================================================

import type { SocialCategoryStats, SocialScanResult } from '../api/commands';

/** Statistical changes caused by removed files in a single category */
interface CategoryRemoval {
  count: number;
  size: number;
  deletableCount: number;
  deletableSize: number;
}

const EMPTY_REMOVAL: CategoryRemoval = { count: 0, size: 0, deletableCount: 0, deletableSize: 0 };

/**
 * Remove successfully deleted paths from the scan result and recompute category and top-level statistics.
 *
 * Only pass paths **confirmed deleted by the backend**: files marked "delete after reboot" are still on disk,
 * counting them would make the UI show them as deleted. Callers must exclude such paths themselves.
 *
 * Returns a new object; does not modify the input.
 */
export function applyDeletedPaths(
  result: SocialScanResult,
  removedPaths: Set<string>,
): SocialScanResult {
  if (removedPaths.size === 0) {
    return result;
  }

  const categories: SocialCategoryStats[] = [];

  for (const category of result.categories) {
    const removal = countRemoval(category, removedPaths);

    if (removal.count === 0) {
      categories.push(category);
      continue;
    }

    categories.push({
      ...category,
      files: category.files.filter(file => !removedPaths.has(file.path)),
      file_count: Math.max(0, category.file_count - removal.count),
      total_size: Math.max(0, category.total_size - removal.size),
      deletable_count: Math.max(0, category.deletable_count - removal.deletableCount),
      deletable_size: Math.max(0, category.deletable_size - removal.deletableSize),
    });
  }

  return {
    ...result,
    categories,
    total_files: categories.reduce((sum, category) => sum + category.file_count, 0),
    total_size: categories.reduce((sum, category) => sum + category.total_size, 0),
    deletable_files: categories.reduce((sum, category) => sum + category.deletable_count, 0),
    deletable_size: categories.reduce((sum, category) => sum + category.deletable_size, 0),
  };
}

/** Count the number and size of files to be removed in a category */
function countRemoval(category: SocialCategoryStats, removedPaths: Set<string>): CategoryRemoval {
  let removal = EMPTY_REMOVAL;

  for (const file of category.files) {
    if (!removedPaths.has(file.path)) {
      continue;
    }
    removal = {
      count: removal.count + 1,
      size: removal.size + file.size,
      deletableCount: removal.deletableCount + (file.deletable ? 1 : 0),
      deletableSize: removal.deletableSize + (file.deletable ? file.size : 0),
    };
  }

  return removal;
}

/**
 * Pick out the paths "confirmed deleted by the backend" from a delete result.
 *
 * Returning null means a local update cannot be done safely; the caller should fall back to a rescan:
 *   - The delete result contains files marked "delete after reboot": these are still on disk and the backend does not return their paths,
 *     so they cannot be distinguished from actually deleted files;
 *   - The sum of success / failure / reboot-pending does not match the requested count: some paths were not categorized,
 *     in which case a local update could under- or over-delete.
 */
export function resolveDeletedPaths(
  requestedPaths: string[],
  deleteResult: {
    success_count: number;
    failed_count: number;
    reboot_pending_count: number;
    failed_files?: Array<{ path: string }> | null;
  },
): Set<string> | null {
  const accounted = deleteResult.success_count
    + deleteResult.failed_count
    + deleteResult.reboot_pending_count;

  if (deleteResult.reboot_pending_count > 0 || accounted !== requestedPaths.length) {
    return null;
  }

  const failedPaths = new Set((deleteResult.failed_files ?? []).map(item => item.path));
  return new Set(requestedPaths.filter(path => !failedPaths.has(path)));
}
