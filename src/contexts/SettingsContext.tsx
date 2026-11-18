// ============================================================================
// App settings context - manages various toggle settings
// ============================================================================

import { createContext, useContext, useState, useCallback, useEffect, type ReactNode } from 'react';
import { APP_MODULE_META, DEFAULT_ACTIVE_MODULE_ID, type AppModuleId } from '../config/moduleMeta';
import i18n, { type Language } from '../i18n';

/** App settings */
interface AppSettings {
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
}

interface SettingsContextValue {
  /** Current settings */
  settings: AppSettings;
  /** Updates the settings */
  updateSettings: (updates: Partial<AppSettings>) => void;
}

const SettingsContext = createContext<SettingsContextValue | null>(null);

const STORAGE_KEY = 'c-cleanup-settings';
const moduleIds = APP_MODULE_META.map(module => module.id);

/** Default settings */
const defaultSettings: AppSettings = {
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
};

function normalizeSettings(settings: AppSettings): AppSettings {
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
    hotspotDepth: Math.min(4, Math.max(2, Number(settings.hotspotDepth) || defaultSettings.hotspotDepth)),
    hotspotSizeThreshold: Math.min(500, Math.max(10, Number(settings.hotspotSizeThreshold) || defaultSettings.hotspotSizeThreshold)),
    bigFilesScanLimit: Math.min(500, Math.max(10, Math.floor(Number(settings.bigFilesScanLimit) || defaultSettings.bigFilesScanLimit))),
    junkDeepScanDepth: Math.min(24, Math.max(4, Math.floor(Number(settings.junkDeepScanDepth) || defaultSettings.junkDeepScanDepth))),
    diskGrowthMaxEntries: Math.min(1000, Math.max(50, Number(settings.diskGrowthMaxEntries) || defaultSettings.diskGrowthMaxEntries)),
    cleanupLogRetention: Math.min(100, Math.max(1, Math.floor(Number(settings.cleanupLogRetention) || defaultSettings.cleanupLogRetention))),
    // Only an explicitly stored false counts as off: older caches don't have this field at all, so it must stay enabled by default
    aiAssistantEnabled: settings.aiAssistantEnabled !== false,
  };
}

interface SettingsProviderProps {
  children: ReactNode;
}

export function SettingsProvider({ children }: SettingsProviderProps) {
  // Read saved settings from localStorage
  const [settings, setSettings] = useState<AppSettings>(() => {
    if (typeof window !== 'undefined') {
      try {
        const saved = localStorage.getItem(STORAGE_KEY);
        if (saved) {
          return normalizeSettings({ ...defaultSettings, ...JSON.parse(saved) });
        }
      } catch (e) {
        console.error('读取设置失败:', e);
      }
    }
    return defaultSettings;
  });

  useEffect(() => {
    // Settings may come from an older cache version; switch them uniformly after the Provider takes effect, to avoid reading an invalid language on first paint.
    void i18n.changeLanguage(settings.language);
  }, [settings.language]);

  // Updates the settings
  const updateSettings = useCallback((updates: Partial<AppSettings>) => {
    setSettings(prev => {
      const newSettings = normalizeSettings({ ...prev, ...updates });
      localStorage.setItem(STORAGE_KEY, JSON.stringify(newSettings));
      return newSettings;
    });
  }, []);

  return (
    <SettingsContext.Provider value={{ settings, updateSettings }}>
      {children}
    </SettingsContext.Provider>
  );
}

export function useSettings(): SettingsContextValue {
  const context = useContext(SettingsContext);
  if (!context) {
    throw new Error('useSettings 必须在 SettingsProvider 内部使用');
  }
  return context;
}
