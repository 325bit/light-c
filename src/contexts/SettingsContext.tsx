// ============================================================================
// App settings context - manages various toggle settings
// ============================================================================

import { createContext, useContext, useState, useCallback, useEffect, type ReactNode } from 'react';
import {
  APP_SETTINGS_STORAGE_KEY,
  normalizeAppSettings,
  readStoredAppSettings,
  type AppSettings,
} from '../config/appSettings';
import i18n from '../i18n';

interface SettingsContextValue {
  /** Current settings */
  settings: AppSettings;
  /** Updates the settings */
  updateSettings: (updates: Partial<AppSettings>) => void;
}

const SettingsContext = createContext<SettingsContextValue | null>(null);

interface SettingsProviderProps {
  children: ReactNode;
}

export function SettingsProvider({ children }: SettingsProviderProps) {
  // Read saved settings from localStorage (defaults + normalization live in config/appSettings,
  // shared with the startup elevation request that runs before React mounts)
  const [settings, setSettings] = useState<AppSettings>(readStoredAppSettings);

  useEffect(() => {
    // Settings may come from an older cache version; switch them uniformly after the Provider takes effect, to avoid reading an invalid language on first paint.
    void i18n.changeLanguage(settings.language);
  }, [settings.language]);

  // Updates the settings
  const updateSettings = useCallback((updates: Partial<AppSettings>) => {
    setSettings(prev => {
      const newSettings = normalizeAppSettings({ ...prev, ...updates });
      try {
        localStorage.setItem(APP_SETTINGS_STORAGE_KEY, JSON.stringify(newSettings));
      } catch (error) {
        // Storage can fail (quota, locked profile). Keep the in-memory value so the current
        // session still honours the change, but report it: the setting just won't survive a restart.
        console.error('保存设置失败:', error);
      }
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
