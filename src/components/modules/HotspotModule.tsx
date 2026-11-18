import { useAiModuleBridge } from '../ai/useAiModuleBridge';
import { AiAskButton } from '../ai/AiAskButton';
import { buildItemQuestion } from '../ai/aiQuestions';
// ============================================================================
// Big directory analysis module
// Deep-analyzes the AppData directory to pinpoint what's eating space
// ============================================================================

import { useState, useCallback, useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import i18n from '../../i18n';
import { createPortal } from 'react-dom';
import { Flame, FolderOpen, Clock, HardDrive, ChevronDown, ChevronRight, Search, ShieldAlert, Shield, Eye, Trash2 } from 'lucide-react';
import { ScanTimingPopover, type TimingGroup } from '../ScanTimingPopover';
import { AdminRequiredHint } from '../AdminRequiredHint';
import { ModuleScanProgress } from '../ModuleScanProgress';
import { StopScanButton } from '../ui/StopScanButton';
import { listen } from '@tauri-apps/api/event';
import { AccordionContent, ModuleCard } from '../ModuleCard';
import { ConfirmDialog } from '../ConfirmDialog';
import { EmptyState } from '../EmptyState';
import {
  defaultDriveLetter,
  DriveSelect,
  driveDisplayName,
  normalizeDriveLetter,
  useLocalDrives,
} from '../ui/DriveSelect';
import { useToast } from '../Toast';
import { useModuleDashboard, useSettings } from '../../contexts';
import { scanHotspot, cancelHotspotScan, openInFolder, cleanupDirectoryContents, type HotspotScanResult, type HotspotEntry, type HotspotScanProgress } from '../../api/commands';
import { formatSize } from '../../utils/format';
import { openSearchUrl } from '../../utils/searchEngine';
import { DrillDownModal } from './DrillDownModal';
import type { ModuleRenderProps } from './moduleProps';

// ============================================================================
// Utility functions
// ============================================================================

/**
 * Format a timestamp as YYYY-MM-DD HH:mm
 */
function formatDateTime(timestamp: number): string {
  if (!timestamp) return '-';
  // The MFT engine returns Unix seconds, regular traversal returns milliseconds; normalize here to avoid second-level timestamps being read as 1970.
  const normalizedTimestamp = timestamp < 10_000_000_000 ? timestamp * 1000 : timestamp;
  const date = new Date(normalizedTimestamp);
  if (Number.isNaN(date.getTime())) return '-';
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, '0');
  const day = String(date.getDate()).padStart(2, '0');
  const hours = String(date.getHours()).padStart(2, '0');
  const minutes = String(date.getMinutes()).padStart(2, '0');
  return `${year}-${month}-${day} ${hours}:${minutes}`;
}

/**
 * Ellipsize the middle of a long path
 * e.g.: C:\Users\xxx\AppData\Local\VeryLongFolderName -> C:\Users\...\VeryLongFolderName
 */
function middleEllipsis(path: string, maxLength: number = 45): string {
  if (path.length <= maxLength) return path;
  
  const parts = path.split('\\');
  if (parts.length <= 3) {
    // Path is too short, just truncate
    return path.slice(0, maxLength - 3) + '...';
  }
  
  // Keep the first two parts and the last part
  const start = parts.slice(0, 2).join('\\');
  const end = parts[parts.length - 1];
  
  // If the tail is too long, truncate it too
  const availableForEnd = maxLength - start.length - 5; // 5 = "\\...\\".length
  const truncatedEnd = end.length > availableForEnd 
    ? end.slice(0, availableForEnd - 3) + '...'
    : end;
  
  return `${start}\\...\\${truncatedEnd}`;
}

function formatDuration(ms?: number): string {
  if (!ms || ms <= 0) return '0.0s';
  return `${(ms / 1000).toFixed(1)}s`;
}

function getProgressStageLabel(stage?: string): string {
  // Pass an empty drive letter as a fallback: if the stage text contains a {{drive}} placeholder, a missing param becomes an empty string,
  // instead of showing "{{drive}}" verbatim. Hotspot scan progress events carry no drive letter, so the placeholder is unused in practice.
  return i18n.t(`scanStages.${stage || 'scanning'}`, {
    ns: 'common',
    drive: '',
    defaultValue: i18n.t('scanStages.scanning', { ns: 'common' }),
  });
}

/**
 * Get the display color for a parent directory type
 */
function getParentTypeColor(type: string): string {
  switch (type) {
    case 'Local':
      return 'status-chip text-blue-500';
    case 'Roaming':
      return 'status-chip text-purple-500';
    case 'LocalLow':
      return 'status-chip text-orange-500';
    case 'Windows':
      return 'status-chip text-red-500';
    case 'Program Files':
    case 'Program Files (x86)':
      return 'status-chip text-amber-500';
    case 'Users':
      return 'status-chip text-cyan-500';
    case 'System':
      return 'status-chip text-rose-500';
    default:
      return 'status-chip text-gray-500';
  }
}

/**
 * Find an entry by path in hotspot results (recurses the whole tree, including subdirectories).
 *
 * Note: read_items lists only top-level entries, but the model often follows up on subdirectory paths from a previous detail,
 * and that subdirectory data is already in memory (the backend does smart drill-down for huge dirs), so we must search recursively.
 * The match criterion is the one with the **longest path prefix**: this also covers exact equality and tolerates the model passing trailing size
 * and file-count text along as part of the path; taking the longest avoids mistaking a higher-level ancestor for the match.
 */
function findHotspotEntry(entries: HotspotEntry[], wanted: string): HotspotEntry | null {
  let best: HotspotEntry | null = null;
  for (const entry of entries) {
    if (wanted.startsWith(entry.path) && (!best || entry.path.length > best.path.length)) {
      best = entry;
    }
    const matched = findHotspotEntry(entry.children ?? [], wanted);
    if (matched && (!best || matched.path.length > best.path.length)) best = matched;
  }
  return best;
}

/**
 * Render a hotspot directory as detail text for the model.
 *
 * Note: only emit fields already in memory — these numbers are shown in the UI anyway, so the model reading them can't disagree with what the user
 * sees. List every subdirectory without truncation: the backend only smart-drills huge dirs and gives a few per level anyway; truncating further
 * would cut the answer to the user's most common question, "which subdirectory takes the most space".
 */
function buildHotspotEntryDetail(entry: HotspotEntry): string {
  const flags = [
    entry.is_cache ? '缓存目录' : '',
    entry.is_program ? '程序目录' : '',
    entry.is_protected ? '系统保护目录' : '',
  ].filter(Boolean);

  const lines = [
    `路径：${entry.path}`,
    `大小：${formatSize(entry.total_size)}（${entry.file_count} 个文件）`,
    `最后修改：${formatDateTime(entry.last_modified)}`,
    `所在位置类型：${entry.parent_type}`,
    `属性：${flags.length > 0 ? flags.join('、') : '无特殊属性'}`,
    `是否可安全清理：${entry.is_safe_to_clean ? '是' : '否'}`,
  ];

  const children = [...(entry.children ?? [])].sort((left, right) => right.total_size - left.total_size);
  if (children.length > 0) {
    lines.push(
      '',
      '其中占用最大的子目录：',
      ...children.map(
        (child) => `- ${child.path}  ${formatSize(child.total_size)}（${child.file_count} 个文件）`,
      ),
    );
  } else {
    // ⚠️ "No subdirectories" must be **said out loud**. Otherwise the model thinks "not read yet",
    // so it repeatedly calls read_items for the same data until rounds run out with no conclusion (hit in practice).
    lines.push(
      '',
      '子目录：无（这一层已经到底了）。',
      '说明：后端只对超大目录做智能下钻；要看到这个目录里**具体是哪些文件**，需要在界面上点开它。',
    );
  }

  return lines.join('\n');
}

// ============================================================================
// Big directory analysis entry component
// ============================================================================

interface HotspotItemProps {
  entry: HotspotEntry;
  rank: number;
  maxSize: number;
  isFullScan: boolean; // Whether this is deep scan mode
  onOpenFolder: (path: string) => void;
  onCleanup: (entry: HotspotEntry) => void;
  onSearch: (path: string) => void;
  /** Parent directory name (for abbreviated path display) */
  parentName?: string;
  /** Whether this is a subdirectory (drill-down result) */
  isChild?: boolean;
  /** Absolute depth in the tree (0 = top level) */
  treeDepth?: number;
  /** Drill-down callback: click to enter drilldown mode */
  onDrillDown?: (path: string) => void;
}

function HotspotItem({ entry, rank, maxSize, isFullScan, onOpenFolder, onCleanup, onSearch, parentName, isChild, treeDepth = 0, onDrillDown }: HotspotItemProps) {
  const { t: moduleT } = useTranslation('modules');
  const { settings } = useSettings();
  // Subdirectories are collapsed by default: one scan can yield hundreds of rows, and expanding all by default shows a wall of hierarchy noise.
  // Expansion state is tracked per row (accordion is a transition, not "only one row open at a time"); the user chooses which branch to view.
  const [isChildrenExpanded, setIsChildrenExpanded] = useState(false);
  // Dynamically control the number of expanded tree levels from the user's display depth setting: treeDepth 0 is top level, settings.hotspotDepth caps the levels
  const maxTreeDepth = settings.hotspotDepth;
  // Whether there is a deeper level to expand: once the depth cap is reached, the drill-down modal takes over
  const hasNestedChildren = treeDepth < maxTreeDepth - 1 && Boolean(entry.children?.length);
  // Calculate the percentage bar width
  const percentage = maxSize > 0 ? (entry.total_size / maxSize) * 100 : 0;
  
  // [Safety measure] Disable the cleanup button in deep-scan mode, or when is_safe_to_clean is false
  const canCleanup = !isFullScan && entry.is_safe_to_clean && entry.is_cache && !entry.is_program && !entry.is_protected;
  
  // Build the abbreviated path: parent directory > subdirectory
  const displayName = parentName ? `${parentName} > ${entry.name}` : entry.name;
  
  // Progressive indentation for the tree: 24px per level, with the cap adjusted dynamically by the user's depth setting
  const indentStyle = treeDepth > 0
    ? { paddingLeft: `${Math.min(treeDepth, maxTreeDepth) * 24}px`, borderLeft: '2px solid var(--border-color)' }
    : {};

  const toggleChildren = () => setIsChildrenExpanded((current) => !current);

  /**
   * Determine whether a click landed on a button inside the row.
   * Drill-down/cleanup/search/open on the right each have their own behavior; a whole-row click must not swallow them,
   * otherwise you get mistakes like "clicked cleanup but only expanded the list"; buttons in child rows bubbling to the parent must also pass through.
   * Match only button/a/form controls, not [role="button"]: the row itself carries role="button" for keyboard accessibility,
   * and counting it would make whole-row clicks be misjudged as "clicked on a button" and fail.
   */
  const isInteractiveTarget = (target: EventTarget | null) => (
    target instanceof Element
    && Boolean(target.closest('button, a, input, select, textarea'))
  );

  /** Whole-row click to expand/collapse: only affects rows with children; childless rows stay purely informational, with no misleading clickable look. */
  const handleRowClick = (event: React.MouseEvent<HTMLDivElement>) => {
    if (!hasNestedChildren || isInteractiveTarget(event.target)) return;
    // Don't collapse while the user is selecting path text, to avoid the list suddenly folding while copying a path.
    // Order matters: let buttons pass first, otherwise "select text then click a button" won't click the button due to the selection state.
    if (window.getSelection()?.toString()) return;
    toggleChildren();
  };

  /** Keyboard accessible: Enter / Space is equivalent to clicking the whole row */
  const handleRowKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (!hasNestedChildren || event.target !== event.currentTarget) return;
    if (event.key !== 'Enter' && event.key !== ' ') return;
    event.preventDefault();
    toggleChildren();
  };

  return (
    <div style={indentStyle}>
      <div
        onClick={handleRowClick}
        onKeyDown={handleRowKeyDown}
        role={hasNestedChildren ? 'button' : undefined}
        tabIndex={hasNestedChildren ? 0 : undefined}
        aria-expanded={hasNestedChildren ? isChildrenExpanded : undefined}
        title={hasNestedChildren ? moduleT(isChildrenExpanded ? 'hotspot.collapseChildren' : 'hotspot.expandChildren') : undefined}
        className={`group relative bg-[var(--bg-main)] rounded-xl p-3 hover:bg-[var(--bg-hover)] transition-colors ${
          hasNestedChildren ? 'cursor-pointer' : ''
        } ${
        entry.is_protected ? 'border border-[var(--hotspot-danger-border)]' : ''
      } ${isChild ? 'bg-opacity-50' : ''}`}>
      {/* Percentage background bar. Dangerous dirs use `--hotspot-danger-fill` (theme variable, one for light and one for dark) —
          do not use `bg-red-100 dark:…`: this project doesn't enable the dark variant, so in dark mode it picks the light red ✗ */}
      <div 
        className={`absolute inset-0 rounded-xl opacity-50 transition-all ${
          entry.is_protected ? 'bg-[var(--hotspot-danger-fill)]' : 'bg-[var(--brand-green-10)]'
        }`}
        style={{ width: `${percentage}%` }}
      />
      
      <div className="relative flex items-center gap-3">
        {/* Expand/collapse indicator: the whole row is clickable; this is only the visual cue for "has children / expanded".
            No click handler here, to avoid stacking with the whole-row click and toggling twice on one click. */}
        {hasNestedChildren && (
          <span
            className="flex-shrink-0 flex items-center gap-1 px-1.5 py-1 rounded-lg text-[var(--text-muted)]"
            aria-hidden="true"
          >
            <ChevronRight className={`w-3.5 h-3.5 transition-transform duration-300 ${isChildrenExpanded ? 'rotate-90' : ''}`} />
            {!isChildrenExpanded && (
              <span className="text-[0.71rem] tabular-nums">{entry.children.length}</span>
            )}
          </span>
        )}

        {/* Folder badge: use a folder as the main visual and the rank as a corner badge, avoiding a plain rank block competing with the tree lines. */}
        <div className="flex-shrink-0 relative w-10 h-9 flex items-center justify-center">
          <div className={`relative w-9 h-7 rounded-md shadow-sm ${
            entry.is_protected
              ? 'bg-red-400'
              : rank <= 3
                ? 'bg-amber-400'
                : 'bg-amber-300'
          }`}>
            <div className={`absolute -top-1 left-1.5 h-2.5 w-4 rounded-t-md ${
              entry.is_protected
                ? 'bg-red-300'
                : rank <= 3
                  ? 'bg-amber-300'
                  : 'bg-amber-200'
            }`} />
            <div className="absolute inset-x-0 bottom-0 h-5 rounded-md bg-gradient-to-b from-yellow-300 to-amber-500" />
          </div>
          <span className={`absolute -right-0.5 -bottom-0.5 min-w-5 h-5 px-1 rounded-full flex items-center justify-center text-[0.71rem] font-bold shadow-sm ${
            entry.is_protected ? 'bg-red-500 text-white' : 'bg-[var(--brand-green)] text-white'
          }`}>
            {rank}
          </span>
        </div>
        
        {/* Folder info */}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 flex-wrap">
            <span className="font-medium text-sm text-[var(--text-primary)] truncate">
              {isChild ? displayName : entry.name}
            </span>
            {/* Drill-down depth indicator */}
            {entry.depth > 0 && (
              <span className="flex-shrink-0 text-[0.71rem] px-1.5 py-0.5 rounded status-chip text-purple-500">
                L{entry.depth}
              </span>
            )}
            <span className={`flex-shrink-0 text-[0.71rem] px-1.5 py-0.5 rounded ${getParentTypeColor(entry.parent_type)}`}>
              {entry.parent_type}
            </span>
            {/* System-protected directory tag - shown during deep scan */}
            {entry.is_protected && (
              <span className="flex-shrink-0 flex items-center gap-0.5 text-[0.71rem] px-1.5 py-0.5 rounded status-chip text-red-500">
                <Shield className="w-3 h-3" />
                {moduleT('hotspot.systemProtected')}
              </span>
            )}
            {/* Program directory tag - red warning, deletion forbidden */}
            {entry.is_program && !entry.is_protected && (
              <span className="flex-shrink-0 flex items-center gap-0.5 text-[0.71rem] px-1.5 py-0.5 rounded status-chip text-red-500">
                <ShieldAlert className="w-3 h-3" />
                {moduleT('hotspot.systemProgram')}
              </span>
            )}
            {/* Cache directory tag - recommended for cleanup (only shown in non-deep-scan mode) */}
            {entry.is_cache && !entry.is_program && !entry.is_protected && !isFullScan && (
              <span className="flex-shrink-0 flex items-center gap-0.5 text-[0.71rem] px-1.5 py-0.5 rounded status-chip text-orange-500">
                <Trash2 className="w-3 h-3" />
                {moduleT('hotspot.temporaryCache')}
              </span>
            )}
            {/* Deep-scan read-only hint */}
            {isFullScan && !entry.is_protected && (
              <span className="flex-shrink-0 flex items-center gap-0.5 text-[0.71rem] px-1.5 py-0.5 rounded status-chip text-blue-500">
                <Eye className="w-3 h-3" />
                {moduleT('hotspot.viewOnly')}
              </span>
            )}
          </div>
          <div 
            className="text-xs text-[var(--text-muted)] mt-0.5 truncate"
            title={entry.path}
          >
            {middleEllipsis(entry.path)}
          </div>
        </div>
        
        {/* Statistics */}
        <div className="flex-shrink-0 flex items-center gap-4 text-xs">
          {/* File count */}
          <div className="hidden sm:flex items-center gap-1 text-[var(--text-muted)]">
            <HardDrive className="w-3 h-3" />
            <span>{moduleT('hotspot.fileCount', { count: entry.file_count.toLocaleString() })}</span>
          </div>
          
          {/* Last modified time */}
          <div className="hidden md:flex items-center gap-1 text-[var(--text-muted)]">
            <Clock className="w-3 h-3" />
            <span>{formatDateTime(entry.last_modified)}</span>
          </div>
          
          {/* Size */}
          <div className={`font-semibold min-w-[5.0rem] text-right ${
            entry.is_protected ? 'text-red-500' : 'text-[var(--brand-green)]'
          }`}>
            {formatSize(entry.total_size)}
          </div>
          
          {/* Action button group */}
          <div className="flex items-center gap-1">
            {/* Drill-down button — always shown; after 3 children are displayed the user may still want to explore further */}
            {onDrillDown && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  onDrillDown(entry.path);
                }}
                className="p-1.5 rounded-lg hover:bg-[var(--brand-green-10)] text-[var(--brand-green)] transition-all"
                title={moduleT('drillDown.expand')}
              >
                <ChevronRight className="w-4 h-4" />
              </button>
            )}

            {/* Cleanup button - only shown in non-deep-scan mode and when cleanable */}
            {canCleanup && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  onCleanup(entry);
                }}
                className="p-1.5 rounded-lg hover:bg-[var(--hotspot-danger-fill)] text-[var(--color-warning)] transition-all"
                title={moduleT('drillDown.cleanup')}
              >
                <Trash2 className="w-4 h-4" />
              </button>
            )}
            
            {/* Search button - check whether this folder can be deleted; use .path for the full path, .name for the folder name */}
            <button
              onClick={(e) => {
                e.stopPropagation();
                onSearch(entry.path);
              }}
              className="p-1.5 rounded-lg hover:bg-[var(--bg-card)] text-[var(--text-muted)] hover:text-blue-500 transition-all"
              title={moduleT('drillDown.search')}
            >
              <Search className="w-4 h-4" />
            </button>
            {/* Quick question: at the same level as the search button in big directory analysis, behavior kept consistent */}
            <AiAskButton
              question={buildItemQuestion(entry.path, 'folder')}
              className="inline-flex h-7 w-7 items-center justify-center"
            />
            
            {/* Open folder button */}
            <button
              onClick={(e) => {
                e.stopPropagation();
                onOpenFolder(entry.path);
              }}
              className="p-1.5 rounded-lg hover:bg-[var(--bg-card)] text-[var(--text-muted)] hover:text-[var(--text-primary)] transition-all"
              title={moduleT('drillDown.open')}
            >
              <FolderOpen className="w-4 h-4" />
            </button>
          </div>
        </div>
      </div>
      </div>
      
      {/* Recursively render subdirectories — collapsed by default, with an accordion transition when expanded; up to maxTreeDepth levels of tree */}
      {hasNestedChildren && (
        // Put indentation and the tree line outside the animation container: AccordionContent needs overflow:hidden,
        // so a border and left padding inside would be clipped.
        <div
          className="mt-1"
          style={{ paddingLeft: `${Math.min(treeDepth + 1, maxTreeDepth) * 24}px`, borderLeft: '2px solid var(--border-color)' }}
        >
          <AccordionContent expanded={isChildrenExpanded}>
            <div className="space-y-1 pt-1">
              {entry.children.map((child, idx) => (
                <HotspotItem
                  key={child.path}
                  entry={child}
                  rank={idx + 1}
                  maxSize={entry.total_size}
                  isFullScan={isFullScan}
                  onOpenFolder={onOpenFolder}
                  onCleanup={onCleanup}
                  onSearch={onSearch}
                  parentName={entry.name}
                  isChild={true}
                  treeDepth={treeDepth + 1}
                  onDrillDown={onDrillDown}
                />
              ))}
            </div>
          </AccordionContent>
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Main component
// ============================================================================

export function HotspotModule({ isPageActive = true }: ModuleRenderProps) {
  const { t: navT } = useTranslation('nav');
  const { t } = useTranslation('common');
  const { t: moduleT } = useTranslation('modules');
  const { moduleState, updateModuleState, oneClickScanTrigger, stopScanTrigger } = useModuleDashboard('hotspot');
  const { showToast } = useToast();
  const { settings } = useSettings();
  const { drives } = useLocalDrives();

  const lastScanTriggerRef = useRef(0);
  const scanningRef = useRef(false);
  const cancelRequestedRef = useRef(false);
  const scanRunIdRef = useRef(0);

  // Local state
  const [scanResult, setScanResult] = useState<HotspotScanResult | null>(null);
  // Hook up the AI assistant (**read-only**: big directory analysis only analyzes, no delete action).
  useAiModuleBridge('hotspot', {
    // Read-only read: lets the assistant "look deeper" at what entries exist (one page at a time, model decides whether to continue).
    readItems: (targetId, offset, limit) => {
      const all = scanResult?.entries ?? [];
      // ⚠️ **Must honor targetId**: the user is asking "what's in this folder",
      // replying with only the top-level list is no answer; the model will repeatedly request the same thing until rounds run out
      // with no conclusion (hit in practice). When targetId is given, read that entry's **subdirectories**.
      const wanted = targetId.trim();
      const list = wanted ? (findHotspotEntry(all, wanted)?.children ?? []) : all;
      if (list.length === 0) {
        if (!wanted) return null;
        // Say clearly "there is no deeper level here", otherwise the model will keep asking
        return {
          total: 0,
          lines: [
            `（「${wanted}」下没有可继续下钻的子目录）`,
            '后端只对超大目录做智能下钻；要看这个目录里具体是哪些文件，需要在界面上点开它。',
          ],
        };
      }
      return {
        total: list.length,
        lines: list.slice(offset, offset + limit).map(
          (item) => `${item.path}  ${formatSize(item.total_size)}  （${item.file_count} 个文件）`,
        ),
      };
    },
    // Read-only: single-item detail. itemId is the full path at the start of a read_items line —
    // return null when not found, letting the common layer say "no content read" uniformly instead of inventing a reason here.
    readItemDetail: (itemId) => {
      const wanted = itemId.trim();
      if (!wanted) return null;
      const entry = findHotspotEntry(scanResult?.entries ?? [], wanted);
      return entry ? buildHotspotEntryDetail(entry) : null;
    },
    summary: () => {
      if (!scanResult) return null;
      return [
        `已递归扫描 ${scanResult.total_folders_scanned} 个目录，累计 ${formatSize(scanResult.scanned_total_size)}。`,
        scanResult.is_full_scan ? '这是全盘扫描结果。' : '当前是限量扫描，只覆盖了部分目录。',
      ].join('');
    },
  });

  const [error, setError] = useState<string | null>(null);
  const [showAll, setShowAll] = useState(false);
  // Deep-scan toggle state
  const [fullScanEnabled, setFullScanEnabled] = useState(false);
  const [selectedDriveLetter, setSelectedDriveLetter] = useState('C:');
  // Cleanup confirmation dialog state
  const [cleanupTarget, setCleanupTarget] = useState<HotspotEntry | null>(null);
  const [isCleaning, setIsCleaning] = useState(false);

  // ====== Scan progress state (only valid during deep scan) ======
  const [progressLogs, setProgressLogs] = useState<HotspotScanProgress[]>([]);

  // ====== Drill-down modal state ======
  /** Selected path: opens DrillDownModal when non-empty */
  const [selectedPath, setSelectedPath] = useState<string | null>(null);

  // Whether expanded
  const selectedDrive = drives.find((drive) => drive.drive_letter === selectedDriveLetter) ?? null;
  const selectedDriveLabel = driveDisplayName(selectedDriveLetter);

  useEffect(() => {
    if (drives.length > 0) {
      setSelectedDriveLetter((current) => {
        const normalized = normalizeDriveLetter(current);
        return drives.some((drive) => drive.drive_letter === normalized)
          ? normalized
          : defaultDriveLetter(drives);
      });
    }
  }, [drives]);

  /** Click the drill-down button → open the modal */
  const handleDrillDown = useCallback((targetPath: string) => {
    setSelectedPath(targetPath);
  }, []);

  /** Sync callback after cleanup happens inside the modal → rescan the main list */
  const handleModalCleanupDone = useCallback(() => {
    // Delay the rescan slightly to avoid conflicting with the modal's close animation
    setTimeout(() => {
      handleScanRef.current?.();
    }, 100);
  }, []);

  // Use a ref to break the circular dependency handleScan ↔ handleModalCleanupDone
  const handleScanRef = useRef<(() => void) | null>(null);

  // ====== Listen for scan progress events ======
  useEffect(() => {
    let unlistenProgress: (() => void) | null = null;
    let unlistenCancelled: (() => void) | null = null;

    const setupListeners = async () => {
      unlistenProgress = await listen<HotspotScanProgress>('hotspot-scan:progress', (event) => {
        const progress = event.payload;
        setProgressLogs((prev) => {
          const last = prev[prev.length - 1];
          // Keep only the latest entry for the same stage, to avoid MFT flooding every 10k items and making frontend logs noisy.
          if (last?.stage === progress.stage) {
            return [...prev.slice(0, -1), progress].slice(-6);
          }
          return [...prev, progress].slice(-6);
        });
      });
      unlistenCancelled = await listen('hotspot-scan:cancelled', () => {
        // Scan cancelled; UI is handled by handleScan's catch/finally
      });
    };

    setupListeners();

    return () => {
      if (unlistenProgress) unlistenProgress();
      if (unlistenCancelled) unlistenCancelled();
    };
  }, []);

  // Run the scan
  const handleScan = useCallback(async () => {
    if (scanningRef.current) return;
    if (fullScanEnabled && selectedDrive && !selectedDrive.is_ntfs) {
      const message = moduleT('hotspotExtra.ntfsError', { drive: selectedDriveLabel, fileSystem: selectedDrive.file_system || moduleT('hotspotExtra.unknownFileSystem') });
      setError(message);
      updateModuleState('hotspot', { status: 'error', error: message });
      return;
    }
    scanningRef.current = true;
    cancelRequestedRef.current = false;
    const scanRunId = ++scanRunIdRef.current;

    updateModuleState('hotspot', { status: 'scanning' });
    setError(null);
    setScanResult(null);
    setShowAll(false);
    setSelectedPath(null);
    setProgressLogs([]);

    try {
      // Choose scan mode based on the deep-scan toggle (full-disk scan yields more entries)
      const topN = fullScanEnabled ? 80 : 50;
      const result = await scanHotspot(
        topN,
        fullScanEnabled,
        settings.hotspotDepth,
        settings.hotspotSizeThreshold,
        settings.hotspotIgnoreSystemDirs,
        selectedDriveLetter,
      );
      if (cancelRequestedRef.current || scanRunId !== scanRunIdRef.current) {
        // When the user actively stops, don't accept a partial result the backend may return, avoiding a list with only one level of dirs causing misjudgment.
        return;
      }
      setScanResult(result);

      // Card summary: number of directories shown + total scan coverage (consistent with the internal stats row)
      updateModuleState('hotspot', {
        status: 'done',
        fileCount: result.entries.length,
        totalSize: result.scanned_total_size,
      });
    } catch (err) {
      if (cancelRequestedRef.current || scanRunId !== scanRunIdRef.current) {
        updateModuleState('hotspot', { status: 'idle', progress: 0 });
        return;
      }
      console.error('大目录分析扫描失败:', err);
      setError(String(err));
      updateModuleState('hotspot', { status: 'error' });
    } finally {
      if (scanRunId === scanRunIdRef.current) {
        scanningRef.current = false;
      }
    }
  }, [updateModuleState, fullScanEnabled, settings, selectedDriveLetter, selectedDrive, selectedDriveLabel, moduleT]);

  const resetHotspotResult = useCallback(() => {
    setScanResult(null);
    setError(null);
    setShowAll(false);
    setSelectedPath(null);
    setProgressLogs([]);
    updateModuleState('hotspot', { status: 'idle', error: null, fileCount: 0, totalSize: 0, progress: 0 });
  }, [updateModuleState]);

  const handleDriveChange = useCallback((driveLetter: string) => {
    if (scanningRef.current) return;
    setSelectedDriveLetter(normalizeDriveLetter(driveLetter));
    resetHotspotResult();
  }, [resetHotspotResult]);

  // Cancel the scan
  const handleStopScan = useCallback(async () => {
    cancelRequestedRef.current = true;
    scanRunIdRef.current += 1;
    scanningRef.current = false;
    updateModuleState('hotspot', { status: 'idle', progress: 0 });
    try {
      await cancelHotspotScan();
      showToast({ type: 'info', title: moduleT('hotspot.scanStopped'), description: moduleT('hotspot.scanStoppedDesc') });
    } catch (err) {
      console.error('停止扫描失败:', err);
    }
  }, [showToast, updateModuleState]);

  // Sync handleScanRef for the modal's cleanup callback
  handleScanRef.current = handleScan;

  // Respond to one-click scan
  useEffect(() => {
    if (oneClickScanTrigger > 0 && oneClickScanTrigger !== lastScanTriggerRef.current) {
      lastScanTriggerRef.current = oneClickScanTrigger;
      handleScan();
    }
  }, [oneClickScanTrigger, handleScan]);

  useEffect(() => {
    if (stopScanTrigger > 0 && scanningRef.current) {
      // The global stop button has already told the backend to cancel; here we only prevent an old Promise from writing back a partial result.
      cancelRequestedRef.current = true;
      scanRunIdRef.current += 1;
      scanningRef.current = false;
    }
  }, [stopScanTrigger]);

  // Open folder
  const handleOpenFolder = useCallback(async (path: string) => {
    try {
      await openInFolder(path);
    } catch (err) {
      console.error('打开文件夹失败:', err);
    }
  }, []);

  // Trigger the cleanup confirmation dialog
  const handleCleanupClick = useCallback((entry: HotspotEntry) => {
    setCleanupTarget(entry);
  }, []);

  // Run the cleanup operation
  const handleCleanupConfirm = useCallback(async () => {
    if (!cleanupTarget) return;
    
    setIsCleaning(true);
    try {
      const result = await cleanupDirectoryContents(cleanupTarget.path);
      
      if (result.deleted_count > 0) {
        showToast({
          type: 'success',
          title: moduleT('hotspot.cleanupDone'),
          description: moduleT('hotspot.cleanupDoneDesc', { count: result.deleted_count, size: formatSize(result.freed_size) }),
        });
        // Rescan after cleanup to refresh the data
        handleScan();
      } else if (result.failed_count > 0) {
        showToast({
          type: 'warning',
          title: moduleT('hotspot.cleanupBlocked'),
          description: moduleT('hotspot.cleanupBlockedDesc', { count: result.failed_count }),
        });
      } else {
        showToast({
          type: 'info',
          title: moduleT('hotspot.empty'),
          description: moduleT('hotspot.emptyDesc'),
        });
      }
    } catch (err) {
      console.error('清理失败:', err);
      showToast({
        type: 'error',
        title: moduleT('hotspot.cleanupFailed'),
        description: String(err),
      });
    } finally {
      setIsCleaning(false);
      setCleanupTarget(null);
    }
  }, [cleanupTarget, handleScan, showToast, moduleT]);

  // Search whether a folder can be deleted - uses the Tauri opener plugin to open a browser
  const handleSearch = useCallback(async (path: string) => {
    try {
      await openSearchUrl(`${moduleT('hotspotExtra.searchPrefix')} ${path} ${moduleT('hotspotExtra.searchSuffix')}`);
    } catch (err) {
      console.error('打开搜索链接失败:', err);
    }
  }, [moduleT]);

  // Entries to display (10 by default, all when expanded)
  const displayedEntries = showAll 
    ? scanResult?.entries || []
    : (scanResult?.entries || []).slice(0, 10);

  // Max size (used to calculate the percentage bar)
  const maxSize = scanResult?.entries[0]?.total_size || 0;
  const driveSelector = (
    <div className="flex items-center gap-2 shrink-0" onClick={(event) => event.stopPropagation()}>
      <DriveSelect
        value={selectedDriveLetter}
        drives={drives}
        onChange={handleDriveChange}
        disabled={moduleState.status === 'scanning'}
      />
    </div>
  );

  // The result page used to show a whole set of stage-timing cards; after release they moved into a popover: numbers kept, space returned to the result list
  const stageRows = progressLogs
    .filter((log) => log.stage && log.stage !== 'result')
    .map((log) => ({
      label: getProgressStageLabel(log.stage),
      value: formatDuration(log.stage_elapsed_ms),
    }));
  const detailNotes = progressLogs
    .map((log) => log.detail)
    .filter((detail): detail is string => Boolean(detail));
  const timingGroups: TimingGroup[] = stageRows.length > 0
    ? [{ title: '', rows: stageRows, note: detailNotes.join(' · ') }]
    : [];

  if (!isPageActive && !cleanupTarget && !selectedPath) {
    return null;
  }

  return (
    <ModuleCard
      id="hotspot"
        title={navT('hotspot')}
        description={`${navT('hotspotDesc')} (${selectedDriveLabel})`}
      icon={<Flame className="w-5 h-5 text-[var(--brand-green)]" />}
      status={moduleState.status}
      fileCount={moduleState.fileCount}
      totalSize={moduleState.totalSize}
      countLabel={moduleT('hotspot.folders')}
      onScan={handleScan}
      scanButtonText={moduleState.status === 'scanning' ? t('scanningShort') : t('startScan')}
      scanDisabled={Boolean(fullScanEnabled && selectedDrive && !selectedDrive.is_ntfs)}
      error={error}
      titleExtra={fullScanEnabled ? driveSelector : null}
      headerExtra={
        // Deep-scan toggle + stop button. The stop button sits on the same row as the "scanning…" button:
        // when a user wants to interrupt, their eyes are at the top-right; they shouldn't have to look in the body.
        <div className="flex items-center gap-2">
          <button
            onClick={() => setFullScanEnabled(!fullScanEnabled)}
            className={`flex items-center gap-1.5 px-2.5 py-1 rounded-lg text-xs font-medium transition-all ${
              fullScanEnabled
                ? 'bg-[var(--brand-green)] text-white'
                : 'bg-[var(--bg-main)] text-[var(--text-muted)] hover:text-[var(--text-primary)] border border-[var(--border-color)]'
            }`}
            title={fullScanEnabled ? moduleT('hotspot.fullScanOn') : moduleT('hotspot.appDataOnly')}
          >
            <Eye className="w-3.5 h-3.5" />
            {moduleT('hotspot.deepScan')}
          </button>
          {moduleState.status === 'scanning' && fullScanEnabled && (
            <StopScanButton onClick={handleStopScan} />
          )}
        </div>
      }
    >
      {/* Without admin rights, full-disk analysis falls back to per-directory traversal (tens of seconds vs seconds).
          Phrase it as "can be accelerated", not "requires permission" — it works without elevation, and saying required would make users think the feature is broken.
          Left/right margins align with the p-5 of this module's other status blocks. */}
      <AdminRequiredHint className="mx-5 mt-4" variant="performance" />

      {/* Scanning state: use the common component. Engine mode (MFT / regular traversal) and the per-stage diagnostics panel are implementation details —
          ScanTimingPopover covers elapsed-time investigation when there's a result; during scanning, keep only "what's being scanned"
          and the hints the user needs. */}
      {moduleState.status === 'scanning' && (
        <div className="p-4">
          <ModuleScanProgress
            title={
              fullScanEnabled
                ? moduleT('hotspot.deepScanning', { drive: selectedDriveLabel })
                : moduleT('hotspot.scanningAppData')
            }
            description={
              fullScanEnabled ? moduleT('hotspot.deepScanWait') : moduleT('hotspot.quickScanWait')
            }
            footer={
              fullScanEnabled &&
              (!settings.hotspotIgnoreSystemDirs || (selectedDrive && !selectedDrive.is_ntfs)) ? (
                <div className="space-y-1 text-xs text-[var(--color-warning)]">
                  {!settings.hotspotIgnoreSystemDirs && (
                    <p>⚠ {moduleT('hotspot.systemFilterOff')}</p>
                  )}
                  {selectedDrive && !selectedDrive.is_ntfs && (
                    <p>{moduleT('hotspot.notNtfs')}</p>
                  )}
                </div>
              ) : undefined
            }
          />
        </div>
      )}

      {/* Scan results */}
      {moduleState.status === 'done' && scanResult && (
        <div className="space-y-3">
          {/* Deep-scan safety notice. Background/text go through theme variables —
              previously written as `bg-blue-50 dark:bg-blue-900/20`, but the dark variant doesn't follow the app theme,
              giving a bright blue banner in dark mode ✗ */}
          {scanResult.is_full_scan && (
            <div className="flex items-center gap-2 px-3 py-2 bg-[var(--notice-info-fill)] rounded-lg text-xs text-[var(--notice-info-text)]">
              <Eye className="w-4 h-4 flex-shrink-0" />
              <span>{moduleT('hotspot.deepScanReadOnly')}</span>
            </div>
          )}

          {/* Stats summary */}
          <div className="flex items-center justify-between px-1 text-xs text-[var(--text-muted)]">
            <div className="flex items-center gap-4 mt-4">
              <span>{moduleT('hotspot.folderCount', { count: scanResult.total_folders_scanned.toLocaleString() })}</span>
              <span title={moduleT('hotspot.coverageTitle')}>
                {moduleT('hotspot.coverage')}{' '}
                <strong className="text-[var(--brand-green)]">{formatSize(scanResult.scanned_total_size)}</strong>
              </span>
            </div>
            <div className="flex items-center gap-1.5">
              <span>{moduleT('hotspot.durationValue', { time: (scanResult.scan_duration_ms / 1000).toFixed(1) })}</span>
              <ScanTimingPopover groups={timingGroups} />
            </div>
          </div>

          {/* Directory list: left/right padding so row cards don't stick to the module border */}
          <div className="space-y-2 px-2">
            {displayedEntries.map((entry, index) => (
              <HotspotItem
                key={entry.path}
                entry={entry}
                rank={index + 1}
                maxSize={maxSize}
                isFullScan={scanResult.is_full_scan}
                onOpenFolder={handleOpenFolder}
                onCleanup={handleCleanupClick}
                onSearch={handleSearch}
                treeDepth={0}
                onDrillDown={handleDrillDown}
              />
            ))}
          </div>

          {/* Expand/collapse button */}
          {scanResult.entries.length > 10 && (
            <button
              onClick={() => setShowAll(!showAll)}
              className="w-full flex items-center justify-center gap-1 py-2 text-xs text-[var(--text-muted)] hover:text-[var(--text-primary)] transition-colors"
            >
              <span>{showAll ? moduleT('hotspot.collapse') : moduleT('hotspot.showAll', { count: scanResult.entries.length })}</span>
              <ChevronDown className={`w-4 h-4 transition-transform ${showAll ? 'rotate-180' : ''}`} />
            </button>
          )}

          {/* Empty state */}
          {scanResult.entries.length === 0 && (
            <div className="p-4">
              <EmptyState
                icon={Flame}
                tone="success"
                title={moduleT('hotspot.noResult')}
                description={moduleT('hotspot.noResultDesc')}
              />
            </div>
          )}
        </div>
      )}

      {/* Initial state */}
      {moduleState.status === 'idle' && !scanResult && (
        <div className="p-4">
          <EmptyState
            icon={Flame}
            title={moduleT('hotspot.idle')}
            description={moduleT('hotspot.idleDesc')}
          />
        </div>
      )}

      {/* Cleanup confirmation dialog */}
      {cleanupTarget && createPortal(
        <ConfirmDialog
          isOpen={!!cleanupTarget}
          title={moduleT('hotspot.confirmCleanup')}
          description={moduleT('hotspot.cleanupConfirm', { name: cleanupTarget.name })}
          warning={moduleT('hotspot.occupiedWarning')}
          confirmText={isCleaning ? t('deleting') : moduleT('hotspot.confirmCleanup')}
          cancelText={t('cancel')}
          onConfirm={handleCleanupConfirm}
          onCancel={() => setCleanupTarget(null)}
          isDanger={false}
        />,
        document.body
      )}

      {/* Drill-down modal - rendered via Portal to body */}
      {selectedPath && (
        <DrillDownModal
          initialPath={selectedPath}
          onClose={() => setSelectedPath(null)}
          onCleanupDone={handleModalCleanupDone}
        />
      )}
    </ModuleCard>
  );
}
