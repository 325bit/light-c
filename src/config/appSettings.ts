// ============================================================================
// App settings schema and local persistence
//
// Kept outside SettingsContext because the settings are read in two places that live in
// different lifetimes: the React settings page (after mount) and the startup elevation
// request (before React mounts, in main.tsx). The storage key, defaults, and normalization
// rules must have exactly one definition, otherwise a new field added on one side silently
// disappears on the other.
// ============================================================================

import { APP_MODULE_META, DEFAULT_ACTIVE_MODULE_ID, type AppModuleId } from './moduleMeta';
// Type-only import: erased at compile time, so reading settings does not pull the i18n
// runtime initialization into the module graph (SettingsContext already imports it).
import type { Language } from '../i18n';

/** App settings */
export interface AppSettings {
  /** UI language, manually selected and persisted together with other app settings. */
  language: Language;
  /** Currently active feature module, saved in global settings to keep position after switching. */
  activeModuleId: AppModuleId;
  /** Big directory analysis depth (2-4, default 3) */
  hotspotDepth: number;
  /** Big directory size threshold in MB (10-500, default 50) */
  hotspotSizeThreshold: number;
  /** Whether to ignore system directories during deep scans (default true, preserves existing behavior) */
  hotspotIgnoreSystemDirs: boolean;
  /** Maximum number of files returned by large-file cleanup (default 50) */
  bigFilesScanLimit: number;
  /**
   * Directory recursion depth for junk cleanup's "Deep Discovery" scan (4-24, default 16).
   * Increasing it reaches deeper cache directories, but each level multiplies the number of directories to traverse, making scans noticeably slower.
   */
  junkDeepScanDepth: number;
  /** Maximum number of changed directories shown in disk-change analysis (default 300) */
  diskGrowthMaxEntries: number;
  /** Maximum number of files kept in cleanup logs (default 10) */
  cleanupLogRetention: number;
  /**
   * Whether to show the AI assistant entry (floating ball at bottom-right + AI buttons in each list row).
   *
   * Enabled by default. Turning it off only hides the entry; all configured providers and API keys are retained —
   * some users haven't configured a provider and don't want these two icons occupying the UI all the time.
   */
  aiAssistantEnabled: boolean;
  /**
   * Whether to ask for administrator rights (UAC) every time the app starts.
   *
   * Off by default: a system prompt on every launch is intrusive for users who only clean caches.
   * When enabled, startup requests elevation once; declining the prompt is respected and the app
   * keeps running unelevated, so users can still bail out without turning the setting off.
   */
  requestAdminOnStartup: boolean;
}

/** localStorage key holding the whole settings object; never change it, or every user's settings reset. */
export const APP_SETTINGS_STORAGE_KEY = 'c-cleanup-settings';

const moduleIds = APP_MODULE_META.map(module => module.id);

/** Default settings */
export const defaultAppSettings: AppSettings = {
  language: 'zh',
  activeModuleId: DEFAULT_ACTIVE_MODULE_ID,
  hotspotDepth: 3,     // default analysis depth 3 levels
  hotspotSizeThreshold: 50, // default 50MB
  hotspotIgnoreSystemDirs: true, // default ignore system directories
  bigFilesScanLimit: 50, // default scan 50 large files, to avoid an overly long initial result list
  // One notch looser than the previous hard-coded 12 levels: deep scanning is meant to find deeply hidden caches, too shallow misses them.
  // Cap stays at 24; users who find it slow can dial it back themselves.
  junkDeepScanDepth: 16,
  diskGrowthMaxEntries: 300, // default show at most 300 changed directories
  cleanupLogRetention: 10, // default keep 10 cleanup logs, preserving historical behavior
  aiAssistantEnabled: true, // default show AI assistant entry, keeping what old users see
  requestAdminOnStartup: false, // default off: elevation only happens when the user asks for it
};

export function normalizeAppSettings(settings: AppSettings): AppSettings {
  const language: Language =
    settings.language === 'en' || settings.language === 'ja' || settings.language === 'zh-TW' ? settings.language : 'zh';
  const activeModuleId = moduleIds.includes(settings.activeModuleId)
    ? settings.activeModuleId
    : DEFAULT_ACTIVE_MODULE_ID;

  return {
    ...settings,
    language,
    activeModuleId,
    // These settings directly affect the number of scan results; clamp boundary values when reading local cache to avoid manual tampering causing abnormal UI or backend pressure.
    hotspotDepth: Math.min(4, Math.max(2, Number(settings.hotspotDepth) || defaultAppSettings.hotspotDepth)),
    hotspotSizeThreshold: Math.min(500, Math.max(10, Number(settings.hotspotSizeThreshold) || defaultAppSettings.hotspotSizeThreshold)),
    bigFilesScanLimit: Math.min(500, Math.max(10, Math.floor(Number(settings.bigFilesScanLimit) || defaultAppSettings.bigFilesScanLimit))),
    junkDeepScanDepth: Math.min(24, Math.max(4, Math.floor(Number(settings.junkDeepScanDepth) || defaultAppSettings.junkDeepScanDepth))),
    diskGrowthMaxEntries: Math.min(1000, Math.max(50, Number(settings.diskGrowthMaxEntries) || defaultAppSettings.diskGrowthMaxEntries)),
    cleanupLogRetention: Math.min(100, Math.max(1, Math.floor(Number(settings.cleanupLogRetention) || defaultAppSettings.cleanupLogRetention))),
    // Only an explicitly stored false counts as off: older caches don't have this field at all, so it must stay enabled by default
    aiAssistantEnabled: settings.aiAssistantEnabled !== false,
    // Mirror of the line above: old caches have no such field, and only an explicit true may trigger a UAC prompt at startup.
    requestAdminOnStartup: settings.requestAdminOnStartup === true,
  };
}

/**
 * Read the saved settings from localStorage.
 *
 * Safe to call before React mounts; returns defaults when storage is unavailable or the
 * cached JSON is corrupted (a broken cache must never block startup).
 */
export function readStoredAppSettings(): AppSettings {
  if (typeof window === 'undefined') {
    return defaultAppSettings;
  }

  try {
    const saved = window.localStorage.getItem(APP_SETTINGS_STORAGE_KEY);
    if (!saved) return defaultAppSettings;
    return normalizeAppSettings({ ...defaultAppSettings, ...(JSON.parse(saved) as Partial<AppSettings>) });
  } catch (error) {
    console.error('读取设置失败:', error);
    return defaultAppSettings;
  }
}
