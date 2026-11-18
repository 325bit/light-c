import { openUrl } from '@tauri-apps/plugin-opener';

export type SearchEngine = 'bing' | 'google' | 'baidu';

export const SEARCH_ENGINE_STORAGE_KEY = 'lightc.searchEngine';

export const SEARCH_ENGINE_CHANGED_EVENT = 'lightc:search-engine-changed';

const WINDOWS_DEVICE_PREFIX = '\\\\?\\';
const WINDOWS_UNC_DEVICE_PREFIX = '\\\\?\\UNC\\';

export const SEARCH_ENGINE_OPTIONS: Array<{ value: SearchEngine; label: string }> = [
  { value: 'bing', label: 'Bing' },
  { value: 'google', label: 'Google' },
  { value: 'baidu', label: '百度' },
];

/**
 * Strip the Windows long-path device prefix so search engines do not treat \\?\ as path content, reducing search result readability.
 * UNC long paths must be restored to the standard \\server form; do not simply leave the UNC text.
 */
export function stripWindowsDevicePrefix(value: string): string {
  return value
    .split(WINDOWS_UNC_DEVICE_PREFIX).join('\\\\')
    .split(WINDOWS_DEVICE_PREFIX).join('');
}

const SEARCH_ENGINE_URLS: Record<SearchEngine, string> = {
  bing: 'https://www.bing.com/search?q=',
  google: 'https://www.google.com/search?q=',
  baidu: 'https://www.baidu.com/s?wd=',
};

export function isSearchEngine(value: unknown): value is SearchEngine {
  return value === 'bing' || value === 'google' || value === 'baidu';
}

export function getStoredSearchEngine(): SearchEngine {
  try {
    const storedValue = localStorage.getItem(SEARCH_ENGINE_STORAGE_KEY);
    return isSearchEngine(storedValue) ? storedValue : 'bing';
  } catch {
    return 'bing';
  }
}

export function setStoredSearchEngine(engine: SearchEngine) {
  // The search engine is a frontend-only preference and does not need to be written to backend config; the broadcast event lets mounted modules pick up the new setting immediately.
  localStorage.setItem(SEARCH_ENGINE_STORAGE_KEY, engine);
  window.dispatchEvent(new CustomEvent<SearchEngine>(SEARCH_ENGINE_CHANGED_EVENT, { detail: engine }));
}

export async function openSearchUrl(searchText: string) {
  const engine = getStoredSearchEngine();
  // All modules strip the device prefix uniformly at the search entry point, so callers cannot miss it and include \\?\ in the search term.
  const query = encodeURIComponent(stripWindowsDevicePrefix(searchText));
  await openUrl(`${SEARCH_ENGINE_URLS[engine]}${query}`);
}
