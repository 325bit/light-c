// ============================================================================
// Debug entry for the portable update modal (registered only in dev builds)
//
// Normally the portable modal appears only when "Check for Updates" actually finds a new version, which is hard to reproduce in local development.
// Here a window method is attached in dev builds, so you can bring up the modal directly from the dev console to inspect styling:
//
//   window.__lightcDebugPortableUpdate()
//   window.__lightcDebugPortableUpdate({ latestVersion: '2.16.9', currentVersion: '2.16.8' })
//   window.__lightcDebugPortableUpdate({ latestVersion: null })   // show channel guidance only
//   window.__lightcDebugPortableUpdate({ isChecking: true })      // checking state
//
// Production builds do not register this global method, to avoid shipping the debug entry in release packages.
// ============================================================================

declare global {
  interface Window {
    __lightcDebugPortableUpdate?: (overrides?: {
      latestVersion?: string | null;
      currentVersion?: string;
      isChecking?: boolean;
    }) => void;
  }
}

/** Registers the debug entry; calling it again in the same session overwrites the previous implementation. */
export function registerPortableUpdateDebugTrigger(
  open: (overrides?: {
    latestVersion?: string | null;
    currentVersion?: string;
    isChecking?: boolean;
  }) => void,
): void {
  window.__lightcDebugPortableUpdate = open;
}
