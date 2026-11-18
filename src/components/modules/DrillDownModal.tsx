// ============================================================================
// Directory drill-down modal component
// Shows unlimited drill-down content for a given path in an immersive popup
// Supports breadcrumb navigation, ESC to close, Portal rendering, and cleanup sync callbacks
// ============================================================================

import { useState, useCallback, useEffect, useRef, useMemo, memo } from 'react';
import { useTranslation } from 'react-i18next';
import { createPortal } from 'react-dom';
import {
  X, Loader2, FolderOpen, Clock, HardDrive, ChevronRight,
  CornerLeftUp, Search, Shield, Trash2, ChevronDown,
} from 'lucide-react';
import { ConfirmDialog } from '../ConfirmDialog';
import { useToast } from '../Toast';
import {
  scanPathDirect, openInFolder, cleanupDirectoryContents,
  type HotspotScanResult, type HotspotEntry,
} from '../../api/commands';
import { formatSize } from '../../utils/format';
import { openSearchUrl } from '../../utils/searchEngine';
import { AiAskButton } from '../ai/AiAskButton';
import { buildItemQuestion } from '../ai/aiQuestions';

// ============================================================================
// Utility functions (logic shared with HotspotModule)
// ============================================================================

function formatDateTime(timestamp: number): string {
  if (!timestamp) return '-';
  // The big-directory MFT engine uses Unix seconds, fallback scans use milliseconds; the display layer normalizes both to one format, compatible with either source.
  const normalizedTimestamp = timestamp < 10_000_000_000 ? timestamp * 1000 : timestamp;
  const d = new Date(normalizedTimestamp);
  if (Number.isNaN(d.getTime())) return '-';
  const Y = d.getFullYear();
  const M = String(d.getMonth() + 1).padStart(2, '0');
  const D = String(d.getDate()).padStart(2, '0');
  const h = String(d.getHours()).padStart(2, '0');
  const m = String(d.getMinutes()).padStart(2, '0');
  return `${Y}-${M}-${D} ${h}:${m}`;
}

function getParentTypeColor(type: string): string {
  switch (type) {
    case 'Local':      return 'status-chip text-blue-500';
    case 'Roaming':    return 'status-chip text-purple-500';
    case 'LocalLow':   return 'status-chip text-orange-500';
    case 'Windows':    return 'status-chip text-red-500';
    case 'Program Files':
    case 'Program Files (x86)':
      return 'status-chip text-amber-500';
    case 'Users':      return 'status-chip text-cyan-500';
    case 'System':     return 'status-chip text-rose-500';
    default:           return 'status-chip text-gray-500';
  }
}

function FolderRankBadge({
  rank,
  isProtected,
}: {
  rank: number;
  isProtected: boolean;
}) {
  return (
    <div className="flex-shrink-0 relative w-10 h-9 flex items-center justify-center">
      <div className={`relative w-9 h-7 rounded-md shadow-sm ${
        isProtected
          ? 'bg-red-400'
          : rank <= 3
            ? 'bg-amber-400'
            : 'bg-amber-300'
      }`}>
        <div className={`absolute -top-1 left-1.5 h-2.5 w-4 rounded-t-md ${
          isProtected
            ? 'bg-red-300'
            : rank <= 3
              ? 'bg-amber-300'
              : 'bg-amber-200'
        }`} />
        <div className="absolute inset-x-0 bottom-0 h-5 rounded-md bg-gradient-to-b from-yellow-300 to-amber-500" />
      </div>
      <span className={`absolute -right-0.5 -bottom-0.5 min-w-5 h-5 px-1 rounded-full flex items-center justify-center text-[0.71rem] font-bold shadow-sm ${
        isProtected ? 'bg-red-500 text-white' : 'bg-[var(--brand-green)] text-white'
      }`}>
        {rank}
      </span>
    </div>
  );
}

// ============================================================================
// Breadcrumb navigation
// Truncate the middle of an overlong path, keeping the last two levels
// ============================================================================

interface BreadcrumbSegment {
  name: string;
  path: string;
}

/** Split a Windows absolute path into breadcrumb segments */
function buildBreadcrumbs(fullPath: string): BreadcrumbSegment[] {
  const parts = fullPath.replace(/\//g, '\\').split('\\').filter(Boolean);
  const segments: BreadcrumbSegment[] = [];
  let accumulated = '';
  for (const part of parts) {
    accumulated = accumulated ? `${accumulated}\\${part}` : part;
    // The first segment is a drive letter like "C:", add a trailing backslash
    const path = segments.length === 0 ? `${accumulated}\\` : accumulated;
    segments.push({ name: part, path });
  }
  return segments;
}

function ModalBreadcrumbs({
  segments,
  initialDepth,
  onNavigate,
}: {
  segments: BreadcrumbSegment[];
  /** Number of levels in the initial path; the first initialDepth levels aren't clickable */
  initialDepth: number;
  onNavigate: (path: string) => void;
}) {
  // If there are more than 5 segments, keep first 2 + "..." + last 2
  const MAX_VISIBLE = 5;
  // Record original indices to determine clickability
  type VisibleItem = { seg: BreadcrumbSegment; originalIndex: number } | null;
  let visible: VisibleItem[] = segments.map((seg, i) => ({ seg, originalIndex: i }));
  if (segments.length > MAX_VISIBLE) {
    visible = [
      { seg: segments[0], originalIndex: 0 },
      { seg: segments[1], originalIndex: 1 },
      null, // ellipsis placeholder
      { seg: segments[segments.length - 2], originalIndex: segments.length - 2 },
      { seg: segments[segments.length - 1], originalIndex: segments.length - 1 },
    ];
  }

  return (
    <div className="flex items-center gap-1 text-xs overflow-x-auto scrollbar-thin py-1 min-w-0">
      {visible.map((item, idx) => {
        if (item === null) {
          return (
            <span key="ellipsis" className="flex items-center gap-1 shrink-0 text-[var(--text-faint)]">
              <ChevronRight className="w-3 h-3" />
              <span>···</span>
            </span>
          );
        }
        const { seg, originalIndex } = item;
        const isLast = idx === visible.length - 1;
        // Parent directories of initialPath aren't clickable (already scanned in the parent module); initialPath and beyond are clickable
        // initialDepth is the number of levels in initialPath, so index < initialDepth - 1 is not clickable
        const isClickable = originalIndex >= initialDepth - 1 && !isLast;
        return (
          <span key={seg.path} className="flex items-center gap-1 shrink-0">
            {idx > 0 && <ChevronRight className="w-3 h-3 text-[var(--text-faint)]" />}
            {isLast ? (
              <span
                className="font-semibold text-[var(--text-primary)] max-w-[12.86rem] truncate"
                title={seg.path}
              >
                {seg.name}
              </span>
            ) : isClickable ? (
              <button
                onClick={() => onNavigate(seg.path)}
                className="text-[var(--text-muted)] hover:text-[var(--brand-green)] hover:underline transition-colors max-w-[8.57rem] truncate"
                title={seg.path}
              >
                {seg.name}
              </button>
            ) : (
              <span
                className="text-[var(--text-faint)] max-w-[8.57rem] truncate cursor-default"
                title={seg.path}
              >
                {seg.name}
              </span>
            )}
          </span>
        );
      })}
    </div>
  );
}

// ============================================================================
// Drill-down list entry (immersive - full width, no indentation)
// ============================================================================

interface ModalEntryItemProps {
  entry: HotspotEntry;
  rank: number;
  maxSize: number;
  onDrillDown: (path: string) => void;
  onOpenFolder: (path: string) => void;
  onCleanup: (entry: HotspotEntry) => void;
  onSearch: (path: string) => void;
}

// Use React.memo to avoid irrelevant re-renders of list items (parent state changes don't affect already-rendered entries)
const ModalEntryItem = memo(function ModalEntryItem({
  entry, rank, maxSize, onDrillDown, onOpenFolder, onCleanup, onSearch,
}: ModalEntryItemProps) {
  const { t: moduleT } = useTranslation('modules');
  const percentage = maxSize > 0 ? (entry.total_size / maxSize) * 100 : 0;
  const canCleanup = entry.is_safe_to_clean && entry.is_cache && !entry.is_program && !entry.is_protected;

  return (
    <div
      className={`group relative rounded-xl p-3 hover:bg-[var(--bg-hover)] transition-colors cursor-default ${
        entry.is_protected ? 'border border-[var(--hotspot-danger-border)]' : ''
      }`}
    >
      {/* Percentage background bar. Dangerous dirs use a theme variable (same as HotspotModule's main list) —
          do not use `bg-red-100 dark:…`: this project doesn't enable the dark variant, so in dark mode it picks the light red ✗ */}
      <div
        className={`absolute inset-0 rounded-xl opacity-40 transition-all ${
          entry.is_protected ? 'bg-[var(--hotspot-danger-fill)]' : 'bg-[var(--brand-green-10)]'
        }`}
        style={{ width: `${percentage}%` }}
      />

      <div className="relative flex items-center gap-3">
        {/* Folder badge: consistent with the big-directory main list, the rank as a corner badge carries ordering info. */}
        <FolderRankBadge rank={rank} isProtected={entry.is_protected} />

        {/* Left side: icon + name */}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 flex-wrap">
            <FolderOpen className="w-4 h-4 text-[var(--text-muted)] flex-shrink-0" />
            <span className="font-medium text-sm text-[var(--text-primary)]">{entry.name}</span>
            <span className={`flex-shrink-0 text-[0.71rem] px-1.5 py-0.5 rounded ${getParentTypeColor(entry.parent_type)}`}>
              {entry.parent_type}
            </span>
            {entry.is_protected && (
              <span className="flex-shrink-0 flex items-center gap-0.5 text-[0.71rem] px-1.5 py-0.5 rounded status-chip text-red-500">
                <Shield className="w-3 h-3" />
                {moduleT('hotspot.systemProtected')}
              </span>
            )}
            {entry.is_cache && !entry.is_program && !entry.is_protected && (
              <span className="flex-shrink-0 flex items-center gap-0.5 text-[0.71rem] px-1.5 py-0.5 rounded status-chip text-orange-500">
                <Trash2 className="w-3 h-3" />
                {moduleT('hotspot.temporaryCache')}
              </span>
            )}
          </div>
        </div>

        {/* Right side: size + modified date + action buttons */}
        <div className="flex-shrink-0 flex items-center gap-4 text-xs">
          <div className="hidden sm:flex items-center gap-1 text-[var(--text-muted)]">
            <HardDrive className="w-3 h-3" />
            <span>{moduleT('hotspot.fileCount', { count: entry.file_count.toLocaleString() })}</span>
          </div>
          <div className="hidden md:flex items-center gap-1 text-[var(--text-muted)]">
            <Clock className="w-3 h-3" />
            <span>{formatDateTime(entry.last_modified)}</span>
          </div>
          <div className={`font-semibold min-w-[5.0rem] text-right ${
            entry.is_protected ? 'text-red-500' : 'text-[var(--brand-green)]'
          }`}>
            {formatSize(entry.total_size)}
          </div>

          {/* Action button group */}
          <div className="flex items-center gap-1">
            {/* Drill down */}
            <button
              onClick={(e) => { e.stopPropagation(); onDrillDown(entry.path); }}
              className="p-1.5 rounded-lg hover:bg-[var(--brand-green-10)] text-[var(--brand-green)] transition-all"
              title={moduleT('drillDown.expand')}
            >
              <ChevronRight className="w-4 h-4" />
            </button>
            {/* Clean up */}
            {canCleanup && (
              <button
                onClick={(e) => { e.stopPropagation(); onCleanup(entry); }}
                className="p-1.5 rounded-lg hover:bg-[var(--hotspot-danger-fill)] text-[var(--color-warning)] transition-all"
                title={moduleT('drillDown.cleanup')}
              >
                <Trash2 className="w-4 h-4" />
              </button>
            )}
            {/* Search: use .path for the full path, .name for the folder name */}
            <button
              onClick={(e) => { e.stopPropagation(); onSearch(entry.path); }}
              className="p-1.5 rounded-lg hover:bg-[var(--bg-card)] text-[var(--text-muted)] hover:text-blue-500 transition-all"
              title={moduleT('drillDown.search')}
            >
              <Search className="w-4 h-4" />
            </button>
            {/* Open folder */}
            <button
              onClick={(e) => { e.stopPropagation(); onOpenFolder(entry.path); }}
              className="p-1.5 rounded-lg hover:bg-[var(--bg-card)] text-[var(--text-muted)] hover:text-[var(--text-primary)] transition-all"
              title={moduleT('drillDown.open')}
            >
              <FolderOpen className="w-4 h-4" />
            </button>
            {/* Quick question: this row is something **the user reached by drilling down themselves**, the data is in the question —
                use skipLookup so the assistant doesn't look it up first (this file-level detail isn't in its read capability anyway) */}
            <AiAskButton
              question={buildItemQuestion(entry.path, 'folder')}
              skipLookup
              className="inline-flex h-7 w-7 items-center justify-center"
            />
          </div>
        </div>
      </div>
    </div>
  );
});

// ============================================================================
// DrillDownModal main component
// ============================================================================

interface DrillDownModalProps {
  /** Initial path (the path passed in when drill-down is clicked) */
  initialPath: string;
  /** Close callback */
  onClose: () => void;
  /** Sync callback after cleanup happens (notifies the main UI to refresh) */
  onCleanupDone?: () => void;
}

export function DrillDownModal({ initialPath, onClose, onCleanupDone }: DrillDownModalProps) {
  const { t: moduleT } = useTranslation('modules');
  const { t } = useTranslation('common');
  const { showToast } = useToast();

  // ====== Animation state ======
  const [isVisible, setIsVisible] = useState(false);
  const [isAnimating, setIsAnimating] = useState(true);
  const enteredRef = useRef(false);
  if (isVisible) enteredRef.current = true;

  // ====== Data state ======
  /** Path stack: starts from the initial path, push on each drill-down, pop on back */
  const [pathStack, setPathStack] = useState<string[]>([initialPath]);
  const [scanResult, setScanResult] = useState<HotspotScanResult | null>(null);
  const [loading, setLoading] = useState(true);
  /** Track whether cleanup happened in this modal (for syncing on close) */
  const didCleanupRef = useRef(false);

  // ====== Cleanup confirmation dialog ======
  const [cleanupTarget, setCleanupTarget] = useState<HotspotEntry | null>(null);
  const [isCleaning, setIsCleaning] = useState(false);

  // ====== Entry cap: show 30 by default, "Show all" when exceeded (prevents hundreds of subdirectories from bloating the DOM) ======
  const [showAll, setShowAll] = useState(false);
  const DEFAULT_DISPLAY_COUNT = 30;

  // Current path (top of stack)
  const currentPath = pathStack[pathStack.length - 1];

  // ====== Enter animation ======
  useEffect(() => {
    // Delay one frame before triggering the animation
    const raf = requestAnimationFrame(() => setIsVisible(true));
    return () => cancelAnimationFrame(raf);
  }, []);

  // ====== ESC key to close ======
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') handleClose();
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // ====== Data loading ======
  const fetchData = useCallback(async (targetPath: string) => {
    setLoading(true);
    try {
      const result = await scanPathDirect(targetPath);
      setScanResult(result);
    } catch (err) {
      console.error('下钻扫描失败:', err);
      showToast({ type: 'error', title: moduleT('drillDown.scanFailed'), description: String(err) });
    } finally {
      setLoading(false);
    }
  }, [showToast]);

  // Initial load + reload on path change, also resetting the display cap
  useEffect(() => {
    fetchData(currentPath);
    setShowAll(false);
  }, [currentPath, fetchData]);

  // ====== Close (with exit animation) ======
  const handleClose = useCallback(() => {
    setIsVisible(false);
    setTimeout(() => {
      setIsAnimating(false);
      if (didCleanupRef.current) {
        onCleanupDone?.();
      }
      onClose();
    }, 190);
  }, [onClose, onCleanupDone]);

  // ====== Drill down ======
  const handleDrillDown = useCallback((path: string) => {
    setPathStack(prev => [...prev, path]);
  }, []);

  // ====== Go back to parent ======
  const handleGoBack = useCallback(() => {
    if (pathStack.length <= 1) {
      handleClose();
      return;
    }
    setPathStack(prev => prev.slice(0, -1));
  }, [pathStack.length, handleClose]);

  // ====== Breadcrumb navigation ======
  const handleBreadcrumbNavigate = useCallback((targetPath: string) => {
    // Find the matching position in pathStack and truncate there
    // If the target isn't in the stack (breadcrumbs come from splitting a full path), replace the stack directly
    const idx = pathStack.findIndex(p => p === targetPath);
    if (idx >= 0) {
      setPathStack(prev => prev.slice(0, idx + 1));
    } else {
      setPathStack([targetPath]);
    }
  }, [pathStack]);

  // ====== Open folder ======
  const handleOpenFolder = useCallback(async (path: string) => {
    try {
      await openInFolder(path);
    } catch (err) {
      console.error('打开文件夹失败:', err);
    }
  }, []);

  // ====== Search ======
  const handleSearch = useCallback(async (path: string) => {
    try {
      await openSearchUrl(`Windows 文件夹 ${path} 可以删除吗`);
    } catch (err) {
      console.error('打开搜索链接失败:', err);
    }
  }, []);

  // ====== Clean up ======
  const handleCleanupConfirm = useCallback(async () => {
    if (!cleanupTarget) return;
    setIsCleaning(true);
    try {
      const result = await cleanupDirectoryContents(cleanupTarget.path);
      if (result.deleted_count > 0) {
        showToast({
          type: 'success',
          title: moduleT('drillDown.cleanupDone'),
          description: moduleT('hotspot.cleanupDoneDesc', { count: result.deleted_count, size: formatSize(result.freed_size) }),
        });
        didCleanupRef.current = true;
        // Reload the current level
        fetchData(currentPath);
      } else if (result.failed_count > 0) {
        showToast({ type: 'warning', title: moduleT('drillDown.cleanupBlocked'), description: moduleT('hotspot.cleanupBlockedDesc', { count: result.failed_count }) });
      } else {
        showToast({ type: 'info', title: moduleT('drillDown.empty'), description: moduleT('hotspot.emptyDesc') });
      }
    } catch (err) {
      console.error('清理失败:', err);
      showToast({ type: 'error', title: moduleT('drillDown.cleanupFailed'), description: String(err) });
    } finally {
      setIsCleaning(false);
      setCleanupTarget(null);
    }
  }, [cleanupTarget, currentPath, fetchData, showToast]);

  // Breadcrumb segments (useMemo avoids recomputing on every render)
  const breadcrumbSegments = useMemo(() => buildBreadcrumbs(currentPath), [currentPath]);
  // Number of levels in the initial path (the first initialDepth levels aren't clickable because the parent module already scanned them)
  const initialDepth = useMemo(() => buildBreadcrumbs(initialPath).length, [initialPath]);
  const maxSize = scanResult?.entries[0]?.total_size || 0;

  // Entries to display (limited to 30 by default, with a "Show all" button when exceeded)
  const displayedEntries = useMemo(() => {
    if (!scanResult) return [];
    return showAll ? scanResult.entries : scanResult.entries.slice(0, DEFAULT_DISPLAY_COUNT);
  }, [scanResult, showAll]);

  if (!isAnimating) return null;

  return createPortal(
    <div className="fixed inset-0 z-[9998] flex items-center justify-center p-4">
      {/* Overlay - semi-transparent frosted */}
      <div
        className={`absolute inset-0 bg-black/40 backdrop-blur-md glass-backdrop ${
          isVisible ? 'modal-overlay-in' : enteredRef.current ? 'modal-overlay-out' : 'opacity-0'
        }`}
        onClick={handleClose}
      />

      {/* Modal body */}
      <div
        className={`glass-dialog relative flex flex-col bg-[var(--bg-elevated)] rounded-2xl shadow-2xl border border-[var(--border-default)]
          w-[51.43em] max-w-[calc(100vw-2em)] max-h-[min(57.14em,calc(100vh_-_2em))] overflow-hidden ${
          isVisible ? 'modal-content-in' : enteredRef.current ? 'modal-content-out' : 'opacity-0'
        }`}
      >
        {/* ====== Header: breadcrumbs + close button ====== */}
        {/* All inner sizes use em, scaling with the global font size, so only the shell grows while the content stays the same */}
        <div className="glass-bar flex items-center justify-between px-[1.43em] py-[0.86em] border-b border-[var(--border-default)] bg-[var(--bg-card)]/80 backdrop-blur-sm">
          <div className="flex-1 min-w-0 mr-[0.86em]">
            <ModalBreadcrumbs segments={breadcrumbSegments} initialDepth={initialDepth} onNavigate={handleBreadcrumbNavigate} />
          </div>
          <button
            onClick={handleClose}
            className="flex-shrink-0 p-[0.43em] rounded-lg text-[var(--text-muted)] hover:text-[var(--text-primary)] hover:bg-[var(--bg-hover)] transition-colors"
            title={moduleT('drillDown.close') + ' (ESC)'}
          >
            <X className="w-[1.14em] h-[1.14em]" />
          </button>
        </div>

        {/* ====== Content area: scrollable list ====== */}
        <div className="flex-1 overflow-y-auto px-[1.14em] py-[0.86em] space-y-[0.57em] min-h-[14.29em]">
          {/* Loading */}
          {loading && (
            <div className="flex items-center justify-center py-[4.57em]">
              <Loader2 className="w-[1.71em] h-[1.71em] animate-spin text-[var(--brand-green)] mr-[0.57em]" />
              <span className="text-[1em] text-[var(--text-muted)]">{moduleT('drillDown.scanning')}</span>
            </div>
          )}

          {/* List content */}
          {!loading && scanResult && (
            <>
              {/* Go back to parent */}
              <button
                onClick={handleGoBack}
                className="w-full flex items-center gap-[0.86em] p-[0.86em] rounded-xl hover:bg-[var(--bg-hover)] transition-colors text-left"
              >
                <div className="w-[2em] h-[2em] rounded-lg bg-[var(--bg-card)] border border-[var(--border-color)] flex items-center justify-center">
                  <CornerLeftUp className="w-[1.14em] h-[1.14em] text-[var(--text-muted)]" />
                </div>
                <span className="text-[1em] text-[var(--text-muted)]">
                  {pathStack.length <= 1 ? moduleT('drillDown.close') : moduleT('drillDown.back')}
                </span>
              </button>

              {/* Directory entries (30-item cap by default, prevents big directories from lagging) */}
              {displayedEntries.map((entry, index) => (
                <ModalEntryItem
                  key={entry.path}
                  entry={entry}
                  rank={index + 1}
                  maxSize={maxSize}
                  onDrillDown={handleDrillDown}
                  onOpenFolder={handleOpenFolder}
                  onCleanup={setCleanupTarget}
                  onSearch={handleSearch}
                />
              ))}

              {/* Show all button (shown when entry count exceeds the limit) */}
              {!showAll && scanResult.entries.length > DEFAULT_DISPLAY_COUNT && (
                <button
                  onClick={() => setShowAll(true)}
                  className="w-full flex items-center justify-center gap-[0.29em] py-[0.57em] text-[0.86em] text-[var(--text-muted)] hover:text-[var(--text-primary)] transition-colors"
                >
                  <span>{moduleT('drillDown.showAll', { count: scanResult.entries.length })}</span>
                  <ChevronDown className="w-[1.14em] h-[1.14em]" />
                </button>
              )}

              {/* Empty state */}
              {scanResult.entries.length === 0 && (
                <div className="text-center py-[2.86em] text-[var(--text-muted)]">
                  <FolderOpen className="w-[2.29em] h-[2.29em] mx-auto mb-[0.57em] opacity-30" />
                  <p className="text-[1em]">{moduleT('drillDown.noFolders')}</p>
                </div>
              )}
            </>
          )}
        </div>

        {/* ====== Footer: statistics ====== */}
        {!loading && scanResult && scanResult.entries.length > 0 && (
          <div className="glass-bar flex items-center justify-between px-[1.43em] py-[0.71em] border-t border-[var(--border-default)] text-[0.86em] text-[var(--text-muted)] bg-[var(--bg-card)]/80 backdrop-blur-sm">
            <span>
              {moduleT('drillDown.summary', { count: scanResult.entries.length, size: formatSize(scanResult.scanned_total_size) })}
            </span>
            <span>{moduleT('drillDown.elapsed', { time: (scanResult.scan_duration_ms / 1000).toFixed(1) })}</span>
          </div>
        )}
      </div>

      {/* Cleanup confirmation dialog (nested Portal, higher z-index) */}
      {cleanupTarget && (
        <ConfirmDialog
          isOpen={!!cleanupTarget}
          title={moduleT('hotspot.confirmCleanup')}
          description={moduleT('drillDown.confirmDesc', { name: cleanupTarget.name })}
          warning={moduleT('drillDown.occupiedWarning')}
          confirmText={isCleaning ? t('deleting') : moduleT('hotspot.confirmCleanup')}
          cancelText={t('cancel')}
          onConfirm={handleCleanupConfirm}
          onCancel={() => setCleanupTarget(null)}
          isDanger={false}
        />
      )}
    </div>,
    document.body
  );
}
