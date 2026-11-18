import { useAiModuleBridge } from '../ai/useAiModuleBridge';
/** Stable deletability check: the directory tree uses it to decide which files can be checked; a new function on every render would recompute the whole tree's selection set */
const isFileDeletable = (file: SocialFileEntry) => file.deletable;

// ============================================================================
// Social software cleanup module component - with risk classification
// Supports smart path tracing and in-depth file type classification
// ============================================================================

import { useState, useCallback, useRef, useMemo, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { createPortal } from 'react-dom';
import { AnimatePresence, motion } from 'framer-motion';
import {
  MessageCircle,
  Trash2, 
  Loader2, 
  Image, 
  FileText, 
  Share2,
  ChevronRight,
  CheckCircle2,
  FolderOpen,
  X,
  ExternalLink,
  Database,
  AlertTriangle,
  Shield,
  ShieldAlert,
  ShieldCheck,
  Clock
} from 'lucide-react';
import { ModuleCard } from '../ModuleCard';
import { FileTreeList } from '../FileTreeList';
import { ModuleDeleteProgress } from '../ModuleDeleteProgress';
import { ModuleOperationToolbar } from '../ModuleOperationToolbar';
import { ModuleScanProgress } from '../ModuleScanProgress';
import { EmptyState } from '../EmptyState';
import { ScanTimingPopover, type TimingGroup } from '../ScanTimingPopover';
import { useToast } from '../Toast';
import { useModuleDashboard } from '../../contexts/DashboardContext';
import {
  scanSocialCache,
  deleteFiles,
  openInFolder,
  openFile,
  recordCleanupAction,
  type SocialScanResult,
  type SocialFileEntry,
  type SocialCategoryStats,
  type RiskLevel,
  type CleanupLogEntryInput,
} from '../../api/commands';
import { formatSize } from '../../utils/format';
import i18n from '../../i18n';
import { getSourceKey, getSourceLabel } from '../../utils/socialSource';
import { applyDeletedPaths, resolveDeletedPaths } from '../../utils/socialScanResult';
import type { ModuleRenderProps } from './moduleProps';

// ============================================================================
// Category config
// ============================================================================

const categoryIcons: Record<string, typeof Image> = {
  chatdatabase: Database,
  imagevideo: Image,
  filetransfer: FileText,
  tempcache: Clock,
  momentscache: Share2,
};

const categoryColors: Record<string, { bg: string; text: string }> = {
  chatdatabase: { bg: 'bg-red-500/10', text: 'text-red-600' },
  imagevideo: { bg: 'bg-emerald-500/10', text: 'text-emerald-600' },
  filetransfer: { bg: 'bg-amber-500/10', text: 'text-amber-600' },
  tempcache: { bg: 'bg-teal-500/10', text: 'text-teal-600' },
  momentscache: { bg: 'bg-cyan-500/10', text: 'text-cyan-600' },
};

// Risk level config
const riskLevelConfig: Record<RiskLevel, { 
  icon: typeof Shield; 
  color: string; 
  bgColor: string;
  borderColor: string;
}> = {
  critical: { 
    icon: ShieldAlert, 
    color: 'text-red-600', 
    bgColor: 'bg-red-500/10',
    borderColor: 'border-red-500/30'
  },
  medium: { 
    icon: AlertTriangle, 
    color: 'text-amber-600', 
    bgColor: 'bg-amber-500/10',
    borderColor: 'border-amber-500/30'
  },
  low: { 
    icon: Shield, 
    color: 'text-emerald-600', 
    bgColor: 'bg-emerald-500/10',
    borderColor: 'border-emerald-500/30'
  },
  none: { 
    icon: ShieldCheck, 
    color: 'text-teal-600', 
    bgColor: 'bg-teal-500/10',
    borderColor: 'border-teal-500/30'
  },
};

// ============================================================================
// Timing details display
// ============================================================================

/** One number per line in the popover; the short ms / s format saves space compared with "1.4 seconds" */
function formatElapsed(milliseconds: number): string {
  if (milliseconds <= 0) return '<1ms';
  if (milliseconds < 1000) return `${milliseconds}ms`;
  return `${(milliseconds / 1000).toFixed(1)}s`;
}

/**
 * Stage names use scanStages in the common namespace, sharing one set of text with several other scan modules.
 *
 * Here we use global i18n instead of useTranslation's t: the text is generated inside useMemo,
 * putting t in the dependency array would rerun this mapping on every render (t's reference changes; see
 * docs/dev/03-social-cleanup.md §4.6).
 */
function getScanStageLabel(stage: string): string {
  return i18n.t(`scanStages.${stage}`, { ns: 'common', defaultValue: stage });
}

/** Paths can be long, so the popover shows only the last directory level; paths with the same name are distinguished by "directory count / file count" */
function getLastPathSegment(path: string): string {
  const segments = path.split(/[\\/]/).filter(Boolean);
  return segments[segments.length - 1] ?? path;
}

// ============================================================================
// Component implementation
// ============================================================================

export function SocialCleanModule({ isPageActive = true }: ModuleRenderProps) {
  const { t: navT } = useTranslation('nav');
  const { t } = useTranslation('common');
  const { t: moduleT } = useTranslation('modules');
  const { moduleState, updateModuleState, triggerHealthRefresh, oneClickScanTrigger } = useModuleDashboard('social');
  const { showToast } = useToast();

  // Tracks whether the current one-click scan trigger has been handled
  const lastScanTriggerRef = useRef(0);

  // Local state
  const [scanResult, setScanResult] = useState<SocialScanResult | null>(null);

  /**
   * Timing details: hidden by default; expand by clicking the stopwatch.
   *
   * Rebuilt only when scan results change — checking, filtering, and list scrolling do not recompute it.
   * The data itself is collected by the backend at phase boundaries (about 8 µs extra per scan, measured upper bound),
   * see docs/dev/scan-performance-optimization.md §6.7.
   */
  const timingGroups = useMemo<TimingGroup[]>(() => {
    if (!scanResult) return [];

    const totalPhase = scanResult.phase_durations.find((phase) => phase.stage === 'total');
    const phaseRows = scanResult.phase_durations
      .filter((phase) => phase.stage !== 'total')
      .map((phase) => ({
        label: getScanStageLabel(phase.stage),
        value: formatElapsed(phase.duration_ms),
      }));

    const slowRows = scanResult.slow_paths.map((slow) => ({
      label: `${slow.app_name} · ${getLastPathSegment(slow.path)}`,
      value: i18n.t('social.slowPathValue', {
        ns: 'modules',
        time: formatElapsed(slow.duration_ms),
        directories: slow.directory_count.toLocaleString(),
        files: slow.file_count.toLocaleString(),
      }),
    }));

    const groups: TimingGroup[] = [
      {
        title: '',
        rows: phaseRows,
        note: totalPhase
          ? i18n.t('social.timingTotal', { ns: 'modules', time: formatElapsed(totalPhase.duration_ms) })
          : undefined,
      },
    ];

    if (slowRows.length > 0) {
      groups.push({
        title: i18n.t('social.slowPaths', { ns: 'modules' }),
        rows: slowRows,
        note: i18n.t('social.slowPathsHint', { ns: 'modules' }),
      });
    }

    return groups;
  }, [scanResult]);
  // Hook up the AI assistant (read-only): provide a summary only, no action tools.
  useAiModuleBridge('social-clean', {
    // Read-only: category list (screenshots also show questions like "which chat apps are there")
    readItems: (_targetId, offset, limit) => {
      const list = scanResult?.categories ?? [];
      if (list.length === 0) return null;
      return {
        total: list.length,
        lines: list.slice(offset, offset + limit).map(
          (item) => `${item.name}  ${formatSize(item.total_size)}  （${item.file_count} 个文件，其中可清理 ${item.deletable_count} 个）`,
        ),
      };
    },
    // Read-only: single-item details. itemId uses the category name at the start of the read_items line — that is how every line in this module begins,
    // if the name doesn't match, fall back to the category id.
    readItemDetail: (itemId) => {
      const wanted = itemId.trim();
      if (!wanted) return null;
      const list = scanResult?.categories ?? [];
      const item =
        list.find((entry) => entry.name.trim() === wanted) ??
        list.find((entry) => entry.id.trim() === wanted);
      if (!item) return null;
      // "What can and cannot be deleted" is the easiest thing to get wrong here: the deletable count and the total count are two different things,
      // so give both numbers; do not let the model see only the total and say "everything can be deleted".
      const keepCount = Math.max(0, item.file_count - item.deletable_count);
      return [
        `分类：${item.name}`,
        `说明：${item.description || '（未提供说明）'}`,
        `占用：${formatSize(item.total_size)}（共 ${item.file_count} 个文件）`,
        `其中可清理：${item.deletable_count} 个`,
        `不建议清理：${keepCount} 个（通常是聊天记录、账号数据等，删了会丢内容）`,
        '结论：只有"可清理"那部分是缓存；用户确认后由模块原有的清理流程处理。',
      ].join('\n');
    },
    summary: () => {
      if (!scanResult) return null;
      const apps = scanResult.detected_apps.slice(0, 8).join('、');
      return [
        `扫描到 ${scanResult.total_files} 个文件，合计 ${formatSize(scanResult.total_size)}；`,
        `其中可清理 ${scanResult.deletable_files} 个（${formatSize(scanResult.deletable_size)}）。`,
        apps ? `涉及的聊天软件：${apps}${scanResult.detected_apps.length > 8 ? ' 等' : ''}。` : '未检测到已安装的聊天软件。',
      ].join('');
    },
  });

  const [selectedPaths, setSelectedPaths] = useState<Set<string>>(new Set());
  const [expandedCategory, setExpandedCategory] = useState<string | null>(null);
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);
  /**
   * The detail modal only records "which category is being viewed"; the file list is read live from scanResult.
   * It no longer holds a file snapshot, so when scanResult is updated locally after deletion the modal updates automatically,
   * and there is no "modal still showing an old snapshot" problem.
   */
  const [fileModalData, setFileModalData] = useState<{ categoryId: string; name: string } | null>(null);
  /** Scope of pending deletion: decided by the triggering action (toolbar = global selection; detail modal = selection within current filter).
   *  The confirmation modal's counts and text are all based on this scope, keeping "what the confirmation says" and "what is actually deleted" always consistent. */
  const [pendingDeletePaths, setPendingDeletePaths] = useState<string[]>([]);
  /** Snapshot of this deletion's file count; the progress overlay uses it to avoid the number flashing back to 0 after deletion clears the stats */
  const [deletingCount, setDeletingCount] = useState(0);

  // Start scan
  const handleScan = useCallback(async () => {
    updateModuleState('social', { status: 'scanning', error: null });
    setScanResult(null);
    setSelectedPaths(new Set());
    setExpandedCategory(null);

    try {
      const result = await scanSocialCache();
      setScanResult(result);
      
      // By default select only deletable files (exclude Critical level)
      const deletablePaths = result.categories
        .flatMap(c => c.files)
        .filter(f => f.deletable)
        .map(f => f.path);
      setSelectedPaths(new Set(deletablePaths));

      updateModuleState('social', {
        status: 'done',
        fileCount: result.total_files,
        totalSize: result.total_size,
      });
    } catch (err) {
      console.error('扫描社交软件缓存失败:', err);
      updateModuleState('social', { status: 'error', error: String(err) });
    }
  }, [updateModuleState]);

  // Listen for the one-click scan trigger
  useEffect(() => {
    if (oneClickScanTrigger > 0 && oneClickScanTrigger !== lastScanTriggerRef.current) {
      lastScanTriggerRef.current = oneClickScanTrigger;
      handleScan();
    }
  }, [oneClickScanTrigger, handleScan]);

  // Toggle selection of a single file (only deletable files allowed)
  const toggleFile = useCallback((file: SocialFileEntry) => {
    if (!file.deletable) return; // Critical level cannot be selected
    
    setSelectedPaths(prev => {
      const next = new Set(prev);
      if (next.has(file.path)) {
        next.delete(file.path);
      } else {
        next.add(file.path);
      }
      return next;
    });
  }, []);

  // Batch-set selection: select all within the filter in one commit, avoiding hundreds or thousands of state updates from toggling one by one
  const setSelectionForPaths = useCallback((paths: string[], select: boolean) => {
    setSelectedPaths(previous => {
      const next = new Set(previous);
      for (const path of paths) {
        if (select) {
          next.add(path);
        } else {
          next.delete(path);
        }
      }
      return next;
    });
  }, []);

  // Toggle category selection (only select deletable files)
  const toggleCategory = useCallback((category: SocialCategoryStats) => {
    const deletableFiles = category.files.filter(f => f.deletable);
    const categoryPaths = deletableFiles.map(f => f.path);
    const allSelected = categoryPaths.every(p => selectedPaths.has(p));
    
    setSelectedPaths(prev => {
      const next = new Set(prev);
      if (allSelected) {
        categoryPaths.forEach(p => next.delete(p));
      } else {
        categoryPaths.forEach(p => next.add(p));
      }
      return next;
    });
  }, [selectedPaths]);

  // Select all / deselect all (only select deletable files)
  const toggleSelectAll = useCallback(() => {
    if (!scanResult) return;
    const deletablePaths = scanResult.categories
      .flatMap(c => c.files)
      .filter(f => f.deletable)
      .map(f => f.path);
    
    const allSelected = deletablePaths.every(p => selectedPaths.has(p));
    if (allSelected) {
      setSelectedPaths(new Set());
    } else {
      setSelectedPaths(new Set(deletablePaths));
    }
  }, [scanResult, selectedPaths]);

  // Index scan results by path: both deletion logs and confirmation counts need to look up files by path,
  // using Array.find for a linear lookup per item degrades to O(n²) at the hundred-thousand-file scale.
  const allFilesByPath = useMemo(() => {
    const index = new Map<string, SocialFileEntry>();
    for (const category of scanResult?.categories ?? []) {
      for (const file of category.files) {
        index.set(file.path, file);
      }
    }
    return index;
  }, [scanResult]);

  // Request deletion: the scope is given by the caller, avoiding the mismatch of "clicked the current filter but deleted elsewhere"
  const requestDelete = useCallback((paths: string[]) => {
    if (paths.length === 0) return;
    setPendingDeletePaths(paths);
    setShowDeleteConfirm(true);
  }, []);

  // Perform deletion: delete only the scope recorded in pendingDeletePaths
  const handleDelete = useCallback(async () => {
    const paths = pendingDeletePaths;
    if (paths.length === 0) return;

    setIsDeleting(true);
    // Snapshot this deletion's count; the progress overlay uses it to avoid the number flashing when stats go to 0 after local update
    setDeletingCount(paths.length);
    try {
      const result = await deleteFiles(paths);

      // Record cleanup log
      const failedPathIndex = new Map((result.failed_files ?? []).map((item) => [item.path, item]));
      const logEntries: CleanupLogEntryInput[] = paths.map((path) => {
        const file = allFilesByPath.get(path);
        return {
          category: '社交软件清理',
          path,
          size: file?.size || 0,
          success: !failedPathIndex.has(path),
          error_message: failedPathIndex.get(path)?.reason,
        };
      });
      recordCleanupAction(logEntries).catch((err) => {
        console.warn('记录清理日志失败:', err);
      });
      
      if (result.failed_count === 0) {
        showToast({
          type: 'success',
          title: moduleT('social.deleteSuccess', { count: result.success_count }),
          description: moduleT('social.deleteSuccessDesc', { size: formatSize(result.freed_size) }),
        });
      } else if (result.success_count === 0) {
        showToast({
          type: 'error',
          title: moduleT('social.deleteFailed'),
          description: moduleT('social.deleteFailedDesc', { count: result.failed_count }),
        });
      } else {
        showToast({
          type: 'warning',
          title: moduleT('social.deletePartial'),
          description: moduleT('social.deletePartialDesc', { success: result.success_count, failed: result.failed_count }),
        });
      }

      // After a successful deletion, update the result set locally instead of rerunning a full scan:
      // a rescan is expensive (it must traverse every app's data directory again) and would reset the user's whole selection to "select all by default".
      // resolveDeletedPaths returning null means some files cannot be confirmed as actually deleted (e.g. marked for deletion on restart),
      // in that case fall back to a rescan — better to be a bit slower than show files still on disk as deleted.
      const removedPaths = resolveDeletedPaths(paths, result);
      if (result.success_count > 0) {
        if (removedPaths && removedPaths.size > 0 && scanResult) {
          const nextResult = applyDeletedPaths(scanResult, removedPaths);
          setScanResult(nextResult);
          updateModuleState('social', {
            status: 'done',
            fileCount: nextResult.total_files,
            totalSize: nextResult.total_size,
          });
          setSelectedPaths((previous) => {
            const remaining = new Set(previous);
            for (const path of removedPaths) {
              remaining.delete(path);
            }
            return remaining;
          });
        } else {
          // Cannot safely update locally; fall back to rescan. The result set is cleared during rescan, so close the modal to avoid a flash of blank content
          setFileModalData(null);
          handleScan();
        }
        triggerHealthRefresh();
      }
    } catch (err) {
      console.error('删除失败:', err);
      showToast({ type: 'error', title: moduleT('social.deleteFailed'), description: String(err) });
    } finally {
      setIsDeleting(false);
    }
  }, [pendingDeletePaths, allFilesByPath, scanResult, handleScan, triggerHealthRefresh, showToast, moduleT, updateModuleState]);

  // Global selection stats for the toolbar. Originally recomputed on every render; selection changes repeatedly traversed the whole result set. Now cached +
  // using the path index, dropping complexity from O(all files) to O(selection size).
  const selectedStats = useMemo(() => {
    let files = 0;
    let size = 0;
    for (const path of selectedPaths) {
      const file = allFilesByPath.get(path);
      if (file) {
        files += 1;
        size += file.size;
      }
    }
    return { files, size };
  }, [selectedPaths, allFilesByPath]);

  // Stats for the pending-delete scope: both the confirmation modal and deletion progress use it, keeping them consistent with the actual deletion scope
  const pendingDeleteStats = useMemo(() => {
    let files = 0;
    let size = 0;
    for (const path of pendingDeletePaths) {
      const file = allFilesByPath.get(path);
      if (file) {
        files += 1;
        size += file.size;
      }
    }
    return { files, size };
  }, [pendingDeletePaths, allFilesByPath]);

  // The name placeholder in the confirmation text only considers "categories of files actually being deleted this time", avoiding listing software not involved
  const confirmTargetName = useMemo(() => {
    const pendingPathSet = new Set(pendingDeletePaths);
    const categoryNames = Array.from(new Set(
      (scanResult?.categories ?? [])
        .filter(category => category.files.some(file => pendingPathSet.has(file.path)))
        .map(category => moduleT(`social.category.${category.id}.name`)),
    ));
    return categoryNames.length > 0
      ? categoryNames.join('、')
      : moduleT('social.confirmTargetFallback');
  }, [pendingDeletePaths, scanResult, moduleT]);

  // The detail modal's file list comes live from scanResult, with no separate snapshot:
  // after deletion, updating scanResult locally updates the modal automatically, avoiding "modal still showing old data".
  const fileModalFiles = useMemo(() => {
    if (!fileModalData || !scanResult) {
      return [];
    }
    return scanResult.categories.find(category => category.id === fileModalData.categoryId)?.files ?? [];
  }, [fileModalData, scanResult]);
  // Visibility is controlled by the current module, avoiding misidentification as a shared operation area.
  const shouldShowOperationToolbar = isPageActive;

  if (!isPageActive && !isDeleting && !showDeleteConfirm && !fileModalData) {
    return null;
  }

  return (
    <>
      {/* Deletion progress overlay */}
      {/* Deletion confirmation modal */}
      <SocialDeleteConfirmModal
        isOpen={showDeleteConfirm}
        onConfirm={() => {
          setShowDeleteConfirm(false);
          handleDelete();
        }}
        onCancel={() => setShowDeleteConfirm(false)}
        targetName={confirmTargetName}
        selectedFiles={pendingDeleteStats.files}
        selectedSize={pendingDeleteStats.size}
      />

      <ModuleCard
        id="social"
        title={navT('socialClean')}
        description={navT('socialCleanDesc')}
        icon={<MessageCircle className="w-6 h-6 text-[var(--brand-green)]" />}
        status={moduleState.status}
        fileCount={moduleState.fileCount}
        totalSize={moduleState.totalSize}
        onScan={handleScan}
        error={moduleState.error}
        allowStickyContent
      >
            {/* Deletion progress is now attached to the floating toolbar (see busyContent above):
                it's a position: fixed layer outside the document flow, so the list is neither stretched nor shifted —
                social cleanup needs to preserve the reference of "which items I am deleting". */}
            {/* Expanded content */}
            <div className="min-h-[21.43rem]">
              {shouldShowOperationToolbar && scanResult && scanResult.total_files > 0 && (
            // The shared toolbar handles bottom positioning; button content is still maintained by the social software module.
            <ModuleOperationToolbar
              moduleId="social"
              busy={isDeleting}
              busyContent={
                <ModuleDeleteProgress
                  variant="floating"
                  phaseLabel={moduleT('social.deleting')}
                  progressLabel={moduleT('social.deletingDesc', { count: deletingCount })}
                />
              }
            >
              <button
                onClick={toggleSelectAll}
                className="module-operation-toolbar__button module-operation-toolbar__button--muted"
              >
                {selectedPaths.size === scanResult.deletable_files ? moduleT('social.deselectAll') : moduleT('social.selectAll')}
              </button>
              <button
                onClick={() => requestDelete(Array.from(selectedPaths))}
                disabled={selectedPaths.size === 0}
                className="module-operation-toolbar__button module-operation-toolbar__button--danger"
              >
                <Trash2 className="w-3.5 h-3.5" />
                {moduleT('social.clean')} ({selectedStats.files})
              </button>
            </ModuleOperationToolbar>
          )}


          {/* Empty state */}
          {moduleState.status === 'idle' && (
            <div className="p-4">
              <EmptyState
                icon={MessageCircle}
              title={t('notScannedSocialCache')}
                description={moduleT('social.idleDesc')}
              />
            </div>
          )}

          {/* Scanning state */}
          {/* The outer p-4 matches the module's other states (empty state / hint bar):
              the module content container only sets min-h with no padding; each state adds its own padding */}
          {moduleState.status === 'scanning' && (
            <div className="p-4">
              <ModuleScanProgress
                title={moduleT('social.scanning')}
                description={moduleT('social.scanningDesc')}
                icon={<Loader2 className="h-7 w-7 animate-spin text-[var(--brand-green)]" />}
              />
            </div>
          )}

          {/* No-results state */}
          {moduleState.status === 'done' && scanResult && scanResult.total_files === 0 && (
            <div className="p-4">
              <EmptyState
                icon={CheckCircle2}
                tone="success"
              title={t('nothingToClean')}
                description={moduleT('social.emptyDesc')}
              />
            </div>
          )}

          {/* Category list: changed to the same "card column" style as junk cleanup — each row is a standalone card grouped by whitespace,
              no longer using separators to join the whole block into one long table. */}
          {moduleState.status === 'done' && scanResult && (
            // pb-20: the bottom toolbar is a fixed floating layer; bottom padding keeps the last category from being covered
            <div className="space-y-2 p-4 pb-20">
              {/* Timing details are collapsed by default: most users only look at categories and sizes; those troubleshooting click the stopwatch to see phases and slow paths.
                  Deliberately placed on the left — the right side belongs to ModuleOperationToolbar (fixed floating, z-index 20),
                  floating on the right would cover the stopwatch and make it unclickable. */}
              {scanResult.phase_durations.length > 0 && (
                <div className="flex items-center gap-2 text-xs text-[var(--text-muted)]">
                  <span className="tabular-nums">
                    {moduleT('social.scanElapsed', {
                      time: formatElapsed(
                        scanResult.phase_durations.find((phase) => phase.stage === 'total')?.duration_ms ?? 0,
                      ),
                    })}
                  </span>
                  <ScanTimingPopover groups={timingGroups} align="left" />
                </div>
              )}
              {scanResult.categories.map((category) => {
                const Icon = categoryIcons[category.id] || FolderOpen;
                const colors = categoryColors[category.id] || categoryColors.imagevideo;
                const isCategoryExpanded = expandedCategory === category.id;
                const hasFiles = category.file_count > 0;
                const deletableFiles = category.files.filter(f => f.deletable);
                const categoryPaths = deletableFiles.map(f => f.path);
                const selectedInCategory = categoryPaths.filter(p => selectedPaths.has(p)).length;
                const isAllSelected = selectedInCategory === categoryPaths.length && categoryPaths.length > 0;
                const isPartialSelected = selectedInCategory > 0 && selectedInCategory < categoryPaths.length;
            
                // Determine whether this is a dangerous category (chat history)
                const isCriticalCategory = category.id === 'chatdatabase';

                return (
                  <div
                    key={category.id}
                    className={`rounded-2xl shadow-sm overflow-hidden ${isCriticalCategory ? 'bg-red-500/5 border border-red-500/20' : 'bg-[var(--bg-card)]'}`}
                  >
                    {/* Category row */}
                    <div
                      className={`px-5 py-4 flex items-center gap-4 transition-all ${hasFiles ? 'cursor-pointer hover:bg-[var(--bg-hover)]' : 'opacity-50'}`}
                      onClick={() => hasFiles && setExpandedCategory(isCategoryExpanded ? null : category.id)}
                    >
                      <div className={`text-[var(--fg-muted)] transition-transform duration-200 ${isCategoryExpanded ? 'rotate-90' : ''}`}>
                        <ChevronRight className="w-5 h-5" />
                      </div>

                      {/* Checkbox - disabled for dangerous categories */}
                      <div
                        onClick={(e) => { 
                          e.stopPropagation(); 
                          if (hasFiles && !isCriticalCategory) toggleCategory(category); 
                        }}
                        className={`w-5 h-5 rounded border-2 flex items-center justify-center transition-colors
                          ${isCriticalCategory 
                            ? 'soft-danger border-2 cursor-not-allowed' 
                            : isAllSelected 
                              ? 'bg-emerald-500 border-emerald-500 cursor-pointer' 
                              : isPartialSelected 
                                ? 'bg-emerald-500/50 border-emerald-500 cursor-pointer' 
                                : 'border-[var(--fg-faint)] cursor-pointer'
                          }`}
                        title={isCriticalCategory ? moduleT('social.chatUndeletable') : undefined}
                      >
                        {isCriticalCategory ? (
                          <X className="w-3 h-3 text-red-500" />
                        ) : (isAllSelected || isPartialSelected) && (
                          <svg className="w-3 h-3 text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={3}>
                            <path strokeLinecap="round" strokeLinejoin="round" d="M5 13l4 4L19 7" />
                          </svg>
                        )}
                      </div>

                      {/* Icon container unified with the junk cleanup style: 40px, rounded-xl, 10% brand-green background + green icon.
                          Changing colors per category used to compete with the labels on the right, making a screen too colorful. */}
                      <div className="w-10 h-10 shrink-0 rounded-xl bg-[var(--brand-green-10)] flex items-center justify-center text-[var(--brand-green)]">
                        <Icon className="w-5 h-5" />
                      </div>

                      <div className="flex-1 min-w-0">
                        <div className="flex items-center gap-2">
                          <p className="text-[1rem] font-bold text-[var(--text-primary)] truncate">{moduleT(`social.category.${category.id}.name`)}</p>
                          {isCriticalCategory ? (
                            <span className="shrink-0 whitespace-nowrap px-2 py-0.5 rounded border text-[0.79rem] font-medium bg-red-500/10 border-red-500/20 text-red-600 flex items-center gap-1">
                              <ShieldAlert className="w-2.5 h-2.5" />
                              {moduleT('social.chatUndeletable')}
                            </span>
                          ) : (
                            <span className={`shrink-0 whitespace-nowrap px-2 py-0.5 rounded border text-[0.79rem] font-medium ${colors.bg} ${colors.text}`}>
                              {hasFiles ? `${moduleT('social.deletable')} ${category.deletable_count}` : t('noData')}
                            </span>
                          )}
                        </div>
                        <p className="text-[0.93rem] text-[var(--text-muted)] mt-1 truncate">{moduleT(`social.category.${category.id}.description`)}</p>
                      </div>

                      <div className="text-right shrink-0">
                        <p className={`text-[1.07rem] font-bold tabular-nums ${isCriticalCategory ? 'text-red-600' : 'text-[var(--text-primary)]'}`}>
                          {formatSize(category.total_size)}
                        </p>
                        <p className="text-[0.93rem] text-[var(--text-muted)] tabular-nums">{category.file_count.toLocaleString()} {moduleT('social.files')}</p>
                      </div>
                    </div>

                    {/* Expanded file list */}
                    <AnimatePresence initial={false}>
                      {isCategoryExpanded && hasFiles && (
                        <motion.div
                          key={`${category.id}-files`}
                          className="overflow-hidden bg-[var(--bg-base)] border-t border-[var(--border-default)]"
                          initial={{ height: 0, opacity: 0 }}
                          animate={{ height: 'auto', opacity: 1 }}
                          exit={{ height: 0, opacity: 0 }}
                          transition={{ duration: 0.24, ease: [0.22, 1, 0.36, 1] }}
                        >
                          {/* Directory tree: cache files under the same directory collapse into one row; checking a folder selects the whole batch */}
                          <FileTreeList
                            files={category.files.slice(0, 20)}
                            selectedPaths={selectedPaths}
                            onSetSelection={setSelectionForPaths}
                            onToggleFile={(file) => toggleFile(file)}
                            isSelectable={isFileDeletable}
                            height={192}
                            rowHeight={44}
                            renderFileMeta={(file) => <SocialFileMeta file={file} />}
                            renderFileActions={(file) => <SocialFileActions file={file} />}
                          />
                          {category.files.length > 20 && (
                            <button
                              onClick={(e) => { e.stopPropagation(); setFileModalData({ categoryId: category.id, name: moduleT(`social.category.${category.id}.name`) }); }}
                              // No top border here: the row above already has a separator line, and another one would make this area look crowded.
                              // Let the theme-colored text itself convey "clickable", with a light hover background as feedback.
                              className="w-full rounded-b-xl px-4 py-2.5 text-center text-xs text-emerald-600 hover:bg-emerald-500/5 transition"
                            >
                              {moduleT('social.viewFiles')} ({category.files.length.toLocaleString()} {moduleT('social.files')}) →
                            </button>
                          )}
                        </motion.div>
                      )}
                    </AnimatePresence>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </ModuleCard>

      {/* File detail modal */}
      <FileListModal
        isOpen={fileModalData !== null}
        title={fileModalData?.name || ''}
        files={fileModalFiles}
        selectedPaths={selectedPaths}
        onToggleFile={toggleFile}
        onSetSelectionForPaths={setSelectionForPaths}
        onRequestDelete={requestDelete}
        onClose={() => setFileModalData(null)}
      />
    </>
  );
}

// ============================================================================
// File row components
// ============================================================================

interface SocialDeleteConfirmModalProps {
  isOpen: boolean;
  /** Description of the scope actually being cleaned, used to fill the name placeholder in the confirmation text */
  targetName: string;
  selectedFiles: number;
  selectedSize: number;
  onConfirm: () => void;
  onCancel: () => void;
}

function SocialDeleteConfirmModal({
  isOpen,
  targetName,
  selectedFiles,
  selectedSize,
  onConfirm,
  onCancel,
}: SocialDeleteConfirmModalProps) {
  const { t } = useTranslation('common');
  const { t: moduleT } = useTranslation('modules');
  return createPortal(
    <AnimatePresence>
      {isOpen && (
        <motion.div
          className="fixed inset-0 z-[10050] flex items-center justify-center"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.18, ease: 'easeOut' }}
        >
          <div className="absolute inset-0 bg-black/50 backdrop-blur-sm glass-backdrop" onClick={onCancel} />
          {/* Use em for all sizing: body inherits the :root font size, so the modal scales with the global font setting; 30em ≈ 420px.
              Height also applies viewport constraints to keep the title and confirm button from being pushed out of view in short windows. */}
          <motion.div
            className="glass-dialog relative bg-[var(--bg-elevated)] rounded-xl shadow-2xl border border-[var(--border-default)] w-[30em] max-w-[calc(100vw-2em)] max-h-[calc(100vh-2em)] overflow-y-auto"
            initial={{ opacity: 0, scale: 0.96, y: 12 }}
            animate={{ opacity: 1, scale: 1, y: 0 }}
            exit={{ opacity: 0, scale: 0.96, y: 12 }}
            transition={{ duration: 0.2, ease: [0.22, 1, 0.36, 1] }}
            onClick={(event) => event.stopPropagation()}
          >
            <div className="flex items-center justify-between px-[1.43em] py-[1.14em] border-b border-[var(--border-default)]">
              <div className="flex items-center gap-[0.86em]">
                <div className="w-[2.86em] h-[2.86em] rounded-full bg-amber-500/15 flex items-center justify-center">
                  <AlertTriangle className="w-[1.43em] h-[1.43em] text-amber-500" />
                </div>
                <h3 className="text-[1.14em] font-semibold text-[var(--fg-primary)]">
                  {moduleT('social.confirmDelete')}
                </h3>
              </div>
              <button
                onClick={onCancel}
                className="p-[0.43em] rounded-lg text-[var(--fg-muted)] hover:text-[var(--fg-primary)] hover:bg-[var(--bg-hover)] transition-colors"
              >
                <X className="w-[1.14em] h-[1.14em]" />
              </button>
            </div>

            <div className="px-[1.43em] py-[1.14em] space-y-[1.14em]">
              <p className="text-[1em] text-[var(--fg-secondary)] leading-relaxed">
                {moduleT('social.confirmDeleteDesc', {
                  name: targetName,
                  count: selectedFiles.toLocaleString(),
                  size: formatSize(selectedSize),
                })}
              </p>
              <div className="bg-amber-500/10 border border-amber-500/30 rounded-lg p-[0.86em]">
                <p className="text-[0.86em] text-amber-600 dark:text-amber-400 leading-relaxed">
                  {moduleT('social.risk.mediumTip')}
                </p>
              </div>
            </div>

            <div className="glass-bar flex items-center justify-end gap-[0.86em] px-[1.43em] py-[1.14em] border-t border-[var(--border-default)] bg-[var(--bg-card)]">
              <button
                onClick={onCancel}
                className="px-[1.14em] py-[0.57em] rounded-lg text-[1em] font-medium text-[var(--fg-secondary)] hover:text-[var(--fg-primary)] hover:bg-[var(--bg-hover)] transition-colors"
              >
                {t('cancel')}
              </button>
              <button
                onClick={onConfirm}
                className="px-[1.14em] py-[0.57em] rounded-lg text-[1em] font-medium text-white transition-all bg-gradient-to-r from-rose-500 to-red-500 hover:from-rose-600 hover:to-red-600 shadow-lg shadow-rose-500/25"
              >
                {moduleT('social.clean')}
              </button>
            </div>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>,
    document.body
  );
}

// Risk text is driven by stable risk enums, avoiding direct display of Chinese text returned by the backend.
function getSocialRiskLabel(translate: (key: string) => string, level: RiskLevel): string {
  return translate(`social.risk.${level}`);
}

function getSocialRiskTooltip(translate: (key: string) => string, level: RiskLevel): string {
  return translate(`social.risk.${level}Tip`);
}

/** Extra info inside a directory tree row: source app + risk level. The row itself renders only name and size. */
function SocialFileMeta({ file }: { file: SocialFileEntry }) {
  const { t: moduleT } = useTranslation('modules');
  const riskConfig = riskLevelConfig[file.risk_level];
  const RiskIcon = riskConfig.icon;

  return (
    <div className="flex shrink-0 items-center gap-1.5">
      <span
        className={`inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[0.71rem] ${riskConfig.bgColor} ${riskConfig.color}`}
        title={getSocialRiskTooltip(moduleT, file.risk_level)}
      >
        <RiskIcon className="h-3 w-3" />
        {getSocialRiskLabel(moduleT, file.risk_level)}
      </span>
      <span className="hidden shrink-0 rounded bg-[var(--bg-hover)] px-2 py-0.5 text-[0.71rem] text-[var(--fg-muted)] sm:inline">
        {getSourceLabel(getSourceKey(file.app_name), moduleT)}
      </span>
    </div>
  );
}

/** Actions at the end of a directory tree row: open containing folder / open file */
function SocialFileActions({ file }: { file: SocialFileEntry }) {
  const { t } = useTranslation('common');

  return (
    <div className="flex shrink-0 items-center gap-0.5">
      <button
        onClick={(event) => { event.stopPropagation(); openInFolder(file.path); }}
        className="rounded p-1 text-[var(--fg-muted)] transition hover:bg-[var(--bg-elevated)] hover:text-emerald-600"
        title={t('openInFolder')}
      >
        <FolderOpen className="h-3.5 w-3.5" />
      </button>
      <button
        onClick={(event) => { event.stopPropagation(); openFile(file.path); }}
        className="rounded p-1 text-[var(--fg-muted)] transition hover:bg-[var(--bg-elevated)] hover:text-emerald-600"
        title={t('openFile')}
      >
        <ExternalLink className="h-3.5 w-3.5" />
      </button>
    </div>
  );
}

// ============================================================================
// File list modal component
// ============================================================================

interface FileListModalProps {
  title: string;
  files: SocialFileEntry[];
  selectedPaths: Set<string>;
  onToggleFile: (file: SocialFileEntry) => void;
  /** Batch-set selection: select all within the filter in one commit, avoiding many re-renders from per-item updates */
  onSetSelectionForPaths: (paths: string[], select: boolean) => void;
  /** Request opening the deletion confirmation dialog. The scope is given by the modal (files selected within the current filter),
   *  avoiding accidental deletion of files in other categories due to "select all by default after scan". */
  onRequestDelete: (paths: string[]) => void;
  isOpen: boolean;
  onClose: () => void;
}

/** Source grouping: different spellings of the same app have been normalized by getSourceKey */
interface SourceGroup {
  key: string;
  count: number;
}

function FileListModal({
  title,
  files,
  selectedPaths,
  onToggleFile,
  onSetSelectionForPaths,
  onRequestDelete,
  isOpen,
  onClose,
}: FileListModalProps) {
  const { t: moduleT } = useTranslation('modules');
  /** Set of source keys for the current filter; an empty set means "all" */
  const [activeSources, setActiveSources] = useState<Set<string>>(new Set());

  // Clear the filter on close so reopening does not stay on the previous source selection
  useEffect(() => {
    if (!isOpen) {
      setActiveSources(new Set());
    }
  }, [isOpen]);

  // Header stats and source grouping depend only on files, so both go into useMemo:
  // virtual list scrolling triggers re-renders; without caching, the whole list would be traversed on every frame
  const totalSize = useMemo(() => files.reduce((sum, file) => sum + file.size, 0), [files]);
  const deletableCount = useMemo(() => files.filter(file => file.deletable).length, [files]);

  // Source grouping depends only on files, not moduleT:
  // the t function's reference may change between renders; putting it in the dependency array would make this O(N) traversal rerun on every scroll re-render.
  // Look up display names by key at render time (few groups, negligible cost).
  const sourceGroups = useMemo<SourceGroup[]>(() => {
    const groups = new Map<string, SourceGroup>();
    for (const file of files) {
      const key = getSourceKey(file.app_name);
      const existing = groups.get(key);
      if (existing) {
        existing.count += 1;
      } else {
        groups.set(key, { key, count: 1 });
      }
    }
    return Array.from(groups.values()).sort((left, right) => right.count - left.count);
  }, [files]);

  // For "all", reuse the original array directly, skipping a pointless copy
  const visibleFiles = useMemo(() => {
    if (activeSources.size === 0) {
      return files;
    }
    return files.filter(file => activeSources.has(getSourceKey(file.app_name)));
  }, [files, activeSources]);

  /** Deletable file paths under the current filter */
  const visibleDeletablePaths = useMemo(
    () => visibleFiles.filter(file => file.deletable).map(file => file.path),
    [visibleFiles],
  );
  // Also needs caching: otherwise every scroll re-render would run a full every() check over the visible set
  const allVisibleSelected = useMemo(
    () => visibleDeletablePaths.length > 0
      && visibleDeletablePaths.every(path => selectedPaths.has(path)),
    [visibleDeletablePaths, selectedPaths],
  );

  // Select all / deselect all applies only to the current filter results, avoiding selecting files not visible in the UI
  const handleToggleSelectAll = useCallback(() => {
    onSetSelectionForPaths(visibleDeletablePaths, !allVisibleSelected);
  }, [onSetSelectionForPaths, visibleDeletablePaths, allVisibleSelected]);

  // Source tags support multi-select; clicking a selected tag again deselects it
  const handleToggleSource = useCallback((sourceKey: string) => {
    setActiveSources(previous => {
      const next = new Set(previous);
      if (next.has(sourceKey)) {
        next.delete(sourceKey);
      } else {
        next.add(sourceKey);
      }
      return next;
    });
  }, []);

  const handleClearSources = useCallback(() => setActiveSources(new Set()), []);

  /** File paths that are both within the current filter and selected. This is the actual scope of the detail modal's delete button. */
  const visibleSelectedPaths = useMemo(
    () => visibleDeletablePaths.filter(path => selectedPaths.has(path)),
    [visibleDeletablePaths, selectedPaths],
  );

  // Items hidden by the current filter but still selected. They will not be affected by this deletion, but the user must be told,
  // otherwise users would think "delete" means deleting all selected items. With no filter, skip this traversal entirely.
  const hiddenSelectedCount = useMemo(() => {
    if (activeSources.size === 0) {
      return 0;
    }
    let count = 0;
    for (const file of files) {
      if (selectedPaths.has(file.path) && !activeSources.has(getSourceKey(file.app_name))) {
        count += 1;
      }
    }
    return count;
  }, [files, selectedPaths, activeSources]);

  return createPortal(
    <AnimatePresence>
      {isOpen && (
        <motion.div
          className="fixed inset-0 z-[9999] flex items-center justify-center p-4"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.18, ease: 'easeOut' }}
        >
          <div className="absolute inset-0 bg-black/50 backdrop-blur-sm glass-backdrop" onClick={onClose} />
          <motion.div
            className="social-detail-modal glass-dialog relative bg-[var(--bg-card)] rounded-2xl border border-[var(--border-default)] shadow-2xl w-full max-w-5xl max-h-[80vh] flex flex-col overflow-hidden"
            initial={{ opacity: 0, scale: 0.96, y: 12 }}
            animate={{ opacity: 1, scale: 1, y: 0 }}
            exit={{ opacity: 0, scale: 0.96, y: 12 }}
            transition={{ duration: 0.2, ease: [0.22, 1, 0.36, 1] }}
            onClick={(event) => event.stopPropagation()}
          >
            <div className="px-6 py-4 border-b border-[var(--border-default)] flex items-center justify-between shrink-0">
              <div>
                <h3 className="text-lg font-semibold text-[var(--fg-primary)]">{title}</h3>
                <p className="text-xs text-[var(--fg-muted)] mt-0.5">
                  {files.length.toLocaleString()} {moduleT('social.files')}，{formatSize(totalSize)}
                  <span className="mx-2">|</span>
                  {moduleT('social.deletable')} {deletableCount}
                </p>
              </div>
              <button onClick={onClose} className="p-2 hover:bg-[var(--bg-hover)] rounded-lg transition">
                <X className="w-5 h-5 text-[var(--fg-muted)]" />
              </button>
            </div>

            {/* Source filter: tags support multi-select; clicking a selected tag again deselects it.
                "All" appears only when there are multiple sources; with a single source it is equivalent to the source tag and showing it would be redundant. */}
            <div className="px-6 py-3 flex items-center gap-3 text-xs shrink-0">
              <div className="flex-1 min-w-0 flex items-center gap-2 overflow-x-auto">
                {sourceGroups.length > 1 && (
                  <button
                    onClick={handleClearSources}
                    className={`shrink-0 px-3 py-1 rounded-full border transition-colors ${
                      activeSources.size === 0
                        ? 'bg-[var(--brand-green)] border-[var(--brand-green)] text-white'
                        : 'bg-[var(--bg-card)] border-[var(--border-default)] text-[var(--fg-muted)] hover:text-[var(--fg-primary)]'
                    }`}
                  >
                    {moduleT('social.allSources')} {files.length.toLocaleString()}
                  </button>
                )}
                {sourceGroups.map(group => {
                  const isActive = activeSources.has(group.key);
                  return (
                    <button
                      key={group.key}
                      onClick={() => handleToggleSource(group.key)}
                      className={`shrink-0 px-3 py-1 rounded-full border transition-colors ${
                        isActive
                          ? 'bg-[var(--brand-green)] border-[var(--brand-green)] text-white'
                          : 'bg-[var(--bg-card)] border-[var(--border-default)] text-[var(--fg-muted)] hover:text-[var(--fg-primary)]'
                      }`}
                    >
                      {getSourceLabel(group.key, moduleT)} {group.count.toLocaleString()}
                    </button>
                  );
                })}
              </div>
              {/* Select all covers only the current filter results, avoiding selecting files not visible in the UI */}
              <button
                onClick={handleToggleSelectAll}
                disabled={visibleDeletablePaths.length === 0}
                className="shrink-0 text-[var(--brand-green)] hover:underline disabled:opacity-40 disabled:no-underline disabled:cursor-not-allowed"
              >
                {allVisibleSelected ? moduleT('social.deselectAll') : moduleT('social.selectAll')}
                {` (${visibleDeletablePaths.length.toLocaleString()})`}
              </button>
            </div>
            {/* The column header row was removed along with the flat list: each directory tree row carries its own hierarchy and summary,
                adding a fixed header would no longer match (directory rows have no risk tag and the size column means something different). */}
            <div className="min-h-0 flex-1 px-4">
              {visibleFiles.length === 0 ? (
                <div className="flex h-full items-center justify-center text-xs text-[var(--fg-muted)]">
                  {moduleT('social.emptyDesc')}
                </div>
              ) : (
                <FileTreeList
                  files={visibleFiles}
                  selectedPaths={selectedPaths}
                  onSetSelection={onSetSelectionForPaths}
                  onToggleFile={(file) => onToggleFile(file)}
                  isSelectable={isFileDeletable}
                  // The modal is the only entry point for "view all files", so sorting goes here;
                  // the 192px preview list in the card still has no sorting control
                  sortable
                  height={Math.round(Math.min(560, Math.max(260, window.innerHeight * 0.45)))}
                  rowHeight={44}
                  renderFileMeta={(file) => <SocialFileMeta file={file} />}
                  renderFileActions={(file) => <SocialFileActions file={file} />}
                />
              )}
            </div>

            {/* Bottom bar: selection stats + delete entry. The deletion scope is the global selection, not limited to the current category,
                so the counts of "hidden by current filter" and "in other categories" must be stated separately. */}
            <div className="glass-bar px-6 py-3 border-t border-[var(--border-default)] bg-[var(--bg-elevated)] flex items-center justify-between gap-4 shrink-0">
              <div className="flex-1 min-w-0 flex items-center gap-x-2 gap-y-1 text-xs text-[var(--fg-muted)] flex-wrap">
                {visibleSelectedPaths.length > 0 ? (
                  <>
                    <span className="text-[var(--fg-primary)] font-medium">
                      {moduleT('social.deleteScopeHint', { count: visibleSelectedPaths.length })}
                    </span>
                    {hiddenSelectedCount > 0 && (
                      <span>· {moduleT('social.hiddenByFilterHint', { count: hiddenSelectedCount })}</span>
                    )}
                  </>
                ) : (
                  <span>{visibleFiles.length.toLocaleString()} {moduleT('social.files')}</span>
                )}
              </div>
              <button
                onClick={() => onRequestDelete(visibleSelectedPaths)}
                disabled={visibleSelectedPaths.length === 0}
                className="shrink-0 px-4 py-1.5 rounded-lg text-xs font-medium bg-[var(--brand-green)] text-white transition-opacity disabled:opacity-40 disabled:cursor-not-allowed"
              >
                {moduleT('social.deleteSelected')}
              </button>
            </div>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>,
    document.body
  );
}

// ============================================================================
// Virtual file row component
// ============================================================================

export default SocialCleanModule;
