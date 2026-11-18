// ============================================================================
// Font size context - supports presets and custom sizes
// ============================================================================

import { createContext, useContext, useEffect, useState, useCallback, type ReactNode } from 'react';

/** Font size presets */
export type FontSizeLevel = 'standard' | 'medium' | 'large' | 'custom';

export const CUSTOM_FONT_SIZE_MIN = 10;
// Raise the custom font size cap to support large screens and visual accessibility, while keeping integer pixels for stable layout.
export const CUSTOM_FONT_SIZE_MAX = 36;
export const DEFAULT_CUSTOM_FONT_SIZE = 14;

/** Font size config */
interface FontSizeConfig {
  label: string;
  offset: number; // px
  baseSize: number; // px
}

/** Font size preset config */
export const FONT_SIZE_CONFIGS: Record<FontSizeLevel, FontSizeConfig> = {
  standard: { label: '标准', offset: 0, baseSize: 14 },
  medium: { label: '适中', offset: 1, baseSize: 14 },
  large: { label: '较大', offset: 2, baseSize: 14 },
  custom: { label: '自定义', offset: 0, baseSize: 14 },
};

interface FontSizeContextValue {
  /** Current font size preset */
  level: FontSizeLevel;
  /** Set the font size preset */
  setLevel: (level: FontSizeLevel) => void;
  /** Current custom font size, in px */
  customFontSize: number;
  /** Set the custom font size, internally clamped to the allowed range */
  setCustomFontSize: (size: number) => void;
}

const FontSizeContext = createContext<FontSizeContextValue | null>(null);

const STORAGE_KEY = 'c-cleanup-font-size';
const CUSTOM_SIZE_STORAGE_KEY = 'c-cleanup-custom-font-size';

interface FontSizeProviderProps {
  children: ReactNode;
}

export function FontSizeProvider({ children }: FontSizeProviderProps) {
  // Read the saved font size preset from localStorage
  const [level, setLevelState] = useState<FontSizeLevel>(() => {
    if (typeof window !== 'undefined') {
      const saved = localStorage.getItem(STORAGE_KEY);
      if (saved === 'standard' || saved === 'medium' || saved === 'large' || saved === 'custom') {
        return saved;
      }
    }
    return 'standard'; // default standard font size
  });
  const [customFontSize, setCustomFontSizeState] = useState<number>(() => {
    if (typeof window !== 'undefined') {
      const savedValue = localStorage.getItem(CUSTOM_SIZE_STORAGE_KEY);
      const savedSize = Number(savedValue);
      if (savedValue !== null && Number.isFinite(savedSize)) {
        return Math.min(CUSTOM_FONT_SIZE_MAX, Math.max(CUSTOM_FONT_SIZE_MIN, Math.round(savedSize)));
      }
    }
    return DEFAULT_CUSTOM_FONT_SIZE;
  });

  // Set the font size preset
  const setLevel = useCallback((newLevel: FontSizeLevel) => {
    setLevelState(newLevel);
    localStorage.setItem(STORAGE_KEY, newLevel);
  }, []);

  const setCustomFontSize = useCallback((nextSize: number) => {
    const normalizedSize = Math.min(CUSTOM_FONT_SIZE_MAX, Math.max(CUSTOM_FONT_SIZE_MIN, Math.round(nextSize)));
    setCustomFontSizeState(normalizedSize);
    localStorage.setItem(CUSTOM_SIZE_STORAGE_KEY, String(normalizedSize));
  }, []);

  // Apply font size to document
  useEffect(() => {
    const offset = level === 'custom'
      ? customFontSize - FONT_SIZE_CONFIGS.standard.baseSize
      : FONT_SIZE_CONFIGS[level].offset;
    // Keep the base font size at 14px and only adjust the offset, so a custom size is not applied twice.
    document.documentElement.style.setProperty('--font-size-offset', `${offset}px`);
    document.documentElement.style.setProperty('--base-font-size', `${FONT_SIZE_CONFIGS.standard.baseSize}px`);
  }, [customFontSize, level]);

  return (
    <FontSizeContext.Provider value={{ level, setLevel, customFontSize, setCustomFontSize }}>
      {children}
    </FontSizeContext.Provider>
  );
}

/** Font size hook */
export function useFontSize() {
  const context = useContext(FontSizeContext);
  if (!context) {
    throw new Error('useFontSize must be used within a FontSizeProvider');
  }
  return context;
}
