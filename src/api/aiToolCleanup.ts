/**
 * Invocation layer for AI tool cleanup.
 *
 * Only forwards invoke calls; fields correspond to `src/types/aiToolCleanup.ts`.
 */
import { invoke } from '@tauri-apps/api/core';
import type {
  AiCacheDeleteResult,
  AiCacheFileList,
  AiToolScanResult,
} from '../types/aiToolCleanup';

/** Scan the cleanable caches of each AI tool */
export function scanAiToolCaches(): Promise<AiToolScanResult> {
  return invoke('scan_ai_tool_caches');
}

/**
 * Delete the selected cache directories, returning the success count and failure details.
 *
 * The backend validates path ownership again (no `..`, directory name within the tool's allowed range, and under its root),
 * so even a tampered path passed from here cannot delete anything out of bounds.
 *
 * You must check `failed` in the return value: when a tool is running, cache files are often locked and deletion partially fails;
 * reading only the success count would make the UI show "all deleted" while the actual usage has not dropped.
 */
export function deleteAiToolCaches(paths: string[]): Promise<AiCacheDeleteResult> {
  return invoke('delete_ai_tool_caches', { paths });
}

/**
 * List all files under a cache directory.
 *
 * Called on demand: a single tool may have thousands of files; the scan stage only gives a summary,
 * and details are fetched when the user clicks "View All", to avoid huge scan results.
 * Over the limit it will be truncated by the backend; when `truncated` is true the UI must state that only the first N entries are shown.
 */
export function listAiCacheFiles(path: string): Promise<AiCacheFileList> {
  return invoke('list_ai_cache_files', { path });
}
