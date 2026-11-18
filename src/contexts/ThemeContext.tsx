// ============================================================================
// Theme context - supports light / dark / follow system
// ============================================================================

import { createContext, useContext, useEffect, useState, useCallback, type ReactNode } from 'react';

/** Theme type */
export type ThemeMode = 'light' | 'dark' | 'system';

/** The theme actually applied */
export type AppliedTheme = 'light' | 'dark';

interface ThemeContextValue {
  /** Theme mode selected by the user */
  mode: ThemeMode;
  /** The theme actually applied */
  theme: AppliedTheme;
  /** Set the theme mode */
  setMode: (mode: ThemeMode) => void;
  /** Whether the liquid-glass look is enabled (orthogonal to light/dark mode, off by default) */
  glass: boolean;
  /** Toggle liquid glass */
  setGlass: (enabled: boolean) => void;
}

const ThemeContext = createContext<ThemeContextValue | null>(null);

const STORAGE_KEY = 'c-cleanup-theme';
/** Liquid glass is persisted separately: it and light/dark mode are two orthogonal axes, switching modes shouldn't affect this toggle */
const GLASS_STORAGE_KEY = 'c-cleanup-glass';

/** Get the system theme */
function getSystemTheme(): AppliedTheme {
  if (typeof window !== 'undefined' && window.matchMedia) {
    return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
  }
  return 'dark';
}

/** Resolve the actual theme from the mode */
function resolveTheme(mode: ThemeMode): AppliedTheme {
  if (mode === 'system') {
    return getSystemTheme();
  }
  return mode;
}

interface ThemeProviderProps {
  children: ReactNode;
}

export function ThemeProvider({ children }: ThemeProviderProps) {
  // Read the saved theme mode from localStorage
  const [mode, setModeState] = useState<ThemeMode>(() => {
    if (typeof window !== 'undefined') {
      const saved = localStorage.getItem(STORAGE_KEY);
      if (saved === 'light' || saved === 'dark' || saved === 'system') {
        return saved;
      }
    }
    return 'dark'; // default to dark theme
  });

  const [theme, setTheme] = useState<AppliedTheme>(() => resolveTheme(mode));

  // Liquid glass is on by default (since v2.16.15). It's now the product's main visual, so give users the best side by default;
  // on low-end devices backdrop-filter does drop frames, and users can turn it off in "Settings - General".
  //
  // The check is deliberately written as "not equal to disabled" rather than "equal to enabled":
  // existing users have no such key locally, so this gives them the new default after upgrading,
  // while users who actively turned it off have stored 'disabled' and it won't be re-enabled.
  const [glass, setGlassState] = useState<boolean>(
    () => localStorage.getItem(GLASS_STORAGE_KEY) !== 'disabled',
  );

  // Toggle glass and persist it, written consistently with setMode above
  const setGlass = useCallback((enabled: boolean) => {
    setGlassState(enabled);
    localStorage.setItem(GLASS_STORAGE_KEY, enabled ? 'enabled' : 'disabled');
  }, []);

  // Set the theme mode
  const setMode = useCallback((newMode: ThemeMode) => {
    setModeState(newMode);
    localStorage.setItem(STORAGE_KEY, newMode);
  }, []);

  // Listen for system theme changes
  useEffect(() => {
    const mediaQuery = window.matchMedia('(prefers-color-scheme: dark)');
    
    const handleChange = () => {
      if (mode === 'system') {
        setTheme(getSystemTheme());
      }
    };

    mediaQuery.addEventListener('change', handleChange);
    return () => mediaQuery.removeEventListener('change', handleChange);
  }, [mode]);

  // Update the actual theme when the mode changes
  useEffect(() => {
    setTheme(resolveTheme(mode));
  }, [mode]);

  // Apply the theme to document
  useEffect(() => {
    document.documentElement.setAttribute('data-theme', theme);
    document.documentElement.classList.remove('light', 'dark');
    document.documentElement.classList.add(theme);
  }, [theme]);

  // The glass toggle only switches the marker class on html; the actual colors, blur radius and background layers are all handled uniformly by the .glass rules in App.css,
  // so adding new surfaces doesn't need any JS logic.
  useEffect(() => {
    document.documentElement.classList.toggle('glass', glass);
  }, [glass]);

  return (
    <ThemeContext.Provider value={{ mode, theme, setMode, glass, setGlass }}>
      {children}
    </ThemeContext.Provider>
  );
}

/** Hook for using the theme */
export function useTheme() {
  const context = useContext(ThemeContext);
  if (!context) {
    throw new Error('useTheme must be used within a ThemeProvider');
  }
  return context;
}
