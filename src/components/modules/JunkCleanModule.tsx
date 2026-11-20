// ============================================================================
// Junk cleanup module component
// Shows junk file scanning and cleanup features in the dashboard
// ============================================================================

import { useState, useCallback, useMemo, useEffect, useRef } from 'react';
import i18n from '../../i18n';
import { useSettings } from '../../contexts';
import { useTranslation } from 'react-i18next';
import { Trash2 } from 'lucide-react';
import { listen } from '@tauri-apps/api/event';
import { ModuleCard } from '../ModuleCard';
import { ModuleDeleteProgress } from '../ModuleDeleteProgress';
import { ModuleOperationToolbar } from '../ModuleOperationToolbar';
import { ModuleScanProgress } from '../ModuleScanProgress';
import { StopScanButton } from '../ui/StopScanButton';
import { CategoryCard } from '../CategoryCard';
import { JunkFileTreeModal } from './JunkFileTreeModal';
import { AdminRequiredHint } from '../AdminRequiredHint';
import { ScanSummary } from '../ScanSummary';
import { ScanTimingPopover, type TimingGroup } from '../ScanTimingPopover';
import { ConfirmDialog } from '../ConfirmDialog';
import { EmptyState } from '../EmptyState';
import { Switch } from '../ui/Switch';
import { useToast } from '../Toast';
import { useModuleDashboard } from '../../contexts/DashboardContext';
import {
  cancelDeepJunkScan,
  deleteDeepJunkFiles,
  enhancedDeleteFiles,
  getDeepJunkCategoryPage,
  recordCleanupAction,
  scanDeepJunkFiles,
  scanJunkFiles,
  type CleanupLogEntryInput,
  type EnhancedDeleteResult,
} from '../../api/commands';
import { formatSize } from '../../utils/format';
import { openSearchUrl } from '../../utils/searchEngine';
import type {
  CategoryScanResult,
  DeepJunkScanResult,
  EnhancedDeleteProgress,
  FileInfo,
  ScanResult,
} from '../../types';
import { useAiModuleBridge } from '../ai/useAiModuleBridge';
import type { ModuleRenderProps } from './moduleProps';
import { errorText } from '../../api/errors';

const DEEP_SCAN_STORAGE_KEY = 'lightc.junkClean.deepScan';

function loadDeepScanPreference(): boolean {
  try {
    return JSON.parse(localStorage.getItem(DEEP_SCAN_STORAGE_KEY) ?? 'false') === true;
  } catch {
    // Fall back to quick mode for older versions or manually edited localStorage, to avoid blocking module loading.
    return false;
  }
}


/** How many entries "View all files" requests from the backend each time: 20,000 is about 4 MB, so a category of tens of thousands takes two or three requests */
const VIEW_ALL_PAGE_SIZE = 20000;

/** Replace a same-named category in the result with a whole category (used after "View all files" returns the complete data) */
function replaceCategory<T extends { categories: CategoryScanResult[] }>(
  result: T,
  category: CategoryScanResult,
): T {
  return {
    ...result,
    categories: result.categories.map((item) => (
      item.display_name === category.display_name ? category : item
    )),
  };
}

/**
 * 删除完成后保留扫描结果：把已成功删除的文件从各分类里摘除，用户可以继续勾选剩下的，
 * 不必重新扫描。只摘除 success 且未标记重启删除的条目——重启标记的文件在重启前仍占着磁盘，
 * 失败的条目留着才有机会重试。
 *
 * 整类删除（深扫描勾选整类）时后端已把会话里的未加载页一并删除，has_more 直接置为 false，
 * 剩余条目按加载列表重算；其余情况按「总数减去已删除的加载项」精确收缩，未加载页不受影响。
 * 整类删除但会话失效降级（result.warning 有值）时同样退回按加载项收缩。
 *
 * 返回 unloadedOffsets：仍有未加载页的分类在删除前的加载条数。后端会话快照删除后不更新，
 * 分页必须以删除前的偏移切原列表，否则既会漏掉未删的文件、又会把已删的文件翻回来。
 */
function pruneResultAfterDelete<T extends ScanResult>(
  result: T,
  deletedPathsLower: Set<string>,
  wholeCategoryDeletedNames: Set<string>,
): { pruned: T; unloadedOffsets: Map<string, number> } {
  let totalSize = 0;
  let totalCount = 0;
  const unloadedOffsets = new Map<string, number>();
  const categories = result.categories.map((category) => {
    const wholeDeleted = wholeCategoryDeletedNames.has(category.display_name);
    const remaining = category.files.filter(
      (file) => !deletedPathsLower.has(file.path.toLowerCase()),
    );
    let next: CategoryScanResult;
    if (wholeDeleted) {
      next = {
        ...category,
        files: remaining,
        has_more: false,
        file_count: remaining.length,
        total_size: remaining.reduce((sum, file) => sum + file.size, 0),
      };
    } else {
      const removedSize = category.files
        .filter((file) => deletedPathsLower.has(file.path.toLowerCase()))
        .reduce((sum, file) => sum + file.size, 0);
      next = {
        ...category,
        file_count: Math.max(0, category.file_count - (category.files.length - remaining.length)),
        total_size: Math.max(0, category.total_size - removedSize),
      };
      if (next.has_more) unloadedOffsets.set(category.display_name, category.files.length);
    }
    totalSize += next.total_size;
    totalCount += next.file_count;
    return next;
  });
  return {
    pruned: { ...result, categories, total_size: totalSize, total_file_count: totalCount },
    unloadedOffsets,
  };
}

/**
 * Deep scanning returns only the first screen of each category; when the whole current page is selected and more pages remain, deletion must cover the complete category.
 * This way, when the user sees the category total, pagination does not cause only the first few hundred MB to be cleaned.
 */
// Stage name → Chinese label mapping; there was also a DEEP_SCAN_STAGES constant for estimating progress by stage,
// but because the estimate could jump backwards it was removed entirely, and so was this constant.

function getScanStageLabel(stage: string, isDeep: boolean): string {
  if (!isDeep) return i18n.t('scanStages.quick', { ns: 'junkClean' });
  switch (stage) {
    case 'discover': return i18n.t('scanStages.discover', { ns: 'junkClean' });
    case 'mft': return i18n.t('scanStages.mft', { ns: 'junkClean' });
    case 'path': return i18n.t('scanStages.path', { ns: 'junkClean' });
    case 'walkdir': return i18n.t('scanStages.walkdir', { ns: 'junkClean' });
    case 'filter': return i18n.t('scanStages.filter', { ns: 'junkClean' });
    case 'recycle': return i18n.t('scanStages.recycle', { ns: 'junkClean' });
    case 'parallel': return i18n.t('scanStages.parallel', { ns: 'junkClean' });
    case 'merge': return i18n.t('scanStages.merge', { ns: 'junkClean' });
    case 'metadata': return i18n.t('scanStages.metadata', { ns: 'junkClean' });
    case 'result': return i18n.t('scanStages.result', { ns: 'junkClean' });
    case 'summary': return i18n.t('scanStages.summary', { ns: 'junkClean' });
    default: return i18n.t('scanStages.default', { ns: 'junkClean' });
  }
}

function formatScanDuration(milliseconds: number): string {
  const seconds = Math.max(0, Math.floor(milliseconds / 1000));
  if (seconds < 60) return i18n.t('seconds', { ns: 'common', count: seconds });
  return i18n.t('minutes', { ns: 'common', min: Math.floor(seconds / 60), sec: seconds % 60 });
}

function getDeletePhaseLabel(phase: EnhancedDeleteProgress['phase']): string {
  return i18n.t(`deleteProgress.${phase === 'preparing' ? 'preparing' : 'cleaning'}`, { ns: 'junkClean' });
}

function formatDeleteSpeed(progress: EnhancedDeleteProgress | null): string {
  if (!progress || progress.elapsed_ms < 1000 || progress.processed_count === 0) return i18n.t('calculating', { ns: 'common' });
  const filesPerSecond = progress.processed_count / (progress.elapsed_ms / 1000);
  return i18n.t('filesPerSecond', { ns: 'common', count: filesPerSecond.toFixed(0) });
}

function getDeleteRemainingTime(progress: EnhancedDeleteProgress | null): string {
  if (!progress || progress.processed_count === 0 || progress.total_count <= progress.processed_count) {
    return progress?.processed_count === progress?.total_count
      ? i18n.t('almostDone', { ns: 'common' })
      : i18n.t('calculating', { ns: 'common' });
  }
  const remainingCount = progress.total_count - progress.processed_count;
  const remainingMilliseconds = (progress.elapsed_ms / progress.processed_count) * remainingCount;
  return i18n.t('estimatedRemaining', { ns: 'common', time: formatScanDuration(remainingMilliseconds) });
}

// ============================================================================
// Component implementation
// ============================================================================

/**
 * Group deletion failure details by reason.
 *
 * A single cleanup failing dozens or hundreds of times is common; listing them all just buries the signal. After grouping, each entry =
 * one reason + count + one example path, so the user can see at a glance "where it got stuck".
 * At most 6 groups, so when there are many reasons it does not turn back into a long list.
 */
function groupFailuresByReason(
  failed: { path: string; failure_reason?: unknown }[],
): { name: string; reason: string }[] {
  const groups = new Map<string, { count: number; example: string }>();
  for (const file of failed) {
    const reason = file.failure_reason ? JSON.stringify(file.failure_reason) : '未知原因';
    const existing = groups.get(reason);
    if (existing) {
      existing.count += 1;
    } else {
      groups.set(reason, { count: 1, example: file.path });
    }
  }
  return [...groups.entries()]
    .sort((a, b) => b[1].count - a[1].count)
    .slice(0, 6)
    .map(([reason, info]) => ({
      name: info.count > 1 ? `${info.count} 项（例如 ${info.example}）` : info.example,
      reason,
    }));
}

export function JunkCleanModule({ isPageActive = true }: ModuleRenderProps) {
  const { t } = useTranslation('junkClean');
  // The recursion depth for deep scanning comes from the settings page and is passed to the command at scan time.
  const { settings } = useSettings();
  const {
    moduleState,
    updateModuleState,
    triggerHealthRefresh,
    oneClickScanTrigger,
    stopScanTrigger,
  } = useModuleDashboard('junk');
  const { showToast } = useToast();

  // Tracks whether the current one-click scan trigger has already been handled
  const lastScanTriggerRef = useRef(0);

  // Local state
  const [scanResult, setScanResult] = useState<ScanResult | null>(null);
  const [deleteResult, setDeleteResult] = useState<EnhancedDeleteResult | null>(null);
  const [selectedPaths, setSelectedPaths] = useState<Set<string>>(new Set());
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);
  const [deleteProgress, setDeleteProgress] = useState<EnhancedDeleteProgress | null>(null);
  // Deep categories are paginated on the first screen, but category selection must keep the explicit meaning of "clean the whole category".
  const [selectedCategoryNames, setSelectedCategoryNames] = useState<Set<string>>(new Set());
  const [deepScanEnabled, setDeepScanEnabled] = useState(loadDeepScanPreference);
  const [deepScanResult, setDeepScanResult] = useState<DeepJunkScanResult | null>(null);
  const [scanMode, setScanMode] = useState<'quick' | 'deep' | null>(null);

  // Integrate the AI assistant: the module only writes this one place.
  // Division of labor: the module states "what the data looks like and what can be operated on", while the pipeline (id format, injection timing,
  // confirmation cards, permissions) all lives in the common layer — so every module follows the same shape, making long-term maintenance easier.
  useAiModuleBridge('junk-clean', {
    summary: () => {
      const result = deepScanResult ?? scanResult;
      if (!result) return null;
      return [
        `扫描结果：共 ${result.total_file_count} 个文件，合计 ${formatSize(result.total_size)}。`,
        // The selected count must be based on the **file path set**: the category set is only written when "the whole category is selected",
        // and the default selection after scanning goes through selectedPaths (learned the hard way: reading the category set gives 0)
        `用户已勾选 ${selectedPaths.size} 个文件。`,
      ].join('\n');
    },
    // Actionable items = categories. IDs use the **category enum** (serialized to stable keys like "RecycleBin"),
    // not display_name — the latter is localized, so after a language change the IDs in history no longer match.
    targets: () => {
      const result = deepScanResult ?? scanResult;
      if (!result) return [];
      return result.categories.map((category) => ({
        id: category.category,
        label: category.display_name,
        detail: [
          formatSize(category.total_size),
          `${category.file_count} 个文件`,
          `已勾选 ${category.files.filter((file) => selectedPaths.has(file.path)).length} 个`,
          category.description,
        ].join(' · '),
        // Recycle bin emptying cannot be undone, so it must be flagged separately on the card.
        // Note: do not use system_managed: that field is for system-managed directories such as Windows.old.
        irreversible: category.category === 'RecycleBin',
      }));
    },
    // P3a: select the categories named by the AI. **Reuse the module's own toggleCategorySelection** —
    // it already handles details such as "selecting a whole category must also select files loaded by pagination",
    // writing a separate implementation would inevitably diverge (and that is exactly the source of "AI and manual operations giving different results").
    onSelect: (ids) => {
      const result = deepScanResult ?? scanResult;
      if (!result) return;
      for (const id of ids) {
        const category = result.categories.find((item) => item.category === id);
        if (!category) continue;
        // System-managed items (old Windows installations) cannot be deleted in the app and are excluded from selection
        if (category.system_managed) continue;
        toggleCategorySelection(category.display_name, category.files, true);
      }
    },
    // Read-only: lets the assistant "look deeper" at which files are actually in a category (one page at a time).
    // The summary only goes down to categories; here it goes down to files — but **not all of them**, at most one page,
    // with the model deciding on its own whether to keep reading.
    readItems: (targetId, offset, limit) => {
      const result = deepScanResult ?? scanResult;
      const category = result?.categories.find((item) => item.category === targetId);
      if (!category) return null;
      return {
        total: category.file_count,
        lines: category.files
          .slice(offset, offset + limit)
          .map((file) => `${file.path}  ${formatSize(file.size)}`),
      };
    },
    // Read-only: single-item detail. itemId is the full path at the start of the line in read_items —
    // use startsWith as a fallback, since the model sometimes includes the trailing description text too.
    readItemDetail: (itemId) => {
      const result = deepScanResult ?? scanResult;
      const wanted = itemId.trim();
      if (!result || !wanted) return null;
      for (const category of result.categories) {
        const file = category.files.find((item) => item.path === wanted || wanted.startsWith(item.path));
        if (!file) continue;
        return [
          `路径：${file.path}`,
          `大小：${formatSize(file.size)}`,
          `所属分类：${category.display_name}（${category.description}）`,
          `分类风险等级：${category.risk_level}`,
        ].join('\n');
      }
      return null;
    },
    // P3b: only reached after an AI proposal plus user confirmation.
    // **Reuse handleDelete** (do not write separate deletion logic), and pass the target paths in explicitly —
    // the selection just written to state cannot be read in the same tick.
    onDelete: async (ids) => {
      const result = deepScanResult ?? scanResult;
      if (!result) {
        return { deletedCount: 0, failures: [{ name: '', reason: '当前没有扫描结果' }] };
      }

      const paths: string[] = [];
      for (const id of ids) {
        const category = result.categories.find((item) => item.category === id);
        if (!category) continue;
        category.files.forEach((file) => paths.push(file.path));
      }
      if (paths.length === 0) {
        return { deletedCount: 0, failures: [{ name: '', reason: '目标已不存在，请重新扫描' }] };
      }

      const outcome = await handleDelete(paths, ids);
      if (!outcome) {
        return { deletedCount: 0, failures: [{ name: '', reason: '删除未执行或失败，详情见界面提示' }] };
      }
      return {
        deletedCount: outcome.success_count,
        freedBytes: outcome.freed_physical_size,
        needsReboot: outcome.reboot_pending_count > 0,
        // Failure details are **grouped by reason** before being returned: failing dozens of times in one cleanup is common,
        // listing them one by one is just noise; what the user really wants is "which reasons, how many each, and an example".
        failures: groupFailuresByReason(outcome.file_results.filter((file) => !file.success)),
      };
    },
  });
  // "View all files": the category whose pages are currently being fetched, and the category shown in the dialog
  const [loadingAllCategory, setLoadingAllCategory] = useState<string | null>(null);
  const [viewAllCategoryName, setViewAllCategoryName] = useState<string | null>(null);
  const scanningRef = useRef(false);
  const cancelRequestedRef = useRef(false);
  // 删除后后端深扫描会话快照不更新（分页仍按原列表切）：记录仍有未加载页的分类在删除前的
  // 加载条数，作为后续分页的 offset，"查看全部文件"才不会漏掉未删的文件或翻回已删的文件。
  const deepPageOffsetRef = useRef<Map<string, number>>(new Map());
  // This used to estimate a scan percentage by "stage number": the backend does not report a progress total
  // (DeepJunkScanProgress only has stage / processed / matched_count / elapsed_ms),
  // so when a stage changed or a new partition restarted the stages, the number could decrease → the progress bar went backwards.
  // Without a real numerator and denominator, no percentage is shown — only elapsed time.

  // Calculate the size of selected files
  const selectedSize = useMemo(() => {
    if (!scanResult) return 0;
    let total = 0;
    for (const category of scanResult.categories) {
      for (const f of category.files) {
        if (selectedPaths.has(f.path)) {
          total += f.size;
        }
      }
      if (scanMode === 'deep' && selectedCategoryNames.has(category.display_name)) {
        // For a whole-category selection, use the category total as the basis, then subtract entries explicitly deselected on the current page.
        const excludedSize = category.files
          .filter((file) => !selectedPaths.has(file.path))
          .reduce((sum, file) => sum + file.size, 0);
        const selectedCategorySize = Math.max(0, category.total_size - excludedSize);
        const loadedSelectedSize = category.files
          .filter((file) => selectedPaths.has(file.path))
          .reduce((sum, file) => sum + file.size, 0);
        total += Math.max(0, selectedCategorySize - loadedSelectedSize);
      }
    }
    return total;
  }, [scanMode, scanResult, selectedCategoryNames, selectedPaths]);

  const selectedFileCount = useMemo(() => {
    if (!scanResult) return selectedPaths.size;
    let count = selectedPaths.size;
    if (scanMode === 'deep') {
      scanResult.categories.forEach((category) => {
        if (selectedCategoryNames.has(category.display_name)) {
          // selectedPaths already includes the current page, so only unloaded files are counted here.
          count += Math.max(0, category.file_count - category.files.length);
        }
      });
    }
    return count;
  }, [scanMode, scanResult, selectedCategoryNames, selectedPaths]);

  const fullySelectedDeepCategoryNames = useMemo(() => (
    scanMode === 'deep' ? Array.from(selectedCategoryNames) : []
  ), [scanMode, selectedCategoryNames]);

  const excludedDeepPaths = useMemo(() => {
    if (scanMode !== 'deep' || !scanResult) return [];
    return scanResult.categories
      .filter((category) => selectedCategoryNames.has(category.display_name))
      .flatMap((category) => category.files
        .filter((file) => !selectedPaths.has(file.path))
        .map((file) => file.path));
  }, [scanMode, scanResult, selectedCategoryNames, selectedPaths]);

  useEffect(() => {
    localStorage.setItem(DEEP_SCAN_STORAGE_KEY, JSON.stringify(deepScanEnabled));
  }, [deepScanEnabled]);

  // Delete progress carries only batch statistics, avoiding extra render pressure from updating the frontend file by file in bulk deletion.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let disposed = false;

    listen<EnhancedDeleteProgress>('junk-clean:delete-progress', (event) => {
      if (!disposed) setDeleteProgress(event.payload);
    }).then((dispose) => {
      if (disposed) dispose();
      else unlisten = dispose;
    }).catch((error) => {
      if (!disposed) showToast({ type: 'warning', title: t('toast.listenDeleteProgressFailed'), description: errorText(error) });
    });

    return () => {
      disposed = true;
      if (unlisten) unlisten();
    };
  }, [showToast, t]);

  // Start scanning
  const handleScan = useCallback(async () => {
    // While a delete command is running, starting a new scan is forbidden so it cannot overwrite data that is about to land.
    if (scanningRef.current) return;

    scanningRef.current = true;
    cancelRequestedRef.current = false;
    deepPageOffsetRef.current = new Map();
    const currentScanMode = deepScanEnabled ? 'deep' : 'quick';
    setScanMode(currentScanMode);
    updateModuleState('junk', { status: 'scanning', error: null });
    setScanResult(null);
    setDeepScanResult(null);
    setDeleteResult(null);
    setDeleteProgress(null);
    setSelectedPaths(new Set());
    setSelectedCategoryNames(new Set());

    try {
      const result = currentScanMode === 'deep'
        ? await scanDeepJunkFiles(settings.junkDeepScanDepth)
        : await scanJunkFiles();
      setScanResult(result);
      if (currentScanMode === 'deep') setDeepScanResult(result as DeepJunkScanResult);
      
      // Select files with risk level <= 2 by default
      const defaultSelected = new Set<string>();
      result.categories.forEach((category) => {
        if (category.risk_level <= 2) {
          category.files.forEach((file) => {
            defaultSelected.add(file.path);
          });
        }
      });
      setSelectedPaths(defaultSelected);
      setSelectedCategoryNames(new Set());

      updateModuleState('junk', {
        status: 'done',
        fileCount: result.total_file_count,
        totalSize: result.total_size,
      });

      // Auto-expand the module
    } catch (err) {
      if (cancelRequestedRef.current) {
        updateModuleState('junk', { status: 'idle', error: null });
      } else {
        console.error('扫描失败:', err);
        updateModuleState('junk', { status: 'error', error: String(err) });
      }
    } finally {
      scanningRef.current = false;
    }
  }, [deepScanEnabled, updateModuleState, settings.junkDeepScanDepth]);

  const handleStopScan = useCallback(async () => {
    if (!scanningRef.current || scanMode !== 'deep') return;

    cancelRequestedRef.current = true;
    try {
      await cancelDeepJunkScan();
      showToast({ type: 'info', title: t('toast.scanStopped'), description: t('toast.scanStoppedDesc') });
    } catch (error) {
      cancelRequestedRef.current = false;
      showToast({ type: 'error', title: t('toast.stopScanFailed'), description: errorText(error) });
    }
  }, [scanMode, showToast, t]);

  // The global stop button at the top reuses the deep scan cancel command.
  useEffect(() => {
    if (stopScanTrigger > 0 && moduleState.status === 'scanning') {
      handleStopScan();
    }
  }, [handleStopScan, moduleState.status, stopScanTrigger]);

  // Listen for the one-click scan trigger
  useEffect(() => {
    if (oneClickScanTrigger > 0 && oneClickScanTrigger !== lastScanTriggerRef.current) {
      lastScanTriggerRef.current = oneClickScanTrigger;
      handleScan();
    }
  }, [oneClickScanTrigger, handleScan]);

  // Perform deletion.
  // overridePaths / overrideCategoryNames are for "deletion on behalf of the AI assistant":
  // it has just written the selection to state, so the new value cannot be read in the same tick; pass the targets in explicitly.
  // When not provided, behavior is exactly the same as before.
  const handleDelete = useCallback(async (overridePaths?: string[], overrideCategoryNames?: string[]) => {
    const targetPaths = overridePaths ?? Array.from(selectedPaths);
    const targetCategoryNames = overrideCategoryNames ?? Array.from(selectedCategoryNames);
    if (targetPaths.length === 0 && targetCategoryNames.length === 0) return;

    // Share the same "busy" flag with handleScan: during deletion, clicking "Rescan" or toggling "Deep Discovery" would
    // clear scanResult and tear down the UI context of the in-progress deletion (also noted in the comment at handleScan).
    // The old flag was only set during scanning and empty during deletion, so it did not actually block anything.
    scanningRef.current = true;

    // Give preparing-phase feedback first so the user does not see an unresponsive overlay while the backend expands deep categories.
    setDeleteProgress({
      phase: 'preparing',
      processed_count: 0,
      total_count: selectedFileCount,
      success_count: 0,
      failed_count: 0,
      reboot_pending_count: 0,
      freed_physical_size: 0,
      elapsed_ms: 0,
    });
    setIsDeleting(true);
    try {
      const paths = targetPaths;
      const result = scanMode === 'deep'
        ? await deleteDeepJunkFiles(paths, {
          scanId: deepScanResult?.scan_id,
          // Normal paths use "categories with the whole category selected"; AI-assisted deletion uses the explicitly passed target categories
          categoryNames: overrideCategoryNames ?? fullySelectedDeepCategoryNames,
          excludedPaths: excludedDeepPaths,
        })
        : await enhancedDeleteFiles(paths);

      // Record cleanup logs (all operations are recorded, including successes and failures)
      if (result.file_results.length > 0) {
        const logEntries: CleanupLogEntryInput[] = result.file_results.map((f) => ({
          category: '垃圾清理',
          path: f.path,
          size: f.physical_size,
          success: f.success,
          error_message: f.failure_reason ? JSON.stringify(f.failure_reason) : undefined,
        }));
        // Record logs asynchronously so the UI is not blocked
        recordCleanupAction(logEntries).catch((err) => {
          console.warn('记录清理日志失败:', err);
          showToast({
            type: 'warning',
            title: t('toast.logFailed'),
            description: String(err),
          });
        });
      }

      // 清理完成：保留扫描结果，只摘除真正从磁盘上消失的文件，用户可以继续勾选删除剩下的，
      // 不用重新扫描。删除 API 的 file_results 已包含真实成功/失败/待重启明细，无需重扫验证。
      setDeleteResult(result);

      // 从磁盘上消失的文件 = 成功且未标记重启删除。重启标记的文件重启前仍占着磁盘，
      // 失败的条目留着才有机会重试，两者都继续显示在列表里。
      const deletedLower = new Set(
        result.file_results
          .filter((f) => f.success && !f.marked_for_reboot)
          .map((f) => f.path.toLowerCase()),
      );
      // 深扫描整类删除（会话未降级）时后端把未加载页也删了，整类 has_more 清零；
      // AI 代删时整类目标由 overrideCategoryNames 显式传入，与删除调用用的目标保持一致。
      const wholeCategoryDeletedNames = new Set(
        scanMode === 'deep' && !result.warning
          ? (overrideCategoryNames ?? fullySelectedDeepCategoryNames)
          : [],
      );
      // 深扫描下 scanResult 与 deepScanResult 是同一次扫描的两份引用，剪裁一次、两边同写。
      const sourceResult = (scanMode === 'deep' ? deepScanResult : null) ?? scanResult;
      if (sourceResult) {
        const { pruned, unloadedOffsets } = pruneResultAfterDelete(
          sourceResult,
          deletedLower,
          wholeCategoryDeletedNames,
        );
        deepPageOffsetRef.current = unloadedOffsets;
        setScanResult(pruned);
        if (scanMode === 'deep') setDeepScanResult(pruned as DeepJunkScanResult);
        setSelectedPaths(new Set());
        setSelectedCategoryNames(new Set());
        updateModuleState('junk', {
          status: 'done',
          fileCount: pruned.total_file_count,
          totalSize: pruned.total_size,
          error: null,
        });
      } else {
        // 没有可保留的扫描结果（防御分支，正常删除时必有结果）：维持旧行为，只显示结果卡片。
        setScanResult(null);
        setDeepScanResult(null);
        setSelectedPaths(new Set());
        setSelectedCategoryNames(new Set());
        updateModuleState('junk', { status: 'done', fileCount: 0, totalSize: 0, error: null });
      }
      setDeleteProgress(null);
      triggerHealthRefresh();

      // Return the result: AI-assisted deletion uses it to build a "how many deleted, how many failed" result card —
      // these numbers must come from the backend, not be paraphrased by the model.
      return result;

      if (result.success_count > 0) {
        // The backend summary_message is fixed in Chinese, so a summary in the current language is generated on the frontend from the structured result.
        const releasedText = result.freed_physical_size > 0
          ? i18n.t('freedSize', { ns: 'common', size: formatSize(result.freed_physical_size) })
          : '';
        const skippedText = result.skipped_size > 0
          ? i18n.t('skippedSize', { ns: 'common', size: formatSize(result.skipped_size) })
          : '';
        const blockedText = result.failed_count > 0
          ? t('toast.blocked', { count: result.failed_count })
          : '';
        const rebootText = result.reboot_pending_count > 0
          ? t('toast.reboot', { count: result.reboot_pending_count })
          : '';
        showToast({
          // Use the success color when files have actually been deleted; failures/reboot-pending are expressed through text and details, so the user does not think nothing ran.
          type: 'success',
          title: t('toast.cleanDone'),
          description: t('toast.cleanDoneDesc', {
            summary: `${releasedText}${skippedText}`,
            blocked: blockedText,
            reboot: rebootText,
          }),
        });

        // When the deep scan session expires the backend degrades to cleaning only the current page; a clear hint is needed so the user does not think the whole category was cleaned.
        if (result.warning) {
          showToast({ type: 'warning', title: t('toast.cleanDone'), description: t('toast.degradedClean') });
        }
      } else if (result.failed_count > 0 || result.reboot_pending_count > 0) {
        const firstFailure = result.file_results.find((f) => !f.success && !f.marked_for_reboot);
        // Read the path first, then check existence: writing firstFailure.path directly would be flagged as possibly undefined under the union type
        const blockedPath = firstFailure?.path;
        showToast({
          type: 'warning',
          title: t('toast.cleanBlocked'),
          description: blockedPath
            ? t('toast.cleanBlockedDesc', { path: blockedPath })
            : t('toast.cleanBlockedReboot'),
        });
      } else {
        showToast({
          type: 'info',
          title: t('toast.noFileCleaned'),
          description: t('toast.noFileCleanedDesc'),
        });
      }

    } catch (err) {
      console.error('删除失败:', err);
      showToast({ type: 'error', title: t('toast.cleanFailed'), description: String(err) });
    } finally {
      setIsDeleting(false);
      setDeleteProgress(null);
      // Paired with the flag set above; it must be restored on exit or no further scans will be possible.
      scanningRef.current = false;
    }
  }, [deepScanResult, excludedDeepPaths, fullySelectedDeepCategoryNames, scanMode, scanResult, selectedFileCount, selectedPaths, selectedCategoryNames, showToast, t, triggerHealthRefresh, updateModuleState]);

  // Junk files are searched by full path by default, while recycle bin entries are searched by their original path, to avoid handing internal $R filenames to the search engine.
  const handleSearchFile = useCallback(async (file: FileInfo) => {
    const searchPath = file.category === 'RecycleBin'
      ? file.original_path || file.name
      : file.path;
    try {
      await openSearchUrl(t('searchQuery', { path: searchPath }));
    } catch (error) {
      console.error('搜索文件用途失败:', error);
      showToast({ type: 'error', title: t('toast.openSearchFailed'), description: errorText(error) });
    }
  }, [showToast, t]);

  // Toggle a file's selection state
  const toggleFileSelection = useCallback((path: string) => {
    // Whole-category selection is kept until deletion ends; single deselects are passed to the backend via excludedPaths so files outside the paginated page are not missed.
    setSelectedPaths((prev) => {
      const newSet = new Set(prev);
      if (newSet.has(path)) {
        newSet.delete(path);
      } else {
        newSet.add(path);
      }
      return newSet;
    });
  }, []);

  /**
   * Batch selection in the directory tree: selecting a directory submits all files under it together.
   *
   * Note: this does not go through toggleFileSelection — calling setState for each of hundreds of files under a directory would
   * trigger dozens of state updates per click; here they are merged into one change.
   * Only selectedPaths is touched, not selectedCategoryNames: that one means "the whole category is selected",
   * which also implies deleting unloaded pages — a meaning directory-level selection does not have.
   */
  const setSelectionForPaths = useCallback((paths: string[], selected: boolean) => {
    if (paths.length === 0) return;
    setSelectedPaths((previous) => {
      const next = new Set(previous);
      for (const path of paths) {
        if (selected) next.add(path);
        else next.delete(path);
      }
      return next;
    });
  }, []);

  // Toggle a category's selection state
  const toggleCategorySelection = useCallback((categoryName: string, files: FileInfo[], selected: boolean) => {
    setSelectedCategoryNames((previous) => {
      const next = new Set(previous);
      if (selected) next.add(categoryName);
      else next.delete(categoryName);
      return next;
    });
    setSelectedPaths((prev) => {
      const newSet = new Set(prev);
      files.forEach((file) => {
        if (selected) {
          newSet.add(file.path);
        } else {
          newSet.delete(file.path);
        }
      });
      return newSet;
    });
  }, []);

  // Select all / deselect all
  const toggleAllSelection = useCallback((selected: boolean) => {
    if (!scanResult) return;
    if (selected) {
      const allPaths = new Set<string>();
      scanResult.categories.forEach((category) => {
        // System-managed categories (old Windows installations) are excluded from select-all: they cannot be deleted in the app,
        // and deleting them would also lose the ability to roll back to the old version; they can only be handled by Disk Cleanup.
        if (category.system_managed) return;
        category.files.forEach((file) => {
          allPaths.add(file.path);
        });
      });
      setSelectedCategoryNames(new Set(
        scanResult.categories
          .filter((category) => category.has_more && !category.system_managed)
          .map((category) => category.display_name),
      ));
      setSelectedPaths(allPaths);
    } else {
      setSelectedCategoryNames(new Set());
      setSelectedPaths(new Set());
    }
  }, [scanResult]);

  const handleDeepScanToggle = useCallback((enabled: boolean) => {
    if (scanningRef.current) return;
    setDeepScanEnabled(enabled);
    // After a mode change the old result no longer represents the current scan scope and must be cleared to avoid mistakenly deleting results from the previous mode.
    setScanResult(null);
    setDeepScanResult(null);
    setSelectedPaths(new Set());
    setSelectedCategoryNames(new Set());
    setDeleteResult(null);
    setScanMode(null);
    deepPageOffsetRef.current = new Map();
    updateModuleState('junk', { status: 'idle', error: null, fileCount: 0, totalSize: 0 });
  }, [updateModuleState]);

  /**
   * Open "View all files".
   *
   * Note: deep scanning fetches only 500 entries per category on the first screen, and the card's window cannot hold tens of thousands of files;
   * making the user click "Load more" page by page is slow and easily leads to the misjudgment "that's all there is in the category".
   * Here, opening it fetches all remaining pages at once (20,000 per batch, usually two batches),
   * and the fetched data is written back to module state — keeping the dialog, the card tree, and the selection count consistent.
   */
  const handleViewAllFiles = useCallback(async (categoryName: string) => {
    if (loadingAllCategory) return;
    const category = scanResult?.categories.find((item) => item.display_name === categoryName);
    if (!category) return;

    // Quick scanning already returns the complete list, so just open the dialog
    if (scanMode !== 'deep' || !deepScanResult || !category.has_more) {
      setViewAllCategoryName(categoryName);
      return;
    }

    setLoadingAllCategory(categoryName);
    try {
      let merged = category;
      // 删除后加载列表里已有文件被摘除，不再是会话原列表的精确前缀：分页 offset 必须用
      // 删除前记录的加载条数（deepPageOffsetRef），否则会跳过未删文件、把已删文件翻回来。
      let offset = deepPageOffsetRef.current.get(categoryName) ?? merged.files.length;
      // Theoretically has_more clears as pages advance; a hard cap is still kept here so bad data cannot freeze the UI
      for (let round = 0; round < 20 && merged.has_more; round += 1) {
        const page = await getDeepJunkCategoryPage(
          deepScanResult.scan_id,
          categoryName,
          offset,
          VIEW_ALL_PAGE_SIZE,
        );
        if (page.files.length === 0) break;
        offset += page.files.length;
        merged = {
          ...merged,
          files: [...merged.files, ...page.files],
          has_more: page.has_more,
        };
      }
      // 分页中途失败时 offset 只走到已取到的位置，记录下来下次从这里继续；
      // 整类取完后（has_more 清零）记录就没用了，直接删掉。
      if (merged.has_more) deepPageOffsetRef.current.set(categoryName, offset);
      else deepPageOffsetRef.current.delete(categoryName);
      const completed = merged;
      setScanResult((previous) => previous ? replaceCategory(previous, completed) : previous);
      setDeepScanResult((previous) => previous
        ? replaceCategory(previous, completed) as DeepJunkScanResult
        : previous);
      if (selectedCategoryNames.has(categoryName)) {
        // Whole category already selected: newly loaded files must be selected too, otherwise "whole category selected" is not true
        setSelectedPaths((previous) => {
          const next = new Set(previous);
          completed.files.forEach((file) => next.add(file.path));
          return next;
        });
      }
      setViewAllCategoryName(categoryName);
    } catch (error) {
      showToast({ type: 'warning', title: t('toast.loadDeepPageFailed'), description: errorText(error) });
    } finally {
      setLoadingAllCategory(null);
    }
  }, [deepScanResult, loadingAllCategory, scanMode, scanResult, selectedCategoryNames, showToast, t]);

  // The category in the dialog comes from module state, so it updates or disappears automatically after deletion/rescan
  const viewAllCategory = viewAllCategoryName
    ? scanResult?.categories.find((item) => item.display_name === viewAllCategoryName) ?? null
    : null;

  // Visibility is controlled by this module, avoiding shared display conditions across modules.
  const shouldShowOperationToolbar = isPageActive;
  // The bottom operation bar only appears when "there are deletable results"; it is absent during deletion and when only the result card remains after cleanup.
  // The 80px padding at the bottom of the content must follow the same condition, otherwise an empty gap remains when the bar is gone.
  const showOperations = !isDeleting
    && shouldShowOperationToolbar
    && !!scanResult
    && scanResult.total_file_count > 0;
  const deleteTotalCount = deleteProgress?.total_count || selectedFileCount;
  const deleteProcessedCount = Math.min(deleteProgress?.processed_count ?? 0, deleteTotalCount);
  const deleteProgressPercent = deleteTotalCount > 0
    ? Math.min(100, Math.round((deleteProcessedCount / deleteTotalCount) * 100))
    : 0;

  // Timing details: this whole block of "per-drive stage timings" used to be printed directly on the page; after release it was moved into a popover, leaving only a small stopwatch icon.
  // The data itself is kept — it is the first-hand basis for diagnosing slowdowns and further optimization, it just should not take up the user's space.
  const timingGroups: TimingGroup[] = [];
  if (deepScanResult) {
    for (const drive of deepScanResult.drives) {
      const rows = (drive.stage_durations ?? []).map((item) => ({
        label: getScanStageLabel(item.stage, true),
        value: `${(item.duration_ms / 1000).toFixed(1)}s`,
      }));
      // MFT segmentation / disk read / parsing are not put into rows: reading and parsing run concurrently, and listing them as parallel stages would imply they should be added
      const notes: string[] = [];
      if (drive.mft_fragments) {
        notes.push(
          t('drives.mftStats', {
            count: drive.mft_fragments,
            io: ((drive.mft_io_ms ?? 0) / 1000).toFixed(1),
            parse: ((drive.mft_parse_ms ?? 0) / 1000).toFixed(1),
          }),
        );
      }
      if (drive.total_duration_ms) {
        notes.push(`${(drive.total_duration_ms / 1000).toFixed(1)}s`);
      }
      timingGroups.push({
        title: `${drive.drive_letter} · ${formatSize(drive.matched_size)}`,
        rows,
        note: notes.join(' · '),
      });
    }

    const outerStages = deepScanResult.outer_stages ?? [];
    if (outerStages.length > 0) {
      timingGroups.push({
        title: t('drives.outerStages'),
        rows: outerStages.map((item) => ({
          label: getScanStageLabel(item.stage, true),
          value: `${(item.duration_ms / 1000).toFixed(1)}s`,
        })),
        note: (deepScanResult.emit_wait_ms ?? 0) > 0
          ? t('drives.emitWait', { time: ((deepScanResult.emit_wait_ms ?? 0) / 1000).toFixed(1) })
          : undefined,
      });
    }
  }
  // Failed/degraded drives must stay on the page and must not be tucked into the timing popover
  const driveWarnings = (deepScanResult?.drives ?? []).filter((drive) => Boolean(drive.warning));

  if (!isPageActive && !isDeleting && !showDeleteConfirm) {
    return null;
  }

  return (
    <>
      {/* View all files: deep scan only fetches the first screen of a category; the full directory tree is shown in the dialog */}
      <JunkFileTreeModal
        category={isDeleting ? null : viewAllCategory}
        selectedPaths={selectedPaths}
        onToggleFile={toggleFileSelection}
        onSetSelectionForPaths={setSelectionForPaths}
        onSearchFile={handleSearchFile}
        onRequestDelete={() => {
          setViewAllCategoryName(null);
          setShowDeleteConfirm(true);
        }}
        onClose={() => setViewAllCategoryName(null)}
      />

      {/* Delete confirmation dialog */}
      <ConfirmDialog
        isOpen={showDeleteConfirm}
        title={t('confirmTitle')}
        description={t('confirmDesc', { count: selectedFileCount.toLocaleString(), size: formatSize(selectedSize) })}
        warning={i18n.t('disclaimer', { ns: 'common' })}
        confirmText={t('confirmTitle')}
        cancelText={i18n.t('cancel', { ns: 'common' })}
        onConfirm={() => {
          setShowDeleteConfirm(false);
          handleDelete();
        }}
        onCancel={() => setShowDeleteConfirm(false)}
        isDanger
      />

      <ModuleCard
        id="junk"
        title={t('title')}
        description={t('desc')}
        icon={<Trash2 className="w-6 h-6 text-[var(--brand-green)]" />}
        status={moduleState.status}
        // Do not show the previous scan's count during deletion — those files are being cleaned, and leaving it at the title's right would suggest there is still something to delete.
        // Setting fileCount to zero makes ModuleCard take the "done but no results" branch, so the badge is turned off at the same time.
        fileCount={isDeleting ? 0 : moduleState.fileCount}
        totalSize={moduleState.totalSize}
        hideDoneBadge={isDeleting}
        onScan={handleScan}
        error={moduleState.error}
        headerExtra={
          <div className="flex max-w-full flex-wrap items-center gap-2">
            <label
              className="flex items-center gap-2 px-2.5 py-1.5 rounded-lg bg-[var(--bg-hover)] text-xs text-[var(--fg-muted)] cursor-pointer select-none"
              title={t('deepDiscoveryTitle')}
            >
              <span>{t('deepDiscovery')}</span>
              {/* Switch to the shared ui/Switch. This used to be a hand-drawn "track + thumb",
                  with --border-color as the background, making the thumb almost invisible on light glass. */}
              <Switch
                checked={deepScanEnabled}
                disabled={moduleState.status === 'scanning'}
                onChange={handleDeepScanToggle}
                aria-label={t('deepDiscovery')}
              />
            </label>
            {moduleState.status === 'scanning' && deepScanEnabled && (
              <StopScanButton onClick={handleStopScan} />
            )}
          </div>
        }
        allowStickyContent
      >
        {/* Expanded content */}
        {/* pb-20: the bottom operation bar is a fixed floating layer; padding at the end keeps the last category from being covered.
            No gap is left when the bar is hidden (during deletion / after cleanup). */}
        <div className={`p-4 space-y-3 ${showOperations ? 'pb-20' : ''}`}>
          {/* Deep discovery reads each drive's $MFT directly, and opening raw volumes requires admin rights; non-admins fall back to per-directory traversal,
              which is an order of magnitude slower. Only show this hint when the user has enabled deep discovery — quick-scan users do not need the noise. */}
          {!isDeleting && deepScanEnabled && (
            <AdminRequiredHint variant="performance" description={t('deepAdminHint')} />
          )}

          {/* During deletion only the progress panel is shown; the stats card and category list below are "pre-deletion" data,
              and leaving them up would suggest the cleanup was incomplete. The deletion itself runs on a backend thread, so the UI need not block. */}
          {isDeleting && (
            <ModuleDeleteProgress
              phaseLabel={getDeletePhaseLabel(deleteProgress?.phase ?? 'preparing')}
              progressLabel={t('deleteProgress.processed', { current: deleteProcessedCount.toLocaleString(), total: deleteTotalCount.toLocaleString() })}
              percent={deleteProgressPercent}
              details={[
                t('deleteProgress.freed', { size: formatSize(deleteProgress?.freed_physical_size ?? 0) }),
                t('deleteProgress.failed', { count: deleteProgress?.failed_count ?? 0 }),
                t('deleteProgress.speed', { speed: formatDeleteSpeed(deleteProgress) }),
                getDeleteRemainingTime(deleteProgress),
              ]}
              hint={t('deleteProgress.doNotClose')}
            />
          )}

          {showOperations && (
            // The common operation bar handles bottom positioning uniformly; button content is still maintained by the junk cleanup module.
            <ModuleOperationToolbar moduleId="junk">
              <button
                onClick={() => toggleAllSelection(true)}
                title={scanMode === 'deep' ? t('selectAllDeepTitle') : undefined}
                className="module-operation-toolbar__button module-operation-toolbar__button--muted"
              >
                {scanMode === 'deep' ? i18n.t('selectAllLoaded', { ns: 'common' }) : i18n.t('selectAll', { ns: 'common' })}
              </button>
              <button
                onClick={() => toggleAllSelection(false)}
                className="module-operation-toolbar__button module-operation-toolbar__button--muted"
              >
                {i18n.t('deselect', { ns: 'common' })}
              </button>
              <button
                onClick={() => setShowDeleteConfirm(true)}
                disabled={selectedPaths.size === 0 && selectedCategoryNames.size === 0}
                className="module-operation-toolbar__button module-operation-toolbar__button--danger"
              >
                <Trash2 className="w-3.5 h-3.5" />
                {t('cleanBtn', { count: selectedFileCount })}
              </button>
            </ModuleOperationToolbar>
          )}

          {/* Scan summary: show the stats card when scan data exists; after cleanup scanResult is cleared,
              so the cleanup result card is still shown when only deleteResult remains */}
          {!isDeleting && (scanResult || deleteResult) && (
            <ScanSummary
              scanResult={scanResult}
              deleteResult={deleteResult}
              selectedCount={selectedPaths.size}
              selectedSize={selectedSize}
              timingDetail={timingGroups.length > 0 ? <ScanTimingPopover groups={timingGroups} /> : undefined}
            />
          )}

          {/* Scanning state: uses the common component uniformly.
              Stage names (enumerating NTFS file records), processed record count, and candidate file count are implementation details
              the user does not need when viewing results; for timing analysis use ScanTimingPopover. Only "what is being searched" is kept here. */}
          {moduleState.status === 'scanning' && (
            <div className="p-4">
              <ModuleScanProgress
                title={scanMode === 'deep' ? t('scanning.deepTitle') : t('scanning.quickTitle')}
                description={scanMode === 'deep' ? t('scanning.deepDesc') : t('scanning.quickDesc')}
                footer={
                  scanMode === 'deep' ? (
                    <p className="text-xs leading-relaxed text-[var(--text-muted)]">
                      {t('scanning.safetyNote')}
                    </p>
                  ) : undefined
                }
              />
            </div>
          )}

          {/* Failed or degraded drives: these hints must stay on the page and not be tucked into the timing popover */}
          {!isDeleting && driveWarnings.length > 0 && (
            <div className="space-y-1 text-[0.72rem] text-[var(--color-warning,inherit)]">
              {driveWarnings.map((drive) => (
                <div key={drive.drive_letter}>
                  {drive.drive_letter} · {drive.warning}
                </div>
              ))}
            </div>
          )}

          {/* Category list */}
          {!isDeleting && scanResult ? (
            <div className="space-y-2">
              {scanResult.categories
                .filter((c) => c.files.length > 0)
                .sort((a, b) => b.total_size - a.total_size)
                .map((category) => (
                  <CategoryCard
                    key={category.display_name}
                    category={category}
                    selectedPaths={selectedPaths}
                    onToggleFile={toggleFileSelection}
                    onToggleCategory={toggleCategorySelection}
                    onSetSelectionForPaths={setSelectionForPaths}
                    onSearchFile={handleSearchFile}
                    hasMore={scanMode === 'deep' && category.has_more === true}
                    onViewAllFiles={() => handleViewAllFiles(category.display_name)}
                    isLoadingAll={loadingAllCategory === category.display_name}
                  />
                ))}

              {scanResult.categories.every((c) => c.files.length === 0) && (
                <EmptyState
                  icon={Trash2}
                  title={t('emptyTitle')}
                  description={t('emptyDesc')}
                  tone="success"
                  compact
                />
              )}
            </div>
          ) : moduleState.status === 'idle' ? (
            <EmptyState
              icon={Trash2}
              title={t('idleTitle')}
              description={t('idleDesc')}
            />
          ) : null}
        </div>
      </ModuleCard>
    </>
  );
}

export default JunkCleanModule;
