// ============================================================================
// Shared types for the settings page
// ============================================================================

import type { LucideIcon } from 'lucide-react';

export type SettingsTab = 'general' | 'ai' | 'features' | 'disk-info' | 'guide' | 'security' | 'feedback' | 'about';

export interface SettingsTabDefinition {
  id: SettingsTab;
  label: string;
  icon: LucideIcon;
}
