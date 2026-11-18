// ============================================================================
// Global "folders only" preference shared by every file tree in the app
//
// Why a module-level store instead of local component state: the same switch
// governs junk cleanup, social cleanup, the card list and the disk-growth
// detail tree. Flipping it in one window must not leave another window still
// showing files, and the choice has to survive a restart.
//
// Default is ON: a folder row already carries "N files · X MB", so the folder
// level is what users scan; individual files are the drill-down detail.
// ============================================================================

import { useCallback, useSyncExternalStore } from 'react';

const STORAGE_KEY = 'lightc.fileTree.foldersOnly';

const listeners = new Set<() => void>();
/** Mirror of the stored value; null = not read yet */
let cached: boolean | null = null;

function readValue(): boolean {
  if (cached !== null) return cached;
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    // Only an explicit "false" turns it off; a missing value keeps the default
    cached = raw !== 'false';
  } catch {
    cached = true;
  }
  return cached;
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function setFoldersOnly(next: boolean): void {
  cached = next;
  try {
    localStorage.setItem(STORAGE_KEY, String(next));
  } catch {
    // Private mode or a full quota: the in-memory choice still applies for this session
  }
  for (const listener of listeners) listener();
}

/** [current, set] — every consumer re-renders when any of them switches it */
export function useFoldersOnly(): [boolean, (next: boolean) => void] {
  const value = useSyncExternalStore(subscribe, readValue, () => true);
  const update = useCallback((next: boolean) => setFoldersOnly(next), []);
  return [value, update];
}
