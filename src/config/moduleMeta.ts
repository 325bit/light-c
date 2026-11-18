// ============================================================================
// Feature module metadata
// Only pure config goes here, avoiding settings state depending on concrete module components and causing circular references.
// ============================================================================

import {
  Bot,
  BrainCircuit,
  Cpu,
  Database,
  FileBox,
  Flame,
  HardDrive,
  Layers,
  MessageCircle,
  MousePointerClick,
  Package,
  Trash2,
  HardDriveDownload,
} from 'lucide-react';
import type { ComponentType } from 'react';

export type AppModuleId =
  | 'junk-clean'
  | 'big-files'
  | 'social-clean'
  | 'ai-tool-cleanup'
  | 'dev-cache'
  | 'system-slim'
  | 'driver-cleanup'
  | 'leftovers'
  | 'registry'
  | 'context-menu'
  | 'hotspot'
  | 'disk-growth'
  | 'ai-models'
  | 'shell-icons';

export interface AppModuleMeta {
  /** The module's stable ID in pages and navigation, must match data-module-id. */
  id: AppModuleId;
  label: string;
  icon: ComponentType<{ className?: string }>;
}

export const APP_MODULE_META: AppModuleMeta[] = [
  { id: 'junk-clean', label: 'junkClean', icon: Trash2 },
  { id: 'big-files', label: 'bigFiles', icon: FileBox },
  { id: 'social-clean', label: 'socialClean', icon: MessageCircle },
  // AI tool cleanup and social cleanup both belong to the "cleanup" group: both aggregate by tool and allow checkable deletion.
  { id: 'ai-tool-cleanup', label: 'aiToolCleanup', icon: Bot },
  // Dev caches: toolchain caches only (multi-version leftovers and other software caches moved to the
  // "leftover & cache" page).
  // Also in the "cleanup" group with AI tool cleanup; the difference is it targets developer toolchains
  // like compilers/package managers, rather than AI clients.
  { id: 'dev-cache', label: 'devCache', icon: Package },
  { id: 'system-slim', label: 'systemSlim', icon: Layers },
  { id: 'driver-cleanup', label: 'driverCleanup', icon: Cpu },
  { id: 'leftovers', label: 'leftovers', icon: Package },
  { id: 'registry', label: 'registry', icon: Database },
  { id: 'context-menu', label: 'contextMenu', icon: MousePointerClick },
  { id: 'hotspot', label: 'hotspot', icon: Flame },
  { id: 'disk-growth', label: 'diskGrowth', icon: HardDrive },
  { id: 'shell-icons', label: 'shellIcons', icon: HardDriveDownload },
  // AI model space covers models, LoRAs, embeddings and caches; "space" emphasizes this is usage analysis, not automatic cleanup.
  { id: 'ai-models', label: 'aiModels', icon: BrainCircuit },
];

export const DEFAULT_ACTIVE_MODULE_ID: AppModuleId = 'junk-clean';
