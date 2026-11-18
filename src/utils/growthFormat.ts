// ============================================================================
// Shared formatting helpers for the disk growth module
//
// Note: the flat list, the tree list and the details modal all need the same
// path shortening, diff formatting and level styling. They used to live inside
// DiskGrowthModule.tsx, which made the tree list import a 1300-line component
// just for a label. Keep them here so both views stay visually identical.
// ============================================================================

import { Minus, Sparkles, TrendingDown, TrendingUp } from 'lucide-react';
import i18n from '../i18n';
import { formatSize } from './format';
import type { DiskGrowthEntry } from '../api/commands';

/** Change level as reported by the backend */
export type GrowthLevel = DiskGrowthEntry['level'];

/** Shorten a long path to its last three segments; the tree view shows the last segment only */
export function simplifyPath(path: string): string {
  const normalized = path.replace(/\\/g, '/');
  const parts = normalized.split('/').filter(Boolean);
  if (parts.length <= 3) return normalized;
  return `.../${parts.slice(-3).join('/')}`;
}

export function formatDiff(diff: number): string {
  const sign = diff >= 0 ? '+' : '-';
  return `${sign}${formatSize(Math.abs(diff))}`;
}

export function formatModifiedTime(timestamp?: number | null, compact = false): string {
  if (!timestamp) return '-';
  const normalizedTimestamp = timestamp < 10_000_000_000 ? timestamp * 1000 : timestamp;
  const date = new Date(normalizedTimestamp);
  if (Number.isNaN(date.getTime())) return '-';
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, '0');
  const day = String(date.getDate()).padStart(2, '0');
  const hours = String(date.getHours()).padStart(2, '0');
  const minutes = String(date.getMinutes()).padStart(2, '0');
  return compact ? `${month}-${day} ${hours}:${minutes}` : `${year}-${month}-${day} ${hours}:${minutes}`;
}

export function buildPathTitle(path: string, modified?: number | null, extraLine?: string): string {
  const lines = [path];
  if (extraLine) lines.push(extraLine);
  const modifiedText = formatModifiedTime(modified);
  if (modifiedText !== '-') {
    // title supports line breaks; when the path is very long, put the real modification time on its own line so key info is not drowned out by the path.
    lines.push(i18n.t('diskGrowth.modifiedTimeTitle', { ns: 'modules', time: modifiedText }));
  }
  return lines.join('\n');
}

export function normalizeDiskPath(path: string): string {
  return path.replace(/\\/g, '/').replace(/\/+$/g, '').toLowerCase();
}

export interface GrowthLevelStyle {
  icon: typeof Minus;
  color: string;
  label: string;
}

export function getGrowthStyle(level: GrowthLevel): GrowthLevelStyle {
  switch (level) {
    case 'significant':
      return { icon: TrendingUp, color: 'text-red-500', label: i18n.t('diskGrowth.level.significant', { ns: 'modules' }) };
    case 'fast':
      return { icon: TrendingUp, color: 'text-orange-500', label: i18n.t('diskGrowth.level.fast', { ns: 'modules' }) };
    case 'minor':
      return { icon: TrendingUp, color: 'text-amber-500', label: i18n.t('diskGrowth.level.minor', { ns: 'modules' }) };
    case 'decreased':
      return { icon: TrendingDown, color: 'text-green-500', label: i18n.t('diskGrowth.level.decreased', { ns: 'modules' }) };
    case 'new':
      return { icon: Sparkles, color: 'text-blue-500', label: i18n.t('diskGrowth.level.new', { ns: 'modules' }) };
    default:
      return { icon: Minus, color: 'text-[var(--text-faint)]', label: i18n.t('diskGrowth.level.stable', { ns: 'modules' }) };
  }
}

/** Build the "what changed" sentence for nodes that have no backend explanation (lazily loaded children) */
export function describeGrowth(level: GrowthLevel, diff: number): string {
  const direction = diff > 0
    ? i18n.t('diskGrowth.increase', { ns: 'modules' })
    : i18n.t('diskGrowth.decrease', { ns: 'modules' });
  return i18n.t('diskGrowth.growthExplanation', {
    ns: 'modules',
    level: getGrowthStyle(level).label,
    direction,
    size: formatSize(Math.abs(diff)),
  });
}

/**
 * Synthesize a growth entry for a tree node that only exists as a directory detail row,
 * so the drill-down modal can be opened for it as well.
 */
export function growthEntryFromNode(node: {
  path: string;
  oldSize: number;
  size: number;
  diff: number;
  modified: number;
  level: GrowthLevel;
}): DiskGrowthEntry {
  return {
    path: node.path,
    old_size: node.oldSize,
    new_size: node.size,
    diff: node.diff,
    diff_percent: node.oldSize > 0 ? (node.diff / node.oldSize) * 100 : 100,
    modified: node.modified,
    level: node.level,
    explanation: describeGrowth(node.level, node.diff),
    suggestion: i18n.t('diskGrowth.growthSuggestion', { ns: 'modules' }),
    details: [],
  };
}
