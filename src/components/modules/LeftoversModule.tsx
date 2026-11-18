// ============================================================================
// Leftover & cache scan module (formerly "Uninstall Leftovers"; supports deep detection of emulator and driver leftovers)
// Scans AppData and ProgramData for orphaned folders left behind by uninstalled software
//
// Since the multi-version leftovers and the "other software caches" group moved here from the dev space module, a full
// scan runs **three** backends in parallel: this module's own AppData/ProgramData heuristic, the dev-cache module's
// version-directory measurement, and the dev-cache module's other-software-cache measurement.
// They are rendered as three sections (they have different deletion contracts), so a failure of one must not hide the others.
// ============================================================================

import { useState, useCallback, useMemo, useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { createPortal } from 'react-dom';
import { Package, Loader2, Trash2, FolderOpen, Search, AlertTriangle, CheckCircle2, Smartphone, HardDrive, ChevronDown, ChevronUp, XCircle, ShieldCheck, ShieldPlus } from 'lucide-react';
import { ModuleCard } from '../ModuleCard';
import { ModuleOperationToolbar } from '../ModuleOperationToolbar';
import { AiAskButton } from '../ai/AiAskButton';
import { useAiModuleBridge } from '../ai/useAiModuleBridge';
import { buildItemQuestion } from '../ai/aiQuestions';
import { ModuleScanProgress } from '../ModuleScanProgress';
import { ConfirmDialog } from '../ConfirmDialog';
import { EmptyState } from '../EmptyState';
import { useToast } from '../Toast';
import { LeftoverWhitelistModal } from './LeftoverWhitelistModal';
import { VersionLeftoverSection } from './VersionLeftoverSection';
import { OtherAppCacheSection } from './OtherAppCacheSection';
import { useModuleDashboard } from '../../contexts/DashboardContext';
import {
  scanUninstallLeftovers,
  deleteLeftoverFolders,
  deleteLeftoversPermanent,
  addLeftoverWhitelistEntry,
  listLeftoverWhitelist,
  removeLeftoverWhitelistEntry,
  openInFolder,
  recordCleanupAction,
  type LeftoverScanResult,
  type LeftoverEntry,
  type PermanentDeleteResult,
  type CleanupLogEntryInput,
  type LeftoverWhitelistEntry,
  type SafetyCheckResult,
} from '../../api/commands';
import { scanOtherAppCaches, scanVersionLeftovers } from '../../api/devCache';
import type { DevCacheScanResult, VersionLeftoverScanResult } from '../../types/devCache';
import { formatSize } from '../../utils/format';
import { openSearchUrl } from '../../utils/searchEngine';
import type { ModuleRenderProps } from './moduleProps';
import { errorText } from '../../api/errors';

/**
 * Human-readable description of a safety check result.
 *
 * Lives here rather than in `api/commands.ts`: the text is UI copy, and that module
 * has no access to the translation function. The previous copy there had been
 * mojibake for a while (`安全` → `瀹夊叏`) precisely because it sat outside i18n.
 */
function safetyCheckText(result: SafetyCheckResult, t: (key: string, options?: Record<string, unknown>) => string): string {
  if (result === 'Safe') return t('leftoversExtra.safetySafe');
  if ('FoundInRegistry' in result) {
    return t('leftoversExtra.safetyRegistryMatch', {
      field: result.FoundInRegistry.matched_field,
      value: result.FoundInRegistry.matched_value,
    });
  }
  if ('ContainsExecutables' in result) {
    const files = result.ContainsExecutables.files;
    return t('leftoversExtra.safetyExecutables', {
      fileCount: files.length,
      preview: files.slice(0, 3).join(', '),
    });
  }
  if ('InProtectedPath' in result) {
    return t('leftoversExtra.safetyProtectedPath', { reason: result.InProtectedPath.reason });
  }
  return t('leftoversExtra.safetyUnknown');
}

// ============================================================================
// Component implementation
// ============================================================================

/**
 * Display duration for deletion-related toasts.
 *
 * Toast defaults to 4 seconds, but deletion-failure/skip notices usually carry a list of remedies
 * ("2 folders contain program files, try deep cleanup..."), which cannot be read in 4 seconds before disappearing,
 * leaving users with only a "flash" — effectively no notice. Add 3 more seconds.
 */
const DELETE_TOAST_DURATION_MS = 7000;

/**
 * Unified path comparison rules.
 *
 * Note: scan results and whitelist paths may differ only in case or slash direction; comparing raw strings misses matches,
 * causing "entries already whitelisted still remain in the current results" and disagreeing with the backend's case-insensitive comparison.
 */
function normalizePathForCompare(path: string): string {
  return path.replace(/\//g, '\\').replace(/\\+$/, '').toLowerCase();
}

function isSamePath(left: string, right: string): boolean {
  return normalizePathForCompare(left) === normalizePathForCompare(right);
}

/**
 * Entries/size contributed by the multi-version section (one item per version directory).
 *
 * Kept as a pure function outside the component: the page shows three lists from two backends, and every place that
 * writes the module card has to add up the same three numbers.
 */
function versionTotals(result: VersionLeftoverScanResult | null): { count: number; size: number } {
  if (!result) return { count: 0, size: 0 };
  return {
    count: result.groups.reduce((sum, group) => sum + group.entries.length, 0),
    size: result.totalSize,
  };
}

/** Entries/size contributed by the other-software-cache section (one item per directory). */
function cacheTotals(result: DevCacheScanResult | null): { count: number; size: number } {
  if (!result) return { count: 0, size: 0 };
  return {
    count: result.groups.reduce((sum, group) => sum + group.entries.length, 0),
    size: result.totalSize,
  };
}

export function LeftoversModule({ isPageActive = true }: ModuleRenderProps) {
  const { t: navT } = useTranslation('nav');
  const { t } = useTranslation('common');
  const { t: moduleT } = useTranslation('modules');
  const { moduleState, updateModuleState, triggerHealthRefresh, oneClickScanTrigger } = useModuleDashboard('leftovers');
  const { showToast } = useToast();

  const lastScanTriggerRef = useRef(0);

  // Local state
  const [scanResult, setScanResult] = useState<LeftoverScanResult | null>(null);
  /**
   * Multi-version leftovers (old version directories of the same software).
   *
   * Kept as separate state instead of being merged into `scanResult`: it comes from a different backend command,
   * can succeed while the leftover scan fails, and is rendered as its own section with its own actions.
   */
  const [versionResult, setVersionResult] = useState<VersionLeftoverScanResult | null>(null);
  const [versionError, setVersionError] = useState<string | null>(null);
  /**
   * Other software caches (driver installers, launcher web caches…).
   *
   * Same reasoning as `versionResult`: different backend, different deletion contract, so it is separate state
   * and a separate section — even though it shares the `delete_dev_cache_paths` command with the version block.
   */
  const [otherResult, setOtherResult] = useState<DevCacheScanResult | null>(null);
  const [otherError, setOtherError] = useState<string | null>(null);
  const [selectedPaths, setSelectedPaths] = useState<Set<string>>(new Set());
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState<string | null>(null);
  const [deleteErrors, setDeleteErrors] = useState<string[]>([]); // detailed error list
  /**
   * Paths corresponding one-to-one with `deleteErrors` (the backend guarantees `errors` and `failed_paths` share indices).
   *
   * Do not parse paths out of error text: that Chinese description can change at any time, and string reverse-parsing will eventually break;
   * only with this separate array can an "open directory" button be attached beside each error.
   */
  const [deleteFailedPaths, setDeleteFailedPaths] = useState<string[]>([]);
  const [showErrorDetails, setShowErrorDetails] = useState(false); // whether to show error details

  // Deep cleanup (permanent delete) state
  const [showDeepCleanWarning, setShowDeepCleanWarning] = useState(false); // first-time deep cleanup warning
  const [showDeepCleanConfirm, setShowDeepCleanConfirm] = useState(false); // deep cleanup confirmation
  const [deepCleanResult, setDeepCleanResult] = useState<PermanentDeleteResult | null>(null); // deep cleanup result
  const [showDeepCleanResult, setShowDeepCleanResult] = useState(false); // show deep cleanup result
  const [whitelistEntries, setWhitelistEntries] = useState<LeftoverWhitelistEntry[]>([]);
  const [showWhitelistManager, setShowWhitelistManager] = useState(false);
  const [whitelistCandidate, setWhitelistCandidate] = useState<LeftoverEntry | null>(null);
  const [isUpdatingWhitelist, setIsUpdatingWhitelist] = useState(false);
  const [whitelistError, setWhitelistError] = useState<string | null>(null);

  // Animation state - deletion progress overlay
  const [isDeletingVisible, setIsDeletingVisible] = useState(false);
  const [isDeletingAnimating, setIsDeletingAnimating] = useState(false);
  const deletingEnteredRef = useRef(false);
  if (isDeletingVisible) deletingEnteredRef.current = true;
  useEffect(() => {
    if (isDeleting) {
      setIsDeletingAnimating(true);
      setIsDeletingVisible(true);
    } else {
      setIsDeletingVisible(false);
      const timer = setTimeout(() => setIsDeletingAnimating(false), 200);
      return () => clearTimeout(timer);
    }
  }, [isDeleting]);

  // Animation state - deep cleanup warning modal
  const [isWarningVisible, setIsWarningVisible] = useState(false);
  const [isWarningAnimating, setIsWarningAnimating] = useState(false);
  const warningEnteredRef = useRef(false);
  if (isWarningVisible) warningEnteredRef.current = true;
  useEffect(() => {
    if (showDeepCleanWarning) {
      setIsWarningAnimating(true);
      setIsWarningVisible(true);
    } else {
      setIsWarningVisible(false);
      const timer = setTimeout(() => setIsWarningAnimating(false), 200);
      return () => clearTimeout(timer);
    }
  }, [showDeepCleanWarning]);

  // Animation state - deep cleanup result modal
  const [isResultVisible, setIsResultVisible] = useState(false);
  const [isResultAnimating, setIsResultAnimating] = useState(false);
  const resultEnteredRef = useRef(false);
  if (isResultVisible) resultEnteredRef.current = true;
  useEffect(() => {
    if (showDeepCleanResult && deepCleanResult) {
      setIsResultAnimating(true);
      setIsResultVisible(true);
    } else {
      setIsResultVisible(false);
      const timer = setTimeout(() => setIsResultAnimating(false), 200);
      return () => clearTimeout(timer);
    }
  }, [showDeepCleanResult, deepCleanResult]);

  // Calculate selected size
  const selectedSize = useMemo(() => {
    if (!scanResult) return 0;
    return scanResult.leftovers
      .filter(l => selectedPaths.has(l.path))
      .reduce((sum, l) => sum + l.size, 0);
  }, [scanResult, selectedPaths]);

  /**
   * Push the module card's counters to the dashboard.
   *
   * The card has one "files / size" pair for the whole page, so every caller must pass all three lists —
   * the AppData leftovers it is given, plus the two dev-space sections held in state. Otherwise deleting an
   * old version or a launcher cache would not change the number in the top-right corner, which reads like nothing happened.
   * `fileCount` here counts *items* (leftover folders + version directories + cache directories), matching the
   * original semantics of this module.
   */
  const pushStats = useCallback(
    (
      leftovers: { count: number; size: number },
      versions: VersionLeftoverScanResult | null,
      others: DevCacheScanResult | null,
    ) => {
      const versionPart = versionTotals(versions);
      const cachePart = cacheTotals(others);
      updateModuleState('leftovers', {
        fileCount: leftovers.count + versionPart.count + cachePart.count,
        totalSize: leftovers.size + versionPart.size + cachePart.size,
      });
    },
    [updateModuleState],
  );

  /** Shorthand for the two callers that only mutate the leftover list and keep both sections as they are */
  const updateStats = useCallback(
    (leftoverCount: number, leftoverSize: number) => {
      pushStats({ count: leftoverCount, size: leftoverSize }, versionResult, otherResult);
    },
    [otherResult, pushStats, versionResult],
  );

  const loadWhitelist = useCallback(async () => {
    try {
      const entries = await listLeftoverWhitelist();
      setWhitelistEntries(entries);
      setWhitelistError(null);
    } catch (error) {
      // The whitelist is part of the deletion safety boundary; on load failure, show an explicit notice so users do not assume protection is active.
      setWhitelistError(errorText(error));
    }
  }, []);

  useEffect(() => {
    void loadWhitelist();
  }, [loadWhitelist]);

  const handleAddToWhitelist = useCallback(async () => {
    if (!whitelistCandidate) return;

    setIsUpdatingWhitelist(true);
    try {
      const entry = await addLeftoverWhitelistEntry(whitelistCandidate.path);
      setWhitelistEntries((entries) => {
        const withoutDuplicate = entries.filter((item) => item.path.toLowerCase() !== entry.path.toLowerCase());
        return [...withoutDuplicate, entry];
      });

      // Immediately remove the current result so users cannot keep mistakenly deleting a protected path in the same scan.
      // Path comparison must match the backend (ignore case and slashes), otherwise protection succeeds but the entry stays in the list.
      setScanResult((result) => {
        if (!result) return result;
        const leftovers = result.leftovers.filter((item) => !isSamePath(item.path, whitelistCandidate.path));
        const totalSize = leftovers.reduce((sum, item) => sum + item.size, 0);
        updateStats(leftovers.length, totalSize);
        return { ...result, leftovers, total_size: totalSize };
      });
      setSelectedPaths((paths) => {
        const next = new Set(paths);
        for (const selected of paths) {
          if (isSamePath(selected, whitelistCandidate.path)) next.delete(selected);
        }
        return next;
      });
      setWhitelistCandidate(null);
      setWhitelistError(null);
    } catch (error) {
      /*
        Failures to add must be visible to users: previously only whitelistError was written, and that text was shown only in the whitelist management modal,
        the confirmation closed as usual, so users would think protection was active. Now show a toast and refetch the list.
      */
      const message = errorText(error);
      setWhitelistError(message);
      showToast({
        type: 'error',
        title: moduleT('leftovers.addToWhitelistFailed'),
        description: message,
      });
    } finally {
      setIsUpdatingWhitelist(false);
    }
  }, [moduleT, showToast, updateStats, whitelistCandidate]);

  /**
   * One-click search: hand the full path to the default search engine so users can confirm what the directory is for themselves.
   * Keep the same interaction as big-file cleanup and disk-change analysis (`openSearchUrl` + failure notice).
   * The question must include the full path — what users need to confirm is "can this whole directory be deleted", not its name.
   */
  const handleSearchLeftover = useCallback(async (path: string) => {
    try {
      await openSearchUrl(moduleT('leftovers.searchQuery', { path }));
    } catch (error) {
      console.error('搜索残留目录用途失败:', error);
      showToast({
        type: 'error',
        title: t('openSearchFailed'),
        description: errorText(error),
      });
    }
  }, [moduleT, showToast, t]);

  const handleRemoveWhitelist = useCallback(async (path: string) => {
    setIsUpdatingWhitelist(true);
    try {
      await removeLeftoverWhitelistEntry(path);
      setWhitelistEntries((entries) => entries.filter((entry) => !isSamePath(entry.path, path)));
      setWhitelistError(null);
    } catch (error) {
      setWhitelistError(errorText(error));
      // Refetch on deletion failure so the UI does not show "removed" while the file is still there.
      await loadWhitelist();
    } finally {
      setIsUpdatingWhitelist(false);
    }
  }, [loadWhitelist]);

  // Start scanning
  const handleScan = useCallback(async () => {
    updateModuleState('leftovers', { status: 'scanning', error: null });
    setScanResult(null);
    setVersionResult(null);
    setVersionError(null);
    setOtherResult(null);
    setOtherError(null);
    setSelectedPaths(new Set());
    setDeleteError(null);
    setDeleteErrors([]);
    setDeleteFailedPaths([]);
    setShowErrorDetails(false);

    /*
      Three independent backends, one click, run in parallel:
      - `scan_uninstall_leftovers` scores first-level AppData / ProgramData directories (heuristics, confidence based);
      - `scan_version_leftovers` measures version directories of the same software (exact sizes, current-version detection);
      - `scan_other_app_caches` measures the caches other software leaves behind (driver installers, launcher web caches).
      These are separate lists with separate deletion contracts, so `allSettled` is required: with `all`, one backend
      failing would throw away the other two's perfectly good results.
    */
    const [leftoverOutcome, versionOutcome, otherOutcome] = await Promise.allSettled([
      scanUninstallLeftovers(),
      scanVersionLeftovers(),
      scanOtherAppCaches(),
    ]);

    let version: VersionLeftoverScanResult | null = null;
    if (versionOutcome.status === 'fulfilled') {
      version = versionOutcome.value;
      setVersionResult(version);
    } else {
      console.error('多版本残留扫描失败:', versionOutcome.reason);
      setVersionError(String(versionOutcome.reason));
    }

    let other: DevCacheScanResult | null = null;
    if (otherOutcome.status === 'fulfilled') {
      other = otherOutcome.value;
      setOtherResult(other);
    } else {
      console.error('其它软件缓存扫描失败:', otherOutcome.reason);
      setOtherError(String(otherOutcome.reason));
    }

    if (leftoverOutcome.status === 'rejected') {
      console.error('残留&缓存扫描失败:', leftoverOutcome.reason);
      updateModuleState('leftovers', { status: 'error', error: String(leftoverOutcome.reason) });
      // The other two scans may still have succeeded; keep their numbers on the card instead of showing 0 / 0 B
      if (version || other) {
        pushStats({ count: 0, size: 0 }, version, other);
      }
      return;
    }

    const result = leftoverOutcome.value;
    setScanResult(result);

    // Such judgments are after-the-fact inferences; even high confidence can damage data still in use, so all first-version results require explicit user confirmation.
    setSelectedPaths(new Set());

    updateModuleState('leftovers', { status: 'done' });
    pushStats({ count: result.leftovers.length, size: result.total_size }, version, other);
  }, [pushStats, updateModuleState]);

  /**
   * Called by the multi-version section after old version directories are deleted.
   *
   * The section owns its list, so this only drops the removed paths from `versionResult` — otherwise deleting a version
   * would leave the card's totals stale (and the exact sizes are no longer measurable after deletion).
   */
  const handleVersionsDeleted = useCallback(
    (deletedPaths: string[]) => {
      if (!versionResult || deletedPaths.length === 0) return;
      const removed = new Set(deletedPaths);

      const groups = versionResult.groups
        .map((group) => {
          const entries = group.entries.filter((entry) => !removed.has(entry.path));
          return {
            ...group,
            entries,
            totalSize: entries.reduce((sum, entry) => sum + entry.size, 0),
            reclaimableSize: entries
              .filter((entry) => !entry.isCurrent)
              .reduce((sum, entry) => sum + entry.size, 0),
          };
        })
        .filter((group) => group.entries.length > 0);

      const next: VersionLeftoverScanResult = {
        ...versionResult,
        groups,
        totalSize: groups.reduce((sum, group) => sum + group.totalSize, 0),
        totalFiles: groups.reduce(
          (sum, group) => sum + group.entries.reduce((files, entry) => files + entry.fileCount, 0),
          0,
        ),
        reclaimableSize: groups.reduce((sum, group) => sum + group.reclaimableSize, 0),
      };
      setVersionResult(next);

      pushStats(
        { count: scanResult?.leftovers.length ?? 0, size: scanResult?.total_size ?? 0 },
        next,
        otherResult,
      );
    },
    [otherResult, pushStats, scanResult, versionResult],
  );

  /**
   * Called by the other-software-cache section after directories are deleted.
   *
   * Same contract as `handleVersionsDeleted`: the section owns its list, so this only drops the removed paths from
   * `otherResult` and rewrites the card totals (the exact sizes are no longer measurable after deletion).
   */
  const handleOtherCachesDeleted = useCallback(
    (deletedPaths: string[]) => {
      if (!otherResult || deletedPaths.length === 0) return;
      const removed = new Set(deletedPaths);

      const groups = otherResult.groups
        .map((group) => {
          const entries = group.entries.filter((entry) => !removed.has(entry.path));
          return {
            ...group,
            entries,
            totalSize: entries.reduce((sum, entry) => sum + entry.size, 0),
            fileCount: entries.reduce((sum, entry) => sum + entry.fileCount, 0),
          };
        })
        .filter((group) => group.entries.length > 0);

      const next: DevCacheScanResult = {
        ...otherResult,
        groups,
        totalSize: groups.reduce((sum, group) => sum + group.totalSize, 0),
        totalFiles: groups.reduce((sum, group) => sum + group.fileCount, 0),
        // "Directly cleanable" must be recomputed too: the deleted directories are gone, so keeping the old
        // number would promise space that is no longer there.
        safeSize: groups
          .flatMap((group) => group.entries)
          .filter((entry) => entry.risk === 'cache')
          .reduce((sum, entry) => sum + entry.size, 0),
      };
      setOtherResult(next);

      pushStats(
        { count: scanResult?.leftovers.length ?? 0, size: scanResult?.total_size ?? 0 },
        versionResult,
        next,
      );
    },
    [otherResult, pushStats, scanResult, versionResult],
  );

  // Listen for the one-click scan trigger
  useEffect(() => {
    if (oneClickScanTrigger > 0 && oneClickScanTrigger !== lastScanTriggerRef.current) {
      lastScanTriggerRef.current = oneClickScanTrigger;
      handleScan();
    }
  }, [oneClickScanTrigger, handleScan]);

  // Execute deletion
  /**
   * Remove successfully deleted paths from the list and selection, and sync the module card's statistics.
   *
   * Both normal deletion and deep cleanup need this. Centralizing it prevents the two filter logics from drifting —
   * missing one of them yields "already deleted but still checked in the list".
   */
  const applyDeletedPaths = useCallback(
    (deletedPaths: string[]) => {
      if (!scanResult || deletedPaths.length === 0) return;
      const removed = new Set(deletedPaths);
      const remaining = scanResult.leftovers.filter((item) => !removed.has(item.path));
      const totalSize = remaining.reduce((sum, item) => sum + item.size, 0);
      setScanResult({ ...scanResult, leftovers: remaining, total_size: totalSize });
      setSelectedPaths((current) => new Set(Array.from(current).filter((path) => !removed.has(path))));
      updateStats(remaining.length, totalSize);
    },
    [scanResult, updateStats],
  );

  const handleDelete = useCallback(async () => {
    if (selectedPaths.size === 0) return;

    setIsDeleting(true);
    setDeleteError(null);
    setDeleteErrors([]);
    setDeleteFailedPaths([]);
    setShowErrorDetails(false);

    try {
      const paths = Array.from(selectedPaths);
      const result = await deleteLeftoverFolders(paths);

      // Record a cleanup log (all operations are recorded)
      const failedPathSet = new Set(result.failed_paths || []);
      const logEntries: CleanupLogEntryInput[] = paths.map((path) => {
        const entry = scanResult?.leftovers.find((l) => l.path === path);
        const errorIndex = result.failed_paths?.indexOf(path);
        return {
          // The category name written into the cleanup log, shown in the log list, must match the module name
          category: '残留&缓存',
          path,
          size: entry?.size || 0,
          success: !failedPathSet.has(path),
          error_message: errorIndex !== undefined && errorIndex >= 0 ? result.errors[errorIndex] : undefined,
        };
      });
      recordCleanupAction(logEntries).catch((err) => {
        console.warn('记录清理日志失败:', err);
      });

      if (result.errors.length > 0) {
        const skippedMsg = result.skipped_executables?.length
          ? moduleT('leftoversExtra.skippedExecutables', { count: result.skipped_executables.length })
          : '';
        setDeleteError(moduleT('leftoversExtra.deleteErrorSummary', { count: result.errors.length, skipped: skippedMsg }));
        setDeleteErrors(result.errors);
        // The backend guarantees errors and failed_paths share indices, so take them by position,
        // without parsing paths out of the error text (that Chinese text can change at any time).
        setDeleteFailedPaths(result.failed_paths ?? []);
      }

      // Always give one-off feedback: skipped items only appear in the notice bar above the list,
      // which users scrolling mid-list never see, showing up as "clicked cleanup but nothing happened".
      if (result.deleted_count === 0) {
        const skippedCount = result.skipped_executables?.length ?? 0;
        showToast({
          type: 'warning',
          title: moduleT('leftoversExtra.nothingDeleted'),
          description:
            skippedCount > 0
              ? moduleT('leftoversExtra.nothingDeletedSkipped', { count: skippedCount })
              : moduleT('leftoversExtra.nothingDeletedFailed', { count: result.failed_paths.length }),
          duration: DELETE_TOAST_DURATION_MS,
        });
      } else {
        showToast({
          type: 'success',
          title: moduleT('leftoversExtra.deleteSuccess', {
            count: result.deleted_count,
            size: formatSize(result.deleted_size),
          }),
        });
      }

      // Remove deleted items from results (keep failures and items skipped for executables)
      if (scanResult) {
        const skippedSet = new Set(result.skipped_executables || []);
        const remainingLeftovers = scanResult.leftovers.filter(
          l => !selectedPaths.has(l.path)
            || result.failed_paths.includes(l.path)
            || skippedSet.has(l.path)
        );
        const newTotalSize = remainingLeftovers.reduce((sum, l) => sum + l.size, 0);

        setScanResult({
          ...scanResult,
          leftovers: remainingLeftovers,
          total_size: newTotalSize,
        });

        // Update selection state (keep only items not successfully deleted)
        const newSelected = new Set(
          Array.from(selectedPaths).filter(
            p => result.failed_paths.includes(p) || skippedSet.has(p)
          )
        );
        setSelectedPaths(newSelected);

        updateStats(remainingLeftovers.length, newTotalSize);
      }

      triggerHealthRefresh();
    } catch (err) {
      console.error('删除失败:', err);
      setDeleteError(String(err));
      setDeleteErrors([String(err)]);
      setDeleteFailedPaths([]);
      // When the command layer throws directly, previously only the inline notice above the list existed — invisible to users scrolling mid-list,
      // showing up as "click did nothing". Add a toast here, using the same feedback path as "skipped".
      showToast({
        type: 'error',
        title: moduleT('leftovers.deleteFailed'),
        description: String(err),
        duration: DELETE_TOAST_DURATION_MS,
      });
    } finally {
      setIsDeleting(false);
      setShowDeleteConfirm(false);
    }
  }, [selectedPaths, scanResult, updateStats, triggerHealthRefresh, moduleT, showToast]);

  /**
   * Paths pending forced deletion: the "needs manual review" batch from the deep cleanup result.
   * Non-empty means the second-confirmation modal should be shown.
   */
  const [forceTargets, setForceTargets] = useState<string[]>([]);

  /**
   * Forced deletion after user confirmation.
   *
   * Only bypass the "directory contains executables" check; whitelist and system-directory protection still apply —
   * this is exactly the exit for "deep cleanup stops at directories with exe files".
   */
  const handleForceDelete = useCallback(async () => {
    if (forceTargets.length === 0) return;
    const paths = forceTargets;
    setForceTargets([]);
    try {
      const result = await deleteLeftoversPermanent(paths, true);
      applyDeletedPaths(result.details.filter((item) => item.success).map((item) => item.path));
      setShowDeepCleanResult(false);
      if (result.success_count === 0) {
        showToast({
          type: 'error',
          title: moduleT('leftoversExtra.nothingDeleted'),
          description: result.details.find((item) => item.failure_reason)?.failure_reason ?? '',
          duration: DELETE_TOAST_DURATION_MS,
        });
      } else {
        showToast({
          type: 'success',
          title: moduleT('leftoversExtra.forceDeleted', {
            count: result.success_count,
            size: formatSize(result.freed_size),
          }),
        });
      }
    } catch (error) {
      showToast({
        type: 'error',
        title: moduleT('leftoversExtra.nothingDeleted'),
        description: errorText(error),
        duration: DELETE_TOAST_DURATION_MS,
      });
    }
  }, [forceTargets, applyDeletedPaths, moduleT, showToast]);

  // Deep cleanup (permanent delete) - show warning on first click
  const handleDeepCleanClick = useCallback(() => {
    if (selectedPaths.size === 0) return;
    // Show warning modal on first deep cleanup
    setShowDeepCleanWarning(true);
  }, [selectedPaths]);

  // Show final confirmation after the deep cleanup warning is confirmed
  const handleDeepCleanWarningConfirm = useCallback(() => {
    setShowDeepCleanWarning(false);
    setShowDeepCleanConfirm(true);
  }, []);

  // Execute deep cleanup (permanent delete)
  const handleDeepClean = useCallback(async () => {
    if (selectedPaths.size === 0) return;

    setIsDeleting(true);
    setDeleteError(null);
    setDeleteErrors([]);
    setDeleteFailedPaths([]);
    setShowErrorDetails(false);
    setShowDeepCleanConfirm(false);

    try {
      const paths = Array.from(selectedPaths);
      const result = await deleteLeftoversPermanent(paths);

      setDeepCleanResult(result);
      setShowDeepCleanResult(true);

      // Remove successes from the list and selection using the same logic as normal deletion, to avoid the two filter conditions drifting
      applyDeletedPaths(
        result.details.filter((item) => item.success).map((item) => item.path),
      );

      // Show error information
      if (result.failed_count > 0 || result.manual_review_count > 0) {
        const errorMessages: string[] = [];
        result.details.forEach(d => {
          if (!d.success && d.failure_reason) {
            // Prefer the machine-readable code so the message follows the UI language
            // instead of leaking the raw OS text.
            const reason = d.failure_code
              ? errorText({ code: d.failure_code, message: d.failure_reason })
              : d.failure_reason;
            errorMessages.push(`${d.path}: ${reason}`);
          }
          if (d.needs_manual_review) {
            errorMessages.push(`${d.path}: ${safetyCheckText(d.safety_check, moduleT)}`);
          }
        });
        if (errorMessages.length > 0) {
          setDeleteErrors(errorMessages);
        }
      }

      triggerHealthRefresh();
    } catch (err) {
      console.error('深度清理失败:', err);
      setDeleteError(String(err));
      setDeleteErrors([String(err)]);
      setDeleteFailedPaths([]);
      showToast({
        type: 'error',
        title: moduleT('leftovers.deleteFailed'),
        description: String(err),
        duration: DELETE_TOAST_DURATION_MS,
      });
    } finally {
      setIsDeleting(false);
    }
  }, [selectedPaths, applyDeletedPaths, triggerHealthRefresh, showToast, moduleT]);

  // Toggle selection
  const toggleSelect = useCallback((path: string) => {
    setSelectedPaths(prev => {
      const next = new Set(prev);
      if (next.has(path)) {
        next.delete(path);
      } else {
        next.add(path);
      }
      return next;
    });
  }, []);

  // Select all / deselect all
  const toggleSelectAll = useCallback(() => {
    if (!scanResult) return;
    if (selectedPaths.size === scanResult.leftovers.length) {
      setSelectedPaths(new Set());
    } else {
      setSelectedPaths(new Set(scanResult.leftovers.map(l => l.path)));
    }
  }, [scanResult, selectedPaths]);

  // Select all suspicious items
  const selectAllSuspicious = useCallback(() => {
    if (!scanResult) return;
    const suspicious = new Set(
      scanResult.leftovers
        .filter(l => l.detection_category === 'Suspicious')
        .map(l => l.path)
    );
    setSelectedPaths(suspicious);
  }, [scanResult]);

  // Get the display name for a source
  const getSourceName = (source: LeftoverEntry['source']) => {
    switch (source) {
      case 'LocalAppData': return moduleT('leftovers.source.local');
      case 'RoamingAppData': return moduleT('leftovers.source.roaming');
      case 'LocalLowAppData': return moduleT('leftovers.source.localLow');
      case 'ProgramData': return moduleT('leftovers.source.programData');
      case 'VirtualDiskFile': return moduleT('leftovers.source.virtualDisk');
      default: return source;
    }
  };

  // Count emulator and virtual disk leftovers
  const emulatorCount = useMemo(() => {
    if (!scanResult) return 0;
    return scanResult.leftovers.filter(l => l.is_emulator).length;
  }, [scanResult]);

  const virtualDiskCount = useMemo(() => {
    if (!scanResult) return 0;
    return scanResult.leftovers.filter(l => l.is_virtual_disk).length;
  }, [scanResult]);

  // Count items at each confidence level
  const highConfidenceCount = useMemo(() => {
    if (!scanResult) return 0;
    return scanResult.leftovers.filter(l => l.detection_category === 'HighConfidenceLeftover').length;
  }, [scanResult]);

  const suspiciousCount = useMemo(() => {
    if (!scanResult) return 0;
    return scanResult.leftovers.filter(l => l.detection_category === 'Suspicious').length;
  }, [scanResult]);

  /**
   * Number of updater cache entries.
   *
   * What distinguishes it from the other two tiers is "known consequences": deleting it only makes the updater re-download next time,
   * without affecting installed programs. So it is shown separately in the notice bar, using an info color rather than a warning color.
   */
  const updaterCacheCount = useMemo(() => {
    if (!scanResult) return 0;
    return scanResult.leftovers.filter(l => l.detection_category === 'UpdaterCache').length;
  }, [scanResult]);

  /**
   * Number of ordinary cache/log directories.
   *
   * Counted separately from the one above: deleting update cache means re-downloading hundreds of MB, while deleting this one just makes the program rebuild it,
   * and users' tolerance for the two is completely different; mixing them into one number makes judgment impossible.
   */
  const cacheDirCount = useMemo(() => {
    if (!scanResult) return 0;
    return scanResult.leftovers.filter(l => l.detection_category === 'CacheDir').length;
  }, [scanResult]);

  // Category summary in the notice bar. Written as an array rather than three adjacent ternary expressions:
  // adding a tier would require recomputing "where to put the separator between which entries", and that style gets messy on the third change.
  // No useMemo: comparing a few integers is cheaper than memo's dependency comparison.
  const confidenceSummary = [
    updaterCacheCount > 0 && {
      label: moduleT('leftovers.updaterCache', { count: updaterCacheCount }),
      className: 'text-[var(--color-info)]',
    },
    cacheDirCount > 0 && {
      label: moduleT('leftovers.cacheDir', { count: cacheDirCount }),
      className: 'text-[var(--color-success)]',
    },
    highConfidenceCount > 0 && {
      label: moduleT('leftovers.highConfidence', { count: highConfidenceCount }),
      className: 'text-[var(--color-danger)]',
    },
    suspiciousCount > 0 && {
      label: moduleT('leftovers.suspicious', { count: suspiciousCount }),
      className: 'text-[var(--color-warning)]',
    },
  ].filter((item): item is { label: string; className: string } => Boolean(item));

  // Confidence category labels
  const getCategoryLabel = (category: string) => {
    switch (category) {
      case 'HighConfidenceLeftover': return moduleT('leftovers.category.high_confidence');
      case 'Suspicious': return moduleT('leftovers.category.suspicious');
      case 'LikelyAppData': return moduleT('leftovers.category.possibly_in_use');
      case 'SystemShared': return moduleT('leftovers.category.system_shared');
      case 'UpdaterCache': return moduleT('leftovers.category.updater_cache');
      case 'CacheDir': return moduleT('leftovers.category.cache_dir');
      default: return category;
    }
  };

  /**
   * Integrate the AI assistant (read-only).
   *
   * **Deliberately do not register onSelect / onDelete**: deletion here is irrecoverable whole-directory deletion,
   * and must be confirmed by users item by item while seeing the name and size — the assistant only explains "what this directory is and whether it can be deleted".
   *
   * Reason for placing it here: `getSourceName` / `getCategoryLabel` / `suspiciousCount` are already defined above,
   * so the summary can use them directly instead of writing another Chinese mapping in the closure (two mappings will eventually disagree).
   */
  useAiModuleBridge('leftovers', {
    // Read-only: single-item detail. itemId is the full path at the start of a read_items row.
    readItemDetail: (itemId) => {
      const wanted = itemId.trim();
      const item = (scanResult?.leftovers ?? []).find(
        (entry) => entry.path === wanted || wanted.startsWith(entry.path),
      );
      if (!item) return null;
      return [
        `路径：${item.path}`,
        `关联应用：${item.app_name}`,
        `大小：${formatSize(item.size)}（${item.file_count} 个文件）`,
        `来源：${getSourceName(item.source)}`,
        `置信度：${Math.round(item.confidence * 100)}%（${getCategoryLabel(item.detection_category)}）`,
        `判定依据：${item.reasons.length > 0 ? item.reasons.join('；') : '无'}`,
      ].join('\n');
    },
    // Read-only paging: one page at a time so the assistant can "look further down" at what entries exist.
    readItems: (_targetId, offset, limit) => {
      const list = scanResult?.leftovers ?? [];
      if (list.length === 0) {
        // ⚠️ Do not return null: the shared layer only says "no content was read", while "not scanned yet" and "scanned but no match"
        // are completely different troubleshooting directions; the reason must be written out clearly here.
        return {
          total: 0,
          lines: [
            scanResult
              ? `（本次扫描没有发现残留或缓存：检查了 ${scanResult.scanned_directories} 个一级目录，最高置信度 ${Math.round(scanResult.highest_confidence * 100)}%，未达到 ${Math.round(scanResult.confidence_threshold * 100)}% 的判定门槛）`
              : '（当前还没有扫描结果 —— 请先点一次「重新扫描」，再问这个问题）',
          ],
        };
      }
      return {
        total: list.length,
        lines: list.slice(offset, offset + limit).map(
          (item) =>
            `${item.path}  ${formatSize(item.size)}  置信度 ${Math.round(item.confidence * 100)}%  （${item.file_count} 个文件；${item.reasons[0] ?? '无判定依据'}）`,
        ),
      };
    },
    summary: () => {
      // All three sections are reported here: the assistant answers questions about the whole page, so silently
      // omitting one would make it claim "there is nothing else" while the user sees that section on screen.
      const versionLines: string[] = [];
      if (versionResult) {
        if (versionResult.groups.length === 0) {
          versionLines.push('多版本残留：没有发现同一软件并排存放多个版本目录的情况。');
        } else {
          const described = versionResult.groups
            .slice(0, 3)
            .map((group) => {
              const current = group.entries.find((entry) => entry.isCurrent);
              return `${group.name}（${group.entries.length} 个版本，当前在用 ${
                current?.version ?? '未知'
              }，可回收 ${formatSize(group.reclaimableSize)}）`;
            })
            .join('、');
          versionLines.push(
            `多版本残留：${versionResult.groups.length} 组，共 ${formatSize(versionResult.totalSize)}，` +
              `其中可回收 ${formatSize(versionResult.reclaimableSize)}。`,
            `分别是：${described}。`,
            '这些目录只能删「旧版本」，当前在用版本不可勾选；删除走多版本区块自己的按钮，不可恢复、不进回收站。',
          );
        }
      } else if (versionError) {
        versionLines.push(`多版本残留：扫描失败（${versionError}），这部分没有数据。`);
      }

      const otherLines: string[] = [];
      if (otherResult) {
        const otherEntries = otherResult.groups.flatMap((group) => group.entries);
        if (otherEntries.length === 0) {
          otherLines.push('其它软件缓存：没有发现驱动安装包、启动器缓存这类目录。');
        } else {
          const biggest = [...otherEntries]
            .sort((left, right) => right.size - left.size)
            .slice(0, 3)
            .map((entry) => `${entry.name} ${formatSize(entry.size)}`)
            .join('、');
          const risky = otherEntries.filter((entry) => entry.risk === 'confirm').length;
          otherLines.push(
            `其它软件缓存：${otherEntries.length} 个目录，共 ${formatSize(otherResult.totalSize)}，` +
              `其中可直接清理 ${formatSize(otherResult.safeSize)}${risky > 0 ? `，另有 ${risky} 项标着「会丢数据」` : ''}。`,
            `最大的三个：${biggest}。`,
            '这些是垃圾清理与驱动清理都不处理的目录（驱动安装包、启动器网页缓存等），删除同样不可恢复、不进回收站。',
          );
        }
      } else if (otherError) {
        otherLines.push(`其它软件缓存：扫描失败（${otherError}），这部分没有数据。`);
      }

      if (!scanResult) {
        return versionLines.length + otherLines.length > 0
          ? [...versionLines, ...otherLines].join('')
          : null;
      }
      if (scanResult.leftovers.length === 0) {
        return [
          `刚扫描完，没有发现可信度足够的残留：检查了 ${scanResult.scanned_directories} 个一级目录，`,
          `最高置信度 ${Math.round(scanResult.highest_confidence * 100)}%，判定门槛 ${Math.round(scanResult.confidence_threshold * 100)}%。`,
          ...versionLines,
          ...otherLines,
        ].join('');
      }
      const topThree = [...scanResult.leftovers]
        .sort((left, right) => right.size - left.size)
        .slice(0, 3)
        .map((item) => `${item.app_name} ${formatSize(item.size)}`)
        .join('、');
      // The summary must include the distribution: that is exactly what the assistant uses to judge "whether to recommend deletion",
      // stating only a total would make it treat all entries as equally trustworthy
      const distribution = [
        updaterCacheCount > 0 ? `更新缓存 ${updaterCacheCount} 项` : '',
        cacheDirCount > 0 ? `缓存/日志 ${cacheDirCount} 项` : '',
        highConfidenceCount > 0 ? `高置信度 ${highConfidenceCount} 项` : '',
        suspiciousCount > 0 ? `可疑 ${suspiciousCount} 项` : '',
      ]
        .filter(Boolean)
        .join('、');
      return [
        `扫描到 ${scanResult.leftovers.length} 项残留或缓存，合计 ${formatSize(scanResult.total_size)}`,
        `（${distribution}），当前勾选 ${selectedPaths.size} 项。`,
        `最大的三个：${topThree}。`,
        `口径：只判断 AppData / ProgramData 的一级目录，子目录不单独统计；置信度是启发式评分，不保证准确。`,
        `「更新缓存」这一类删掉只会让更新器下次重新下载，不影响已安装的程序；其余条目没有这个保证。`,
        `含 .exe/.dll/.sys 的目录用「删除选中」会被跳过，只能用「深度清理」；删除不可恢复。`,
        ...versionLines,
        ...otherLines,
      ].join('');
    },
  });

  /**
   * Colors for category labels.
   *
   * All use theme variables (`--color-*` / `--brand-green`), **do not use `dark:`**
   * —— in this project `dark:` is bound to the system light/dark mode, not the app theme.
   * Each label is a pill with "same-color stroke + 15% background + same-color text", more eye-catching than a plain faint background:
   * previously "update cache" used `--color-info`, which was not defined, so the label inherited a color and looked like ordinary text.
   */
  const getCategoryColor = (category: string) => {
    switch (category) {
      case 'HighConfidenceLeftover':
        return 'border-[var(--color-danger)]/35 bg-[var(--color-danger)]/15 text-[var(--color-danger)]';
      case 'Suspicious':
        return 'border-[var(--color-warning)]/35 bg-[var(--color-warning)]/15 text-[var(--color-warning)]';
      // Rebuildable, but at different costs: update cache must be re-downloaded, ordinary cache is rebuilt by the program itself
      case 'UpdaterCache':
        return 'border-[var(--color-info)]/35 bg-[var(--color-info)]/15 text-[var(--color-info)]';
      case 'CacheDir':
        return 'border-[var(--color-success)]/35 bg-[var(--color-success)]/15 text-[var(--color-success)]';
      case 'LikelyAppData':
        return 'border-[var(--brand-green)]/35 bg-[var(--brand-green-10)] text-[var(--brand-green)]';
      case 'SystemShared':
      default:
        return 'border-[var(--border-default)] bg-[var(--bg-hover)] text-[var(--text-muted)]';
    }
  };
  /**
   * Whether the bottom action bar is visible.
   *
   * ⚠️ Cannot be omitted: `ModuleOperationToolbar` is a fixed overlay portaled to body,
   * not constrained by the module container — without this toggle it would keep floating over other modules' pages.
   */
  const shouldShowOperationToolbar = isPageActive;

  if (!isPageActive && !isDeletingAnimating) {
    return null;
  }

  return (
    <>
      {/* Deletion progress overlay - rendered via Portal to body to ensure full-screen coverage */}
      {isDeletingAnimating && createPortal(
        <div className="fixed inset-0 z-[9999] flex items-center justify-center">
          <div className={`absolute inset-0 bg-black/50 backdrop-blur-sm glass-backdrop ${isDeletingVisible ? 'modal-overlay-in' : deletingEnteredRef.current ? 'modal-overlay-out' : 'opacity-0'}`} />
          {/* Size uses em to follow the global font size; the 20em cap corresponds to the old max-w-sm.
              The content is short, but add a viewport constraint too, to avoid overflow in short windows when the font size is increased. */}
          <div className={`relative bg-[var(--bg-card)] rounded-2xl p-[2.29em] glass-dialog shadow-2xl flex flex-col items-center gap-[1.14em] max-w-[20em] mx-[1em] max-h-[calc(100vh-2em)] overflow-y-auto ${isDeletingVisible ? 'modal-content-in' : deletingEnteredRef.current ? 'modal-content-out' : 'opacity-0'}`}>
            <div className="w-[4.57em] h-[4.57em] rounded-full bg-[var(--color-warning)]/10 flex items-center justify-center">
              <Loader2 className="w-[2.29em] h-[2.29em] text-[var(--color-warning)] animate-spin" />
            </div>
            <div className="text-center">
        <h3 className="text-[1.29em] font-semibold text-[var(--text-primary)]">{moduleT('leftovers.deleting')}</h3>
              <p className="text-[1em] text-[var(--text-muted)] mt-[0.29em]">
                {moduleT('leftovers.deletingFolder')} ({selectedPaths.size})...
              </p>
            </div>
            <div className="w-full h-[0.57em] bg-[var(--bg-hover)] rounded-full overflow-hidden">
              <div className="h-full bg-[var(--color-warning)] rounded-full animate-pulse" style={{ width: '100%' }} />
            </div>
            <p className="text-[0.86em] text-[var(--text-faint)]">{moduleT('leftovers.doNotClose')}</p>
          </div>
        </div>,
        document.body
      )}

      <ModuleCard
        id="leftovers"
        title={navT('leftovers')}
        description={navT('leftoversDesc')}
        icon={<Package className="w-6 h-6 text-[var(--brand-green)]" />}
        status={moduleState.status}
        fileCount={moduleState.fileCount}
        totalSize={moduleState.totalSize}
        onScan={handleScan}
        error={moduleState.error}
        headerExtra={
          <button
            onClick={() => {
              setShowWhitelistManager(true);
              void loadWhitelist();
            }}
            className="p-2 rounded-lg text-[var(--text-muted)] hover:text-[var(--brand-green)] hover:bg-[var(--bg-hover)] transition-colors"
            title={moduleT('leftovers.whitelistManage')}
          >
            <ShieldCheck className="w-4 h-4" />
          </button>
        }
      >
        {moduleState.status === 'idle' && !scanResult && (
          <div className="p-5">
            <EmptyState
              icon={Package}
              title={t('notScannedLeftovers')}
              description={moduleT('leftovers.idleDesc')}
            />
          </div>
        )}

        {moduleState.status === 'scanning' && !scanResult && (
          <div className="p-5">
            <ModuleScanProgress
              title={moduleT('leftovers.scanning')}
              description={moduleT('leftovers.scanningDesc')}
              icon={<Loader2 className="h-7 w-7 animate-spin text-[var(--brand-green)]" />}
            />
          </div>
        )}

        {/* Scan result content: the leftover list and the two dev-space sections share one column,
            but each is rendered/hidden independently (they come from three different backends) */}
        {(scanResult || versionResult || versionError || otherResult || otherError) && (
          // pb-20: the bottom action bar is a fixed floating layer outside document flow; leave trailing space so it does not cover the last entry
          <div className="p-5 space-y-4 pb-20">
            {/* Nothing found on the AppData side: keep the diagnostic (it is the proof the scan actually ran)
                but compress it, because the multi-version section below still needs the vertical space */}
            {scanResult && scanResult.leftovers.length === 0 && (
              <div className="flex items-start gap-3 rounded-xl border border-[var(--color-success)]/20 bg-[var(--color-success)]/10 px-4 py-3">
                <CheckCircle2 className="mt-0.5 h-4 w-4 shrink-0 text-[var(--color-success)]" />
                <div>
                  <p className="text-[0.92rem] font-medium text-[var(--text-primary)]">
                    {t('noLeftovers')}
                  </p>
                  {/* This module routinely returns 0 entries; users cannot distinguish "really no leftovers" from "scan is broken".
                      Report the number of directories checked, the highest score, and the threshold — evidence that "it is running". */}
                  <p className="mt-0.5 text-[0.82rem] text-[var(--text-muted)]">
                    {scanResult.scanned_directories > 0
                      ? moduleT('leftovers.noResultDiagnostic', {
                          scanned: scanResult.scanned_directories,
                          scored: scanResult.below_threshold,
                          score: Math.round(scanResult.highest_confidence * 100),
                          threshold: Math.round(scanResult.confidence_threshold * 100),
                        })
                      : moduleT('leftovers.noResultDesc')}
                  </p>
                </div>
              </div>
            )}

            {scanResult && scanResult.leftovers.length > 0 && (
              <>
            {/* Risk notice + confidence statistics */}
            <div className="flex items-start gap-3 p-4 bg-[var(--color-warning)]/10 rounded-xl border border-[var(--color-warning)]/20">
              <AlertTriangle className="w-5 h-5 text-[var(--color-warning)] shrink-0 mt-0.5" />
              <div>
                <p className="text-sm font-medium text-[var(--text-primary)]">{moduleT('leftovers.confidenceTitle')}</p>
                <p className="text-xs text-[var(--text-muted)] mt-1">
                  {moduleT('leftovers.confidenceDesc')}
                  {confidenceSummary.map((item, index) => (
                    <span key={item.label}>
                      <span className={`font-medium ${item.className}`}>{item.label}</span>
                      {index < confidenceSummary.length - 1 && ' · '}
                    </span>
                  ))}
                  {` ${moduleT('leftovers.accuracy')}`}
                </p>
              </div>
            </div>

            {/* Action bar reuses the shared component: select all / select suspicious / delete / deep clean are all here.
                It portals to body and is fixed at the bottom of the viewport, staying visible while scrolling —
                previously this row was inside the scroll area and disappeared after scrolling two screens down. */}
            {!isDeleting && shouldShowOperationToolbar && (
              <ModuleOperationToolbar moduleId="leftovers">
                <button
                  type="button"
                  onClick={toggleSelectAll}
                  className="module-operation-toolbar__button module-operation-toolbar__button--muted"
                >
                  {selectedPaths.size === scanResult.leftovers.length ? t('deselectAll') : t('selectAll')}
                </button>
                {suspiciousCount > 0 && (
                  <button
                    type="button"
                    onClick={selectAllSuspicious}
                    className="module-operation-toolbar__button module-operation-toolbar__button--muted"
                  >
                    {moduleT('leftovers.selectSuspicious')}
                  </button>
                )}
                <span className="whitespace-nowrap px-1 text-xs tabular-nums text-[var(--text-muted)]">
                  {moduleT('leftovers.selected', { count: selectedPaths.size, size: formatSize(selectedSize) })}
                </span>
                {/* Normal deletion skips directories containing executables; use orange: irreversible, but not the most severe tier */}
                <button
                  type="button"
                  onClick={() => setShowDeleteConfirm(true)}
                  disabled={selectedPaths.size === 0 || isDeleting}
                  className="module-operation-toolbar__button module-operation-toolbar__button--warning"
                >
                  <Trash2 className="h-3.5 w-3.5" />
                  {moduleT('leftovers.deleteSelected')}
                </button>
                {/* Deep cleanup is permanent deletion; red is reserved for the most severe tier */}
                <button
                  type="button"
                  onClick={handleDeepCleanClick}
                  disabled={selectedPaths.size === 0 || isDeleting}
                  title={t('permanentDeleteWarning')}
                  className="module-operation-toolbar__button module-operation-toolbar__button--danger"
                >
                  <Trash2 className="h-3.5 w-3.5" />
                  {moduleT('leftovers.deepClean')}
                </button>
              </ModuleOperationToolbar>
            )}

            {/* Error notices */}
            {deleteError && (
              <div className="p-3 bg-[var(--color-danger)]/10 rounded-xl border border-[var(--color-danger)]/20">
                <div className="flex items-center justify-between">
                  <span className="text-sm text-[var(--color-danger)]">{deleteError}</span>
                  {deleteErrors.length > 0 && (
                    <button
                      onClick={() => setShowErrorDetails(!showErrorDetails)}
                      className="text-xs text-[var(--color-danger)] hover:underline"
                    >
                      {showErrorDetails ? moduleT('leftovers.collapseDetails') : moduleT('leftovers.viewDetails')}
                    </button>
                  )}
                </div>
                {showErrorDetails && deleteErrors.length > 0 && (
                  <div className="mt-2 pt-2 border-t border-[var(--color-danger)]/20 space-y-1 max-h-32 overflow-auto">
                    {deleteErrors.map((err, idx) => (
                      <div key={`${idx}-${err}`} className="flex items-start gap-2">
                        <p className="min-w-0 flex-1 text-xs text-[var(--color-danger)]/80 break-all">
                          • {err}
                        </p>
                        {/* Most failures are the "skipped because they contain executables" batch: must allow opening the directory in one click,
                            otherwise users can only follow the path in the error text and hunt for it in Explorer themselves. */}
                        {deleteFailedPaths[idx] && (
                          <button
                            type="button"
                            onClick={() => openInFolder(deleteFailedPaths[idx])}
                            title={t('openInFolder')}
                            className="shrink-0 rounded p-0.5 text-[var(--color-danger)]/70 transition-colors hover:bg-[var(--color-danger)]/10 hover:text-[var(--color-danger)]"
                          >
                            <FolderOpen className="h-3.5 w-3.5" />
                          </button>
                        )}
                      </div>
                    ))}
                  </div>
                )}
              </div>
            )}

            {/* Emulator/virtual disk leftover notice */}
            {(emulatorCount > 0 || virtualDiskCount > 0) && (
              <div className="flex items-start gap-3 p-4 bg-[var(--color-danger)]/10 rounded-xl border border-[var(--color-danger)]/20">
                <Smartphone className="w-5 h-5 text-[var(--color-danger)] shrink-0 mt-0.5" />
                <div>
                  <p className="text-sm font-medium text-[var(--text-primary)]">{moduleT('leftovers.emulatorTitle')}</p>
                  <p className="text-xs text-[var(--text-muted)] mt-1">
                    {moduleT('leftovers.emulatorDesc', {
                      emulator: emulatorCount > 0 ? moduleT('leftovers.emulatorItem', { count: emulatorCount }) : '',
                      separator: emulatorCount > 0 && virtualDiskCount > 0 ? '、' : '',
                      virtualDisk: virtualDiskCount > 0 ? moduleT('leftovers.virtualDiskItem', { count: virtualDiskCount }) : '',
                    })}
                  </p>
                </div>
              </div>
            )}

            {/* Leftover list */}
            <div className="space-y-2">
              {scanResult.leftovers.map((leftover) => (
                <div
                  key={leftover.path}
                  className={`
                    flex items-center gap-4 p-4 rounded-xl cursor-pointer transition-colors
                    ${leftover.is_emulator || leftover.is_virtual_disk
                      ? 'border-2 border-[var(--color-danger)]/30'
                      : ''
                    }
                    ${selectedPaths.has(leftover.path)
                      ? 'bg-[var(--brand-green-10)]'
                      : 'bg-[var(--bg-main)] hover:bg-[var(--bg-hover)]'
                    }
                  `}
                  onClick={() => toggleSelect(leftover.path)}
                >
                  {/* Checkbox */}
                  <div className={`
                    w-5 h-5 rounded border-2 flex items-center justify-center shrink-0
                    ${selectedPaths.has(leftover.path)
                      ? 'bg-[var(--brand-green)] border-[var(--brand-green)]'
                      : 'border-[var(--text-faint)]'
                    }
                  `}>
                    {selectedPaths.has(leftover.path) && (
                      <svg className="w-3 h-3 text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={3}>
                        <path strokeLinecap="round" strokeLinejoin="round" d="M5 13l4 4L19 7" />
                      </svg>
                    )}
                  </div>

                  {/* Icon - different icon by type */}
                  <div className={`w-10 h-10 rounded-xl flex items-center justify-center shrink-0 ${leftover.is_emulator
                      ? 'bg-[var(--color-danger)]/10'
                      : leftover.is_virtual_disk
                        ? 'bg-purple-500/10'
                        : 'bg-[var(--brand-green-10)]'
                    }`}>
                    {leftover.is_emulator ? (
                      <Smartphone className="w-5 h-5 text-[var(--color-danger)]" />
                    ) : leftover.is_virtual_disk ? (
                      <HardDrive className="w-5 h-5 text-purple-500" />
                    ) : (
                      <Package className="w-5 h-5 text-[var(--brand-green)]" />
                    )}
                  </div>

                  {/* Info */}
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                      <p className="text-sm font-medium text-[var(--text-primary)] truncate">
                        {leftover.app_name}
                      </p>
                      {/* Confidence category label */}
                      <span className={`flex-shrink-0 inline-flex items-center rounded-full border px-1.5 py-0.5 text-[0.71rem] font-medium ${getCategoryColor(leftover.detection_category)}`}>
                        {getCategoryLabel(leftover.detection_category)}
                      </span>
                      {/* Emulator/virtual disk label */}
                      {leftover.is_emulator && (
                        <span className="flex-shrink-0 text-[0.71rem] px-1.5 py-0.5 rounded text-[var(--color-danger)] bg-[var(--color-danger)]/10">
                          {moduleT('leftovers.emulatorTag')}
                        </span>
                      )}
                      {leftover.is_virtual_disk && (
                        <span className="flex-shrink-0 text-[0.71rem] px-1.5 py-0.5 rounded text-purple-500 bg-purple-500/10">
                          {moduleT('leftovers.virtualDiskTag')}
                        </span>
                      )}
                    </div>
                    <p className="text-xs text-[var(--text-muted)] truncate mt-0.5" title={leftover.path}>
                      {leftover.path}
                    </p>
                    <div className="flex items-center gap-3 mt-1 text-xs text-[var(--text-faint)]">
                      <span>{getSourceName(leftover.source)}</span>
                      <span>{leftover.file_count} {moduleT('leftovers.files')}</span>
                      {/* The update-cache tier does not show a percentage:
                          its score is only for sorting (deletion consequences are unrelated to "how long since last modified"),
                          showing 40% and 75% under the same label would only make people think the confidence differs a lot. */}
                      {leftover.detection_category !== 'UpdaterCache' && (
                        <span title={leftover.reasons.join('\n')}>{moduleT('leftovers.confidence', { percent: Math.round(leftover.confidence * 100) })}</span>
                      )}
                    </div>
                  </div>

                  {/* Size - highlight big files */}
                  <div className="text-right shrink-0">
                    <p className={`text-sm font-bold tabular-nums ${leftover.size > 1024 * 1024 * 1024
                        ? 'text-[var(--color-danger)]'
                        : leftover.size > 100 * 1024 * 1024
                          ? 'text-[var(--color-warning)]'
                          : 'text-[var(--text-primary)]'
                      }`}>
                      {formatSize(leftover.size)}
                    </p>
                  </div>

                  {/* Quick question: the prompt is a full sentence, not search-engine keywords (see aiQuestions docs).
                      Do not set skipLookup — the assistant must first read this module's data to know how big the directory is and its confidence. */}
                  <AiAskButton
                    question={buildItemQuestion(leftover.path, 'folder')}
                    className="inline-flex h-8 w-8 items-center justify-center"
                  />

                  {/* One-click search: placed before "Open folder" following the big-file cleanup pattern —
                      first confirm what this directory is and whether it can be deleted, then decide whether to open and look. */}
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      handleSearchLeftover(leftover.path);
                    }}
                    className="p-2 rounded-lg text-[var(--text-muted)] hover:text-[var(--brand-green)] hover:bg-[var(--bg-hover)] transition-colors"
                    title={t('searchDeletionAdvice')}
                  >
                    <Search className="w-4 h-4" />
                  </button>

                  {/* Open folder button */}
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      openInFolder(leftover.path);
                    }}
                    className="p-2 rounded-lg text-[var(--text-muted)] hover:text-[var(--brand-green)] hover:bg-[var(--bg-hover)] transition-colors"
              title={t('openInFolder')}
                  >
                    <FolderOpen className="w-4 h-4" />
                  </button>
                  <button
                    onClick={(event) => {
                      event.stopPropagation();
                      setWhitelistCandidate(leftover);
                    }}
                    className="p-2 rounded-lg text-[var(--text-muted)] hover:text-[var(--brand-green)] hover:bg-[var(--bg-hover)] transition-colors"
                    title={moduleT('leftovers.addToWhitelist')}
                  >
                    <ShieldPlus className="w-4 h-4" />
                  </button>
                </div>
              ))}
            </div>
              </>
            )}

            {/* Multi-version leftovers (moved here from the dev space module): old version directories of the same software */}
            {versionError && (
              <div className="flex items-start gap-3 rounded-xl border border-[var(--color-warning)]/20 bg-[var(--color-warning)]/10 px-4 py-3">
                <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-[var(--color-warning)]" />
                <div>
                  <p className="text-[0.92rem] font-medium text-[var(--text-primary)]">
                    {moduleT('leftoversVersion.scanFailedTitle')}
                  </p>
                  <p className="mt-0.5 break-all text-[0.82rem] text-[var(--text-muted)]">
                    {versionError}
                  </p>
                </div>
              </div>
            )}

            {versionResult && (
              <VersionLeftoverSection
                groups={versionResult.groups}
                totalSize={versionResult.totalSize}
                reclaimableSize={versionResult.reclaimableSize}
                skipped={versionResult.skipped}
                onDeleted={handleVersionsDeleted}
              />
            )}

            {/* Other software caches (moved here from the dev space module): driver installers, launcher web caches */}
            {otherError && (
              <div className="flex items-start gap-3 rounded-xl border border-[var(--color-warning)]/20 bg-[var(--color-warning)]/10 px-4 py-3">
                <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-[var(--color-warning)]" />
                <div>
                  <p className="text-[0.92rem] font-medium text-[var(--text-primary)]">
                    {moduleT('leftoversOtherCache.scanFailedTitle')}
                  </p>
                  <p className="mt-0.5 break-all text-[0.82rem] text-[var(--text-muted)]">
                    {otherError}
                  </p>
                </div>
              </div>
            )}

            {otherResult && (
              <OtherAppCacheSection
                groups={otherResult.groups}
                totalSize={otherResult.totalSize}
                safeSize={otherResult.safeSize}
                skipped={otherResult.skipped}
                onDeleted={handleOtherCachesDeleted}
              />
            )}
          </div>
        )}
      </ModuleCard>

      {/* Delete confirmation dialog */}
      <ConfirmDialog
        isOpen={showDeleteConfirm}
        onCancel={() => setShowDeleteConfirm(false)}
        onConfirm={handleDelete}
        title={t('confirmDeleteLeftovers')}
        description={moduleT('leftovers.confirmDesc', { count: selectedPaths.size, size: formatSize(selectedSize) })}
        warning={t('leftoversDeleteWarning')}
        confirmText={t('delete')}
        cancelText={t('cancel')}
        isDanger={true}
      />

      <ConfirmDialog
        isOpen={whitelistCandidate !== null}
        onCancel={() => setWhitelistCandidate(null)}
        onConfirm={handleAddToWhitelist}
        title={moduleT('leftovers.addToWhitelist')}
        description={moduleT('leftovers.whitelistConfirmDesc', { path: whitelistCandidate?.path ?? '' })}
        warning={moduleT('leftovers.whitelistConfirmWarning')}
        confirmText={moduleT('leftovers.addToWhitelist')}
        cancelText={t('cancel')}
      />

      {/* The whitelist modal handles its own enter/exit with AnimatePresence, so it stays mounted here and is controlled by isOpen */}
      <LeftoverWhitelistModal
        isOpen={showWhitelistManager}
        entries={whitelistEntries}
        error={whitelistError}
        isUpdating={isUpdatingWhitelist}
        onClose={() => setShowWhitelistManager(false)}
        onOpen={openInFolder}
        onRemove={handleRemoveWhitelist}
        t={moduleT}
        commonT={t}
      />

      {/* Deep cleanup warning modal - WeChat style */}
      {isWarningAnimating && createPortal(
        <div className="fixed inset-0 z-[9999] flex items-center justify-center">
          <div className={`absolute inset-0 bg-black/50 backdrop-blur-sm glass-backdrop ${isWarningVisible ? 'modal-overlay-in' : warningEnteredRef.current ? 'modal-overlay-out' : 'opacity-0'}`} onClick={() => setShowDeepCleanWarning(false)} />
          {/* Size uses em to follow the global font size; the 25em cap corresponds to the old max-w-md.
              Add a viewport height constraint to avoid the title and buttons being pushed out of view in short windows. */}
          <div className={`relative bg-[var(--bg-card)] rounded-2xl p-[1.43em] glass-dialog shadow-2xl max-w-[25em] mx-[1em] max-h-[calc(100vh-2em)] overflow-y-auto ${isWarningVisible ? 'modal-content-in' : warningEnteredRef.current ? 'modal-content-out' : 'opacity-0'}`}>
            {/* Warning icon */}
            <div className="flex justify-center mb-[1.14em]">
              <div className="w-[4.57em] h-[4.57em] rounded-full bg-[var(--color-danger)]/10 flex items-center justify-center">
                <AlertTriangle className="w-[2.29em] h-[2.29em] text-[var(--color-danger)]" />
              </div>
            </div>

            {/* Title */}
            <h3 className="text-[1.29em] font-bold text-[var(--text-primary)] text-center mb-[0.86em]">
              {moduleT('leftovers.deepWarningTitle')}
            </h3>

            {/* Content */}
            <div className="space-y-[0.86em] mb-[1.71em]">
              <p className="text-[1em] text-[var(--text-secondary)] text-center">
                {moduleT('leftovers.deepWarningDesc')}
              </p>
              <div className="bg-[var(--color-warning)]/10 rounded-xl p-[1.14em] border border-[var(--color-warning)]/20">
                <p className="text-[0.86em] text-[var(--text-muted)]">
                  <span className="font-semibold text-[var(--color-warning)]">{moduleT('leftovers.safetyTitle')}</span>
                  {moduleT('leftovers.safetyDesc')}
                </p>
              </div>
            </div>

            {/* Buttons */}
            <div className="flex gap-[0.86em]">
              <button
                onClick={() => setShowDeepCleanWarning(false)}
                className="flex-1 px-[1.14em] py-[0.86em] rounded-xl text-[1em] font-medium bg-[var(--bg-hover)] text-[var(--text-primary)] hover:bg-[var(--bg-active)] transition-colors"
              >
                {t('cancel')}
              </button>
              <button
                onClick={handleDeepCleanWarningConfirm}
                className="flex-1 px-[1.14em] py-[0.86em] rounded-xl text-[1em] font-medium bg-[var(--color-danger)] text-white hover:opacity-90 transition-colors"
              >
                {moduleT('leftovers.understood')}
              </button>
            </div>
          </div>
        </div>,
        document.body
      )}

      {/* Deep cleanup final confirmation modal */}
      <ConfirmDialog
        isOpen={showDeepCleanConfirm}
        onCancel={() => setShowDeepCleanConfirm(false)}
        onConfirm={handleDeepClean}
        title={t('confirmPermanentCleanup')}
        description={moduleT('leftovers.confirmDesc', { count: selectedPaths.size, size: formatSize(selectedSize) })}
        warning={t('permanentCleanupWarning')}
        confirmText={t('permanentDelete')}
        cancelText={t('cancel')}
        isDanger={true}
      />

      {/* Forced-delete confirmation: launched from the "needs manual review" section of the result modal, only reached after explicit user choice */}
      <ConfirmDialog
        isOpen={forceTargets.length > 0}
        onCancel={() => setForceTargets([])}
        onConfirm={handleForceDelete}
        title={moduleT('leftoversExtra.forceDeleteTitle')}
        description={moduleT('leftoversExtra.forceDeleteDesc')}
        warning={moduleT('leftoversExtra.forceDeleteWarning')}
        confirmText={t('permanentDelete')}
        cancelText={t('cancel')}
        isDanger={true}
      />

      {/* Deep cleanup result modal */}
      {isResultAnimating && deepCleanResult && createPortal(
        <DeepCleanResultModal
          result={deepCleanResult}
          isVisible={isResultVisible}
          hasEntered={resultEnteredRef.current}
          onClose={() => setShowDeepCleanResult(false)}
          onForceDelete={setForceTargets}
        />,
        document.body
      )}
    </>
  );
}

export default LeftoversModule;

// ============================================================================
// Deep cleanup result modal component
// ============================================================================

interface DeepCleanResultModalProps {
  result: PermanentDeleteResult;
  isVisible: boolean;
  hasEntered: boolean;
  onClose: () => void;
  /** Called when the user chooses to force-delete the "needs manual review" batch; the confirmation modal is handled by the parent */
  onForceDelete?: (paths: string[]) => void;
}

function DeepCleanResultModal({
  result,
  isVisible,
  hasEntered,
  onClose,
  onForceDelete,
}: DeepCleanResultModalProps) {
  const { t } = useTranslation('common');
  const { t: moduleT } = useTranslation('modules');
  const [expandedSection, setExpandedSection] = useState<'review' | 'failed' | null>(null);

  // Get items needing review
  const reviewItems = result.details.filter(d => d.needs_manual_review);
  // Get failed items
  const failedItems = result.details.filter(d => !d.success && !d.needs_manual_review && !d.marked_for_reboot);

  // Whether to show "force delete" at the bottom: there are review items and the parent provided a handler
  const canForceDelete = Boolean(onForceDelete) && reviewItems.length > 0;

  // Get a friendly description of the failure reason
  const getFailureReason = (detail: typeof result.details[0]): string => {
    // The backend sends a code now; switch on it instead of pattern-matching the
    // OS text (that used to break silently whenever the wording changed).
    if (detail.failure_code) {
      return errorText({ code: detail.failure_code, message: detail.failure_reason ?? '' });
    }
    if (detail.failure_reason) {
      return detail.failure_reason;
    }
    return safetyCheckText(detail.safety_check, moduleT);
  };

  // Get the folder name
  const getFolderName = (path: string): string => {
    const parts = path.replace(/\\/g, '/').split('/');
    return parts[parts.length - 1] || path;
  };

  return (
    <div className="fixed inset-0 z-[9999] flex items-center justify-center">
      <div
        className={`absolute inset-0 bg-black/50 backdrop-blur-sm glass-backdrop ${isVisible ? 'modal-overlay-in' : hasEntered ? 'modal-overlay-out' : 'opacity-0'}`}
        onClick={onClose}
      />
      {/* Sizes uniformly use em: body inherits the :root font size, so the modal scales with the global font-size setting, 30em ≈ 420px.
          Add a viewport height constraint: with only 57.14em, windows shorter than 832px overflow vertically and the header/footer get cut off. */}
      <div className={`relative bg-[var(--bg-card)] rounded-2xl p-[1.43em] glass-dialog shadow-2xl w-[30em] max-h-[min(57.14em,calc(100vh_-_2em))] overflow-hidden flex flex-col mx-[1em] ${isVisible ? 'modal-content-in' : hasEntered ? 'modal-content-out' : 'opacity-0'}`}>
        {/* Result icon */}
        <div className="flex justify-center mb-[1.14em]">
          <div className={`w-[4.57em] h-[4.57em] rounded-full flex items-center justify-center ${result.success_count > 0
              ? 'bg-[var(--brand-green)]/10'
              : 'bg-[var(--color-danger)]/10'
            }`}>
            {result.success_count > 0 ? (
              <CheckCircle2 className="w-[2.29em] h-[2.29em] text-[var(--brand-green)]" />
            ) : (
              <AlertTriangle className="w-[2.29em] h-[2.29em] text-[var(--color-danger)]" />
            )}
          </div>
        </div>

        {/* Title */}
        <h3 className="text-[1.29em] font-bold text-[var(--text-primary)] text-center mb-[1.14em]">
          {moduleT('leftovers.completed')}
        </h3>

        {/* Statistics - scrollable area */}
        <div className="flex-1 overflow-auto space-y-[0.86em] mb-[1.14em]">
          {/* Successfully deleted */}
          {result.success_count > 0 && (
            <div className="flex items-center justify-between p-[0.86em] bg-[var(--brand-green)]/10 rounded-xl">
              <span className="text-[1em] text-[var(--text-secondary)]">{moduleT('leftovers.successDeleted')}</span>
              <span className="text-[1em] font-bold text-[var(--brand-green)]">
                {moduleT('leftoversExtra.successSummary', { count: result.success_count, size: formatSize(result.freed_size) })}
              </span>
            </div>
          )}

          {/* Needs manual review - expandable */}
          {reviewItems.length > 0 && (
            <div className="bg-[var(--color-warning)]/10 rounded-xl overflow-hidden">
              <button
                onClick={() => setExpandedSection(expandedSection === 'review' ? null : 'review')}
                className="w-full flex items-center justify-between p-[0.86em] hover:bg-[var(--color-warning)]/5 transition-colors"
              >
                <span className="text-[1em] text-[var(--text-secondary)]">{moduleT('leftovers.manualReview')}</span>
                <div className="flex items-center gap-[0.57em]">
                  <span className="text-[1em] font-bold text-[var(--color-warning)]">
                    {moduleT('leftoversExtra.count', { count: reviewItems.length })}
                  </span>
                  {expandedSection === 'review' ? (
                    <ChevronUp className="w-[1.14em] h-[1.14em] text-[var(--color-warning)]" />
                  ) : (
                    <ChevronDown className="w-[1.14em] h-[1.14em] text-[var(--color-warning)]" />
                  )}
                </div>
              </button>
              {expandedSection === 'review' && (
                <div className="px-[0.86em] pb-[0.86em] space-y-[0.57em]">
                  <p className="text-[0.86em] text-[var(--text-muted)] mb-[0.57em]">
                    {moduleT('leftovers.manualReviewDesc')}
                  </p>
                  {reviewItems.map((item, idx) => (
                    <div key={idx} className="glass-block flex items-center justify-between gap-[0.57em] p-[0.57em] bg-[var(--bg-card)] rounded-lg">
                      <div className="flex-1 min-w-0">
                        <p className="text-[0.86em] font-medium text-[var(--text-primary)] truncate" title={item.path}>
                          {getFolderName(item.path)}
                        </p>
                        <p className="text-[0.71em] text-[var(--text-muted)] truncate" title={item.path}>
                          {item.path}
                        </p>
                      </div>
                      <button
                        onClick={() => openInFolder(item.path)}
                        className="shrink-0 p-[0.43em] rounded-lg text-[var(--text-muted)] hover:text-[var(--brand-green)] hover:bg-[var(--bg-hover)] transition-colors"
                    title={t('openInFolder')}
                      >
                        <FolderOpen className="w-[1.14em] h-[1.14em]" />
                      </button>
                    </div>
                  ))}
                </div>
              )}
            </div>
          )}

          {/* Pending delete on reboot */}
          {result.reboot_pending_count > 0 && (
            <div className="flex items-center justify-between p-[0.86em] bg-[var(--color-info)]/10 rounded-xl">
              <span className="text-[1em] text-[var(--text-secondary)]">{moduleT('leftovers.pendingReboot')}</span>
              <span className="text-[1em] font-bold text-[var(--color-info)]">
                {moduleT('leftoversExtra.count', { count: result.reboot_pending_count })}
              </span>
            </div>
          )}

          {/* Deletion failed - expandable */}
          {failedItems.length > 0 && (
            <div className="bg-[var(--color-danger)]/10 rounded-xl overflow-hidden">
              <button
                onClick={() => setExpandedSection(expandedSection === 'failed' ? null : 'failed')}
                className="w-full flex items-center justify-between p-[0.86em] hover:bg-[var(--color-danger)]/5 transition-colors"
              >
                <span className="text-[1em] text-[var(--text-secondary)]">{moduleT('leftovers.deleteFailed')}</span>
                <div className="flex items-center gap-[0.57em]">
                  <span className="text-[1em] font-bold text-[var(--color-danger)]">
                    {moduleT('leftoversExtra.count', { count: failedItems.length })}
                  </span>
                  {expandedSection === 'failed' ? (
                    <ChevronUp className="w-[1.14em] h-[1.14em] text-[var(--color-danger)]" />
                  ) : (
                    <ChevronDown className="w-[1.14em] h-[1.14em] text-[var(--color-danger)]" />
                  )}
                </div>
              </button>
              {expandedSection === 'failed' && (
                <div className="px-[0.86em] pb-[0.86em] space-y-[0.57em]">
                  {failedItems.map((item, idx) => (
                    <div key={idx} className="glass-block flex items-center gap-[0.57em] p-[0.57em] bg-[var(--bg-card)] rounded-lg">
                      <XCircle className="w-[1.14em] h-[1.14em] text-[var(--color-danger)] shrink-0" />
                      <div className="flex-1 min-w-0">
                        <p className="text-[0.86em] font-medium text-[var(--text-primary)] truncate" title={item.path}>
                          {getFolderName(item.path)}
                        </p>
                        <p className="text-[0.71em] text-[var(--color-danger)]">
                          {getFailureReason(item)}
                        </p>
                      </div>
                      <button
                        onClick={() => openInFolder(item.path)}
                        className="shrink-0 p-[0.43em] rounded-lg text-[var(--text-muted)] hover:text-[var(--brand-green)] hover:bg-[var(--bg-hover)] transition-colors"
                    title={t('openInFolder')}
                      >
                        <FolderOpen className="w-[1.14em] h-[1.14em]" />
                      </button>
                    </div>
                  ))}
                </div>
              )}
            </div>
          )}
        </div>

        {/* Bottom action area.
            Force delete must sit alongside "OK": it used to be attached to the end of the expanded "needs manual review" list,
            and users who did not scroll there would never see it — yet those who did not scroll are exactly the ones who needed this exit most.
            Use a danger-colored outline instead of solid red: "OK" is the main action here, force delete is the recovery path. */}
        <div className="flex shrink-0 items-stretch gap-[0.57em]">
          {canForceDelete && (
            <button
              type="button"
              onClick={() => onForceDelete?.(reviewItems.map((item) => item.path))}
              className="min-w-0 flex-1 rounded-xl border border-[var(--color-danger)]/40 bg-[var(--color-danger)]/10 px-[1.14em] py-[0.86em] text-[1em] font-medium text-[var(--color-danger)] transition-colors hover:bg-[var(--color-danger)]/20"
            >
              {moduleT('leftoversExtra.forceDelete', { count: reviewItems.length })}
            </button>
          )}
          <button
            onClick={onClose}
            className={`min-w-0 rounded-xl px-[1.14em] py-[0.86em] text-[1em] font-medium bg-[var(--brand-green)] text-white hover:opacity-90 transition-colors ${canForceDelete ? 'flex-1' : 'w-full'}`}
          >
            {moduleT('leftoversExtra.confirm')}
          </button>
        </div>
      </div>
    </div>
  );
}
