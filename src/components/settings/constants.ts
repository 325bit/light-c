// ============================================================================
// Shared configuration for the settings page
// ============================================================================

import { BookOpen, HardDrive, Info, MessageSquare, Monitor, Moon, Settings, ShieldCheck, SlidersHorizontal, Sparkles, Sun, type LucideIcon } from 'lucide-react';
import { FONT_SIZE_CONFIGS, type FontSizeLevel, type ThemeMode } from '../../contexts';
import type { SettingsTabDefinition } from './types';

export const SETTINGS_TABS: SettingsTabDefinition[] = [
  { id: 'general', label: 'tabs.general', icon: Settings },
  { id: 'ai', label: 'tabs.ai', icon: Sparkles },
  { id: 'features', label: 'tabs.features', icon: SlidersHorizontal },
  { id: 'guide', label: 'tabs.guide', icon: BookOpen },
  { id: 'disk-info', label: 'tabs.diskInfo', icon: HardDrive },
  { id: 'security', label: 'tabs.security', icon: ShieldCheck },
  { id: 'feedback', label: 'tabs.feedback', icon: MessageSquare },
  { id: 'about', label: 'tabs.about', icon: Info },
];

export const THEME_OPTIONS: { mode: ThemeMode; label: string; icon: LucideIcon }[] = [
  { mode: 'light', label: 'theme.light', icon: Sun },
  { mode: 'dark', label: 'theme.dark', icon: Moon },
  { mode: 'system', label: 'theme.system', icon: Monitor },
];

export const FONT_SIZE_OPTIONS: { level: FontSizeLevel; label: string }[] = [
  { level: 'standard', label: 'fontSize.standard' },
  { level: 'medium', label: 'fontSize.medium' },
  { level: 'large', label: 'fontSize.large' },
  { level: 'custom', label: 'fontSize.custom' },
];

// Keep a unified export so page components read the font-size hint config from a single entry point.
export { FONT_SIZE_CONFIGS };
