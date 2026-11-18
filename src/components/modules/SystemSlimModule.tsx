import { useAiModuleBridge } from '../ai/useAiModuleBridge';
// ============================================================================
// System slim module component
// Display the system slim feature in the dashboard
// ============================================================================

import { useState, useCallback, useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { listen } from '@tauri-apps/api/event';
import { 
  Rocket, 
  Moon, 
  Package, 
  MemoryStick,
  Search,
  AlertTriangle,
  Loader2,
  CheckCircle2,
  ShieldAlert,
  ChevronRight
} from 'lucide-react';
import { ModuleCard } from '../ModuleCard';
import { ModuleScanProgress } from '../ModuleScanProgress';
import { AdminRequiredHint } from '../AdminRequiredHint';
import { EmptyState } from '../EmptyState';
import { ConfirmDialog } from '../ConfirmDialog';
import { useToast } from '../Toast';
import { useModuleDashboard } from '../../contexts/DashboardContext';
import {
  getSystemSlimStatus,
  disableHibernation,
  enableHibernation,
  cleanupWinsxs,
  cleanupWinsxsResetbase,
  openVirtualMemorySettings,
  rebuildSearchIndex,
  SlimItemStatus,
  SlimOperationProgress,
  SystemSlimStatus
} from '../../api/commands';
import { formatSize } from '../../utils/format';
import type { ModuleRenderProps } from './moduleProps';
import { errorText } from '../../api/errors';

// ============================================================================
// Configuration
// ============================================================================

const itemIcons: Record<string, typeof Moon> = {
  hibernation: Moon,
  winsxs: Package,
  winsxs_resetbase: Package,
  pagefile: MemoryStick,
  search_index: Search,
};

const itemColors: Record<string, { bg: string; text: string }> = {
  hibernation: { bg: 'bg-indigo-500/10', text: 'text-indigo-500' },
  winsxs: { bg: 'bg-amber-500/10', text: 'text-amber-500' },
  winsxs_resetbase: { bg: 'bg-orange-500/10', text: 'text-orange-500' },
  pagefile: { bg: 'bg-cyan-500/10', text: 'text-cyan-500' },
  search_index: { bg: 'bg-blue-500/10', text: 'text-blue-500' },
};


/**
 * Get the button label.
 *
 * Note: the button label must reflect the real state rather than a fixed label:
 *   - Hibernation: show "Disable hibernation" when enabled, "Enable hibernation" when disabled;
 *   - The backend action_text is in Chinese (no cleanup needed / deep cleanup / run cleanup / rescan),
 *     so in other languages it must be used to look up the matching i18n key, to avoid Chinese buttons appearing in the English/Japanese UI.
 */
function getActionLabel(
  item: SlimItemStatus,
  translate: (key: string, options?: Record<string, unknown>) => string,
): string {
  if (item.id === 'hibernation') {
    return translate(item.enabled ? 'systemSlim.items.hibernation.disable' : 'systemSlim.items.hibernation.enable');
  }

  // Mapping between action_text and i18n keys; key order corresponds one-to-one with backend labels.
  const actionTextToKey: Record<string, string> = {
    无需清理: 'noCleanup',
    开始清理: 'startCleanup',
    深度清理: 'deepCleanup',
    执行清理: 'runCleanup',
    重新检测: 'rescan',
    打开设置: 'openSettings',
  };
  const matchedKey = actionTextToKey[item.action_text];
  if (matchedKey) {
    return translate(`systemSlim.actions.${matchedKey}`);
  }

  // When the backend adds a label but the frontend has not added the i18n key yet, at least do not fall back to the old static label (which would cause inconsistent state).
  return item.action_text || translate(`systemSlim.items.${item.id}.action`);
}

/**
 * Format the elapsed time.
 *
 * Note: when the DISM cleanup progress percentage stays unchanged for a long time, the only evidence that it is "still running" is the elapsed time,
 * so display it consistently in "seconds" or "minutes:seconds" to avoid confusion from showing only milliseconds.
 */
function formatElapsed(elapsedMs: number): string {
  const totalSeconds = Math.max(0, Math.floor(elapsedMs / 1000));
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return minutes > 0 ? `${minutes}:${String(seconds).padStart(2, '0')}` : `${seconds}s`;
}

// ============================================================================
// Component implementation
// ============================================================================

export function SystemSlimModule({ isPageActive = true }: ModuleRenderProps) {
  const { t: navT } = useTranslation('nav');
  const { t } = useTranslation('common');
  const { t: moduleT } = useTranslation('modules');
  const { moduleState, updateModuleState, triggerHealthRefresh, oneClickScanTrigger } = useModuleDashboard('system');
  const { showToast } = useToast();

  // Track whether the current one-click scan trigger has been handled
  const lastScanTriggerRef = useRef(0);

  // Local state
  const [status, setStatus] = useState<SystemSlimStatus | null>(null);
  // DISM analysis progress: shown only while the two WinSxS items are "analyzing", to avoid a lone spinner for tens of seconds
  const [winsxsProgress, setWinsxsProgress] = useState<{ percent: number; elapsed_ms: number } | null>(null);
  // When a component cleanup has just run, record the pre-cleanup reclaimable amount: only when the analysis result returns can we say whether space was actually freed,
  // which is unknown at the moment the command returns (DISM must re-analyze to know).
  const pendingCleanupRef = useRef<{ before: number; resetBase: boolean } | null>(null);
  // showToast may be a new function on every render; put it in a ref so the event subscription does not need it in the dependency array
  const showToastRef = useRef(showToast);
  useEffect(() => {
    showToastRef.current = showToast;
  }, [showToast]);
  // When background events arrive, read the latest state to merge; cannot rely on a closure capturing the snapshot from render time
  const statusRef = useRef<SystemSlimStatus | null>(null);
  useEffect(() => {
    statusRef.current = status;
  }, [status]);

  // While background DISM analysis is still running, the UI disables "rescan" (clicking would only return the same "analyzing" result).
  // But the backend's stuck-state self-healing (force-restart analysis after timeout) only runs on a new request - a locked button would never wait for
  // that request, leaving them deadlocked. So if analysis exceeds the fallback time (backend 600 seconds + margin), release the button
  // so the user can click once to restart the stuck analysis.
  const isAnalyzingWinsxs = status?.items.some((item) => item.analyzing) ?? false;
  const [analyzeTimedOut, setAnalyzeTimedOut] = useState(false);
  useEffect(() => {
    if (!isAnalyzingWinsxs) {
      setAnalyzeTimedOut(false);
      return;
    }
    const timer = window.setTimeout(() => setAnalyzeTimedOut(true), 11 * 60 * 1000);
    return () => window.clearTimeout(timer);
  }, [isAnalyzingWinsxs]);
  // Integrate with the AI assistant (read-only): provide a summary only, no actionable operations.
  useAiModuleBridge('system-slim', {
    summary: () => {
      if (!status) return null;
      return [
        `系统瘦身可处理 ${status.items.length} 项，合计可释放约 ${formatSize(status.total_reclaimable)}。`,
        status.is_admin
          ? '当前已取得管理员权限，可以处理需要权限的项目。'
          : '当前没有管理员权限，需要权限的项目只能给出说明、无法直接处理。',
      ].join('');
    },
    // Read-only: the slim item list (the column shown in the UI).
    // Include the backend's status_text and "whether it can be handled" for each item so the model cannot guess from names.
    readItems: (_targetId, offset, limit) => {
      const items = status?.items ?? [];
      if (items.length === 0) {
        // State the two reasons for an empty result separately: the troubleshooting directions are completely different
        return {
          total: 0,
          lines: [
            status
              ? '（已经检查过，但没有读到任何可处理的项目）'
              : '（当前还没有检查结果 —— 请先在模块里点一次扫描，再问这个问题）',
          ],
        };
      }
      return {
        total: items.length,
        lines: items.slice(offset, offset + limit).map((item) => {
          const size = item.size > 0 ? formatSize(item.size) : '未统计';
          return `${item.name}  ${size}  （${item.status_text}${item.actionable ? '，可处理' : '，当前不可处理'}）`;
        }),
      };
    },
    // Read-only: single-item detail. For itemId, first use the id from items; if not found, fall back to the name.
    readItemDetail: (itemId) => {
      const wanted = itemId.trim();
      if (!wanted) return null;
      const items = status?.items ?? [];
      const item =
        items.find((entry) => entry.id === wanted) ??
        items.find((entry) => entry.name.trim() === wanted);
      if (!item) return null;
      return [
        `名称：${item.name}`,
        `说明：${item.description}`,
        `可释放：${item.size > 0 ? formatSize(item.size) : '未统计'}`,
        // Give the breakdown details too: reporting only a total would make the model vague as well
        item.breakdown
          ? `可回收构成：被取代的旧组件副本 ${formatSize(item.breakdown.backups)}、缓存与临时数据 ${formatSize(item.breakdown.cache)}`
          : '',
        `当前状态：${item.status_text}`,
        item.warning ? `⚠️ 风险提示：${item.warning}` : '',
        item.actionable
          ? `可执行的操作：${item.action_text}`
          : '当前状态下不能直接处理（通常是缺管理员权限，或该状态不允许操作）',
      ]
        .filter(Boolean)
        .join('\n');
    },
  });

  const [loading, setLoading] = useState(false);
  const [actionLoading, setActionLoading] = useState<string | null>(null);
  // Progress of the running slim operation: DISM cleanup may last several minutes, so phase and percentage feedback is required.
  const [operationProgress, setOperationProgress] = useState<SlimOperationProgress | null>(null);
  /*
    Elapsed time cannot rely only on backend pushes. DISM only emits progress events when the percentage advances, and it often stays at the same
    percentage for a long time (on a real machine "10%" stalled for tens of seconds), with no events during that period,
    so "elapsed 0s" would freeze along with it - what the user sees is "stuck". Use a local clock as a fallback here,
    taking the larger of the two when backend events arrive, so it neither goes backwards nor stops.
  */
  const progressStartedAtRef = useRef(0);
  const [clockNow, setClockNow] = useState(0);
  const progressRunning = operationProgress !== null && actionLoading !== null;
  useEffect(() => {
    if (!progressRunning) return;
    setClockNow(Date.now());
    const timer = window.setInterval(() => setClockNow(Date.now()), 500);
    return () => window.clearInterval(timer);
  }, [progressRunning]);
  const [showAdminWarning, setShowAdminWarning] = useState(true);
  // Baseline compaction is irreversible; require a second confirmation before executing.
  const [pendingConfirmItem, setPendingConfirmItem] = useState<SlimItemStatus | null>(null);
  // The confirm dialog callback needs the latest handler; use a ref to avoid rebuilding all button callbacks for it.
  const handleActionRef = useRef<(item: SlimItemStatus) => void>(() => {});

  // Subscribe to backend progress events; after the operation ends, keep the final state and let the operation flow handle cleanup.
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;

    listen<SlimOperationProgress>('system-slim:progress', (event) => {
      if (!disposed) setOperationProgress(event.payload);
    })
      .then((dispose) => {
        if (disposed) dispose();
        else unlisten = dispose;
      })
      .catch((error) => {
        // Subscription failure only affects progress display; the operation itself can still run, so just log it.
        console.error('订阅系统瘦身进度失败:', error);
      });

    return () => {
      disposed = true;
      if (unlisten) unlisten();
    };
  }, []);

  /**
   * WinSxS's DISM analysis runs in the background and pushes results via events.
   *
   * Note: DISM analysis takes tens of seconds to minutes; previously the whole detection API waited for it (released only after a 30-second timeout),
   * so the user saw a "checking" spinner for half a minute. Now the backend first returns the other four items and marks these two as analyzing,
   * and this code replaces them once analysis completes.
   */
  useEffect(() => {
    let disposed = false;
    const disposers: Array<() => void> = [];

    const register = (promise: Promise<() => void>) => {
      promise
        .then((dispose) => {
          if (disposed) dispose();
          else disposers.push(dispose);
        })
        .catch((error) => {
          // Subscription failure only affects this item's auto-update; the user can still manually rescan
          console.error('订阅 WinSxS 分析事件失败:', error);
        });
    };

    // DISM analysis percentage: during tens of seconds of waiting, this is the only thing that confirms progress to the user
    register(
      listen<{ percent: number; elapsed_ms: number }>('system-slim:winsxs-progress', (event) => {
        if (!disposed) setWinsxsProgress(event.payload ?? null);
      }),
    );

    register(
      listen<{ items: SlimItemStatus[] }>('system-slim:winsxs-ready', (event) => {
        if (disposed) return;
        setWinsxsProgress(null);

        const incoming = event.payload?.items ?? [];
        if (incoming.length === 0) return;

        const current = statusRef.current;
        if (!current) return;

        const byId = new Map(incoming.map((item) => [item.id, item]));
        const items = current.items.map((item) => byId.get(item.id) ?? item);
        const total = items
          .filter((item) => item.enabled)
          .reduce((sum, item) => sum + item.size, 0);

        const next = { ...current, items, total_reclaimable: total };
        statusRef.current = next;
        setStatus(next);
        updateModuleState('system', {
          fileCount: items.filter((item) => item.actionable).length,
          totalSize: total,
        });

        // A component cleanup just ran: now that the analysis result is back can we say whether space was actually freed.
        // That was unknown when the command returned - previously a number was made up at that moment, which was false.
        const pending = pendingCleanupRef.current;
        if (!pending) return;

        const winsxsItem = items.find((item) => item.id === 'winsxs');
        if (!winsxsItem || winsxsItem.analyzing) return;
        pendingCleanupRef.current = null;

        const title = pending.resetBase
          ? moduleT('systemSlim.resetBaseCompletedTitle')
          : moduleT('systemSlim.componentCleanup');

        if (winsxsItem.size <= 0) {
          // size 0 has two possibilities: it was really emptied, or this analysis read no result at all.
          // The latter (actionable is true when analysis fails) must not be described as "all cleaned up" - that would be a lie.
          showToastRef.current({
            title,
            description: winsxsItem.actionable
              ? moduleT('systemSlim.cleanupReprobeFailed')
              : moduleT('systemSlim.cleanupNoRemaining'),
            type: winsxsItem.actionable ? 'warning' : 'success',
          });
        } else if (winsxsItem.size < pending.before) {
          showToastRef.current({
            title,
            description: moduleT('systemSlim.actualReclaim', {
              size: formatSize(pending.before - winsxsItem.size),
            }),
            type: 'success',
          });
        } else {
          showToastRef.current({
            title,
            description: moduleT('systemSlim.cleanupNoSpaceFreed', {
              size: formatSize(winsxsItem.size),
            }),
            type: 'info',
          });
        }
      }),
    );

    return () => {
      disposed = true;
      disposers.forEach((dispose) => dispose());
    };
  }, [updateModuleState, moduleT]);

  const markItemsNeedRescan = useCallback((itemIds: string[]) => {
    setStatus((current) => {
      if (!current) return current;
      const idSet = new Set(itemIds);
      const items = current.items.map((item) => {
        if (!idSet.has(item.id)) return item;
        return {
          ...item,
          enabled: false,
          size: 0,
          actionable: false,
          action_text: moduleT('systemSlim.rescan'),
          status_text: moduleT('systemSlim.done'),
        };
      });
      updateModuleState('system', {
        fileCount: items.filter((item) => item.actionable).length,
        totalSize: items.filter((item) => item.enabled).reduce((sum, item) => sum + item.size, 0),
      });
      return { ...current, items, total_reclaimable: items.filter((item) => item.enabled).reduce((sum, item) => sum + item.size, 0) };
    });
  }, [updateModuleState]);

  /**
   * Re-read the backend state immediately after an operation ends.
   *
   * Note: the backend state is authoritative. Previously, completing an operation only did a local "mark needs rescan", causing the button label and
   * size to stay at the pre-operation snapshot (still showing "Disable hibernation" after disabling it, and the old estimated size after cleanup).
   * This does not reuse loadStatus because it has a "block while busy" guard, and this function is called during operation wrap-up.
   */
  const refreshStatusAfterAction = useCallback(async () => {
    try {
      const result = await getSystemSlimStatus();
      setStatus(result);
      updateModuleState('system', {
        status: 'done',
        fileCount: result.items.filter((current) => current.actionable).length,
        totalSize: result.total_reclaimable,
      });
    } catch (error) {
      // A refresh failure does not affect the operation result itself; degrade to prompting the user to rescan manually.
      console.warn('操作后刷新系统瘦身状态失败:', error);
      markItemsNeedRescan(['hibernation', 'winsxs', 'winsxs_resetbase']);
    }
  }, [markItemsNeedRescan, updateModuleState]);

  // Load the system slim state
  //
  // forceRefresh: pass true when the user clicks "rescan" - component storage analysis results are cached for 10 minutes,
  // without a forced refresh the API returns stale data instantly, making the UI look "refreshed" rather than actually re-tested.
  const loadStatus = useCallback(async (forceRefresh = false) => {
    // Rescanning is blocked while a system operation runs: concurrent DISM analysis and cleanup interfere with each other and scramble the state display.
    if (actionLoading) {
      showToast({ title: moduleT('systemSlim.busyTitle'), description: moduleT('systemSlim.busyDesc'), type: 'info' });
      return;
    }

    setLoading(true);
    updateModuleState('system', { status: 'scanning' });
    
    try {
      const result = await getSystemSlimStatus(forceRefresh);
      setStatus(result);
      
      updateModuleState('system', {
        status: 'done',
        fileCount: result.items.filter(i => i.actionable).length,
        totalSize: result.total_reclaimable,
      });
    } catch (error) {
      console.error('加载系统瘦身状态失败:', error);
      updateModuleState('system', { status: 'error', error: errorText(error) });
    } finally {
      setLoading(false);
    }
  }, [actionLoading, moduleT, showToast, updateModuleState]);

  // Listen for the one-click scan trigger
  useEffect(() => {
    if (oneClickScanTrigger > 0 && oneClickScanTrigger !== lastScanTriggerRef.current) {
      lastScanTriggerRef.current = oneClickScanTrigger;
      loadStatus(true);
    }
  }, [oneClickScanTrigger, loadStatus]);

  // Execute a slim operation
  const runAction = useCallback(async (item: SlimItemStatus) => {
    if (!status?.is_admin) {
      showToast({ title: moduleT('systemSlim.adminRequired'), description: moduleT('systemSlim.adminHint'), type: 'error' });
      return;
    }

    // Only one system-level operation at a time: concurrent DISM and powercfg interfere and are harder to troubleshoot.
    if (actionLoading) {
      showToast({ title: moduleT('systemSlim.busyTitle'), description: moduleT('systemSlim.busyDesc'), type: 'info' });
      return;
    }

    setActionLoading(item.id);
    // Record the starting point: the local clock counts from this moment, so tens of quiet DISM seconds will not freeze "elapsed".
    progressStartedAtRef.current = Date.now();
    setClockNow(Date.now());
    // Record the pre-operation size; after completion, tell the user the real difference ("how much was actually freed") instead of only the estimate.
    const sizeBefore = status?.items.find((current) => current.id === item.id)?.size ?? 0;
    // Use local state as a fallback first to avoid a "clicked but nothing happened" gap before the first backend progress event.
    setOperationProgress({
      item_id: item.id,
      phase: 'preparing',
      message: moduleT('systemSlim.progressPreparing'),
      percent: 0,
      status: 'running',
      elapsed_ms: 0,
    });

    try {
      switch (item.id) {
        case 'hibernation':
          if (item.enabled) {
            await disableHibernation();
            showToast({ title: moduleT('systemSlim.operationCompleted'), description: moduleT('systemSlim.operationCompletedDesc'), type: 'success' });
          } else {
            await enableHibernation();
            showToast({ title: moduleT('systemSlim.operationCompleted'), description: moduleT('systemSlim.operationCompletedDesc'), type: 'success' });
          }
          break;
        case 'winsxs':
          await cleanupWinsxs();
          // A successful command does not mean space was really freed. DISM must re-analyze (about 1 minute) to know the result,
          // so here only say "command completed, re-analyzing" and report the actual number when the analysis result is pushed back.
          pendingCleanupRef.current = { before: sizeBefore, resetBase: false };
          showToast({ title: moduleT('systemSlim.componentCleanup'), description: moduleT('systemSlim.cleanupStartedReprobe'), type: 'success' });
          break;
        case 'winsxs_resetbase':
          await cleanupWinsxsResetbase();
          pendingCleanupRef.current = { before: sizeBefore, resetBase: true };
          showToast({ title: moduleT('systemSlim.resetBaseCompletedTitle'), description: moduleT('systemSlim.cleanupStartedReprobe'), type: 'success' });
          break;
        case 'pagefile':
          await openVirtualMemorySettings();
          showToast({ title: moduleT('systemSlim.settingsOpened'), description: moduleT('systemSlim.pagefileOpened'), type: 'info' });
          break;
        case 'search_index':
          await rebuildSearchIndex();
          showToast({ title: moduleT('systemSlim.searchIndexRebuildStartedTitle'), description: moduleT('systemSlim.searchIndexRebuildStarted'), type: 'success' });
          break;
      }

      // After the operation completes, re-read the real state: whether hibernation is off and whether components still have reclaimable content must come from the backend,
      // otherwise the button label stays at the pre-operation state (e.g., still showing "Disable hibernation" after disabling it).
      if (item.id !== 'pagefile' && item.id !== 'search_index') {
        await refreshStatusAfterAction();
      }

      if (item.id === 'hibernation' || item.id === 'winsxs' || item.id === 'winsxs_resetbase') {
        triggerHealthRefresh();
      }
    } catch (error) {
      showToast({ title: moduleT('systemSlim.operationFailed'), description: errorText(error), type: 'error' });
    } finally {
      setActionLoading(null);
      // Remove the progress view as soon as the operation ends: success is reported by a toast, failure by an error toast.
      setOperationProgress(null);
    }
  }, [actionLoading, moduleT, refreshStatusAfterAction, showToast, status, triggerHealthRefresh, markItemsNeedRescan]);

  // Ensure the confirm dialog always calls the latest handler.
  useEffect(() => {
    handleActionRef.current = runAction;
  }, [runAction]);

  /**
   * Button entry: baseline compaction removes the update rollback baseline and is irreversible, so show a second confirmation first;
   * other operations are reversible and run directly.
   */
  const handleAction = useCallback((item: SlimItemStatus) => {
    if (item.id === 'winsxs_resetbase') {
      setPendingConfirmItem(item);
      return;
    }
    void runAction(item);
  }, [runAction]);
  // Any running system operation counts as busy: all operation entry points (buttons, rescan) use this uniformly to block.
  const isActionBusy = actionLoading !== null;
  // Analysis just started -> disable; analysis has passed the fallback time -> release, to avoid button and stuck process deadlocking.
  const isAnalyzingLocked = isAnalyzingWinsxs && !analyzeTimedOut;

  // Elapsed time for display: use whichever is larger, the backend's or the local clock's
  // (the backend corrects it once at wrap-up, the local clock handles continuity between frames).
  const elapsedMs = Math.max(
    operationProgress?.elapsed_ms ?? 0,
    progressStartedAtRef.current > 0 && clockNow > 0
      ? clockNow - progressStartedAtRef.current
      : 0,
  );

  if (!isPageActive && !actionLoading) {
    return null;
  }

  return (
    <>
      {/* Baseline compaction is irreversible; execute only after a second confirmation */}
      <ConfirmDialog
        isOpen={pendingConfirmItem !== null}
        title={moduleT('systemSlim.confirmResetBaseTitle')}
        description={moduleT('systemSlim.confirmResetBaseDesc')}
        warning={moduleT('systemSlim.items.winsxs_resetbase.warning')}
        confirmText={moduleT('systemSlim.confirmResetBaseConfirm')}
        cancelText={t('cancel')}
        onConfirm={() => {
          const item = pendingConfirmItem;
          setPendingConfirmItem(null);
          if (item) void handleActionRef.current(item);
        }}
        onCancel={() => setPendingConfirmItem(null)}
        isDanger
      />

      <ModuleCard
      id="system"
        title={navT('systemSlim')}
        description={navT('systemSlimDesc')}
      icon={<Rocket className="w-6 h-6 text-[var(--brand-green)]" />}
      status={moduleState.status}
      fileCount={moduleState.fileCount}
      totalSize={moduleState.totalSize}
      onScan={() => loadStatus(true)}
      scanButtonText={loading ? moduleT('systemSlim.checking') : status ? moduleT('systemSlim.rescan') : moduleT('systemSlim.check')}
      // Rescanning is not allowed while checking / background analysis / system operation is running:
      // the first two would not return a new conclusion, and the last would interfere with concurrent DISM.
      scanDisabled={loading || isAnalyzingLocked || isActionBusy}
      error={moduleState.error}
      headerExtra={
        status && (
          <div className="flex items-center gap-2 text-xs">
            {status.is_admin ? (
              <span className="flex items-center gap-1 px-2 py-0.5 rounded-full bg-emerald-500/10 text-emerald-600">
                <CheckCircle2 className="w-3 h-3" />
                {moduleT('systemSlim.administrator')}
              </span>
            ) : (
              <span className="flex items-center gap-1 px-2 py-0.5 rounded-full bg-amber-500/10 text-amber-600">
                <ShieldAlert className="w-3 h-3" />
                {moduleT('systemSlim.permissionRequired')}
              </span>
            )}
          </div>
        )
      }
    >
      {/* Expanded content */}
      <div className="p-4 space-y-3">
        {/* Admin privilege warning: whether elevation is active is determined by the shared component; this module only decides "whether to show".
            ⚠️ Do not add `status &&` - that would wait for the scan to return, so the user would not see this hint at all before scanning,
            which is exactly when they most need to know "I have no permission". */}
        {showAdminWarning && (
          <AdminRequiredHint
            description={moduleT('systemSlim.adminHint')}
            dismissible
            onDismiss={() => setShowAdminWarning(false)}
          />
        )}

        {/* Loading state */}
        {loading && !status && (
          <ModuleScanProgress
            title={`${moduleT('systemSlim.checking')}...`}
            icon={<Loader2 className="h-7 w-7 animate-spin text-[var(--brand-green)]" />}
          />
        )}

        {/* Empty state */}
        {moduleState.status === 'idle' && !status && (
          <EmptyState
            icon={Rocket}
              title={t('notScannedSystemState')}
            description={moduleT('systemSlim.emptyDesc')}
          />
        )}

        {/* Slim item list */}
        {status && (
          <div className="space-y-2">
            {status.items.map((item) => {
              const Icon = itemIcons[item.id] || Package;
              const colors = itemColors[item.id] || itemColors.winsxs;
              const isLoading = actionLoading === item.id;
              // The disable condition and the button style share the same decision to avoid "looks clickable but nothing happens".
              const isItemButtonDisabled = !item.actionable || !status.is_admin || isActionBusy;

              return (
                <div
                  key={item.id}
                  className={`glass-list bg-[var(--bg-base)] rounded-xl border border-[var(--border-default)] overflow-hidden transition-all ${
                    item.actionable
                      ? 'hover:border-emerald-500/30'
                      : item.analyzing
                        ? ''
                        : 'opacity-60'
                  }`}
                >
                  <div className="p-4">
                    <div className="flex items-start gap-3">
                      {/* Icon */}
                      <div className={`w-10 h-10 rounded-lg ${colors.bg} flex items-center justify-center shrink-0`}>
                        <Icon className={`w-5 h-5 ${colors.text}`} />
                      </div>

                      {/* Content */}
                      <div className="flex-1 min-w-0">
                        <div className="flex items-center gap-2">
                          <h4 className="text-sm font-semibold text-[var(--fg-primary)]">
                            {moduleT(`systemSlim.items.${item.id}.name`, { defaultValue: item.name })}
                            {/* Search index rebuild depends on the local SearchAPI.dll and has not been fully verified in real environments, so it is temporarily marked Beta */}
                            {item.id === 'search_index' && <span className="ml-1">（Beta）</span>}
                          </h4>
                          {/* This item is still being analyzed: it is not "unavailable", the "conclusion has not returned", so do not gray it out */}
                          {item.analyzing && (
                            <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin text-[var(--brand-green)]" />
                          )}
                          {item.enabled && item.size > 0 && (
                            <span className="px-1.5 py-0.5 rounded-full text-[0.71rem] font-medium bg-emerald-500/10 text-emerald-600">
                              {formatSize(item.size)}
                            </span>
                          )}
                          {!item.enabled && item.id === 'hibernation' && (
                            <span className="px-1.5 py-0.5 rounded-full text-[0.71rem] font-medium bg-[var(--bg-hover)] text-[var(--fg-muted)]">
                              {moduleT('systemSlim.disabled')}
                            </span>
                          )}
                        </div>
                        <p className="text-xs text-[var(--fg-secondary)] mt-0.5">{moduleT(`systemSlim.items.${item.id}.description`, { defaultValue: item.description })}</p>
                        {item.status_text && (
                          <p className="text-[0.79rem] text-[var(--fg-muted)] mt-1">
                            {item.id === 'search_index' ? (
                              // The search index itemStatus key is fixed text; when actionable, show the dynamic state (service + database size),
                              // when not actionable, show the backend's specific gray-out reason (service stopped / disabled / not installed)
                              item.actionable ? (
                                <>
                                  {moduleT('systemSlim.searchIndexServiceRunning')}
                                  {item.size > 0 && (
                                    <> · {moduleT('systemSlim.searchIndexDbSize', { size: formatSize(item.size) })}</>
                                  )}
                                </>
                              ) : (
                                item.status_text
                              )
                            ) : (
                              // Use the backend text directly. This used to be wrapped in a translation call with defaultValue,
                              // but that key really exists, so defaultValue never took effect - all five items were replaced with
                              // the same sentence "operation completed, please rescan to refresh the current state", and the backend's
                              // real conclusions such as "DISM found no reclaimable components" were never visible.
                              item.status_text
                            )}
                          </p>
                        )}

                        {/* Cleanup scope details: writing only "clean up superseded component versions" leaves the user who sees 6.9 GB
                            unsure of what will actually be deleted or whether in-use items are affected. Here the scope,
                            untouched items, and the breakdown from DISM are listed separately. */}
                        {(item.id === 'winsxs' || item.id === 'winsxs_resetbase') && (
                          <div className="mt-2 rounded-lg border border-[var(--border-default)] bg-[var(--bg-hover)]/40 px-2.5 py-2">
                            <p className="text-[0.71rem] font-medium text-[var(--fg-secondary)]">
                              {moduleT(`systemSlim.items.${item.id}.scopeTitle`)}
                            </p>
                            <ul className="mt-1 space-y-1">
                              {(item.id === 'winsxs'
                                ? ['scopeSuperseded', 'scopeCache']
                                : ['scopeExtra']
                              ).map((scopeKey) => (
                                <li key={scopeKey} className="flex gap-1.5">
                                  <span className="mt-[0.42rem] h-1 w-1 shrink-0 rounded-full bg-amber-500" />
                                  <span className="text-[0.71rem] leading-relaxed text-[var(--fg-muted)]">
                                    {moduleT(`systemSlim.items.${item.id}.${scopeKey}`)}
                                  </span>
                                </li>
                              ))}
                            </ul>

                            {/* Breakdown from DISM analysis: split the total into "old component copies" and "cache and temporary data" */}
                            {item.id === 'winsxs' &&
                              item.breakdown &&
                              (item.breakdown.backups > 0 || item.breakdown.cache > 0) && (
                                <div className="mt-1.5 border-t border-dashed border-[var(--border-default)] pt-1.5">
                                  <p className="text-[0.71rem] font-medium text-[var(--fg-secondary)]">
                                    {moduleT('systemSlim.items.winsxs.breakdownTitle')}
                                  </p>
                                  <div className="mt-1 space-y-0.5">
                                    <div className="flex items-baseline justify-between gap-3 text-[0.71rem]">
                                      <span className="text-[var(--fg-muted)]">
                                        {moduleT('systemSlim.items.winsxs.breakdownBackups')}
                                      </span>
                                      <span className="shrink-0 tabular-nums text-[var(--fg-secondary)]">
                                        {formatSize(item.breakdown.backups)}
                                      </span>
                                    </div>
                                    <div className="flex items-baseline justify-between gap-3 text-[0.71rem]">
                                      <span className="text-[var(--fg-muted)]">
                                        {moduleT('systemSlim.items.winsxs.breakdownCache')}
                                      </span>
                                      <span className="shrink-0 tabular-nums text-[var(--fg-secondary)]">
                                        {formatSize(item.breakdown.cache)}
                                      </span>
                                    </div>
                                  </div>
                                </div>
                              )}

                            <p className="mt-1.5 border-t border-dashed border-[var(--border-default)] pt-1.5 text-[0.71rem] leading-relaxed text-[var(--fg-muted)]">
                              {moduleT(`systemSlim.items.${item.id}.scopeKeep`)}
                            </p>
                          </div>
                        )}

                        {/* Risk warning */}
                        <div className="mt-2 flex items-start gap-1.5 bg-amber-500/5 rounded-lg px-2 py-1.5">
                          <AlertTriangle className="w-3 h-3 text-amber-500 shrink-0 mt-0.5" />
                          <p className="text-[0.71rem] text-amber-600 leading-relaxed">{moduleT(`systemSlim.items.${item.id}.warning`, { defaultValue: item.warning })}</p>
                        </div>

                        {/* Analysis progress: while DISM is running with no conclusion yet, the percentage proves it is advancing.
                            Attach it only to the winsxs item - both items share the same analysis result, and drawing it twice is noisy. */}
                        {item.id === 'winsxs' && item.analyzing && winsxsProgress && (
                          <div className="mt-2 rounded-lg bg-[var(--brand-green-10)] px-3 py-2">
                            <div className="flex items-center justify-between gap-2 text-[0.79rem] text-[var(--brand-green)]">
                              <span className="truncate">{moduleT('systemSlim.analyzingComponents')}</span>
                              <span className="shrink-0 tabular-nums">
                                {winsxsProgress.percent > 0
                                  ? `${winsxsProgress.percent}%`
                                  : formatElapsed(winsxsProgress.elapsed_ms)}
                              </span>
                            </div>
                            <div className="mt-1.5 h-1.5 overflow-hidden rounded-full bg-[var(--bg-hover)]">
                              <div
                                className="h-full rounded-full bg-[var(--brand-green)] transition-all duration-500"
                                // DISM reports 0% at first; give a short initial segment, otherwise the progress bar looks empty
                                style={{ width: `${Math.max(winsxsProgress.percent, 2)}%` }}
                              />
                            </div>
                          </div>
                        )}

                        {/* Execution progress: let the user see the phase, percentage, and elapsed time */}
                        {isLoading && operationProgress && operationProgress.item_id === item.id && (
                          <div className="mt-2 rounded-lg bg-emerald-500/10 px-3 py-2">
                            <div className="flex items-center justify-between gap-2 text-[0.79rem] text-emerald-700 dark:text-emerald-400">
                              <span className="truncate">{operationProgress.message}</span>
                              <span className="shrink-0 tabular-nums">
                                {operationProgress.percent > 0 ? `${operationProgress.percent}%` : formatElapsed(elapsedMs)}
                              </span>
                            </div>
                            <div className="mt-1.5 h-1.5 overflow-hidden rounded-full bg-[var(--bg-hover)]">
                              <div
                                className={`h-full rounded-full bg-emerald-500 transition-all duration-300 ${operationProgress.percent > 0 ? '' : 'w-1/3 animate-pulse'}`}
                                style={operationProgress.percent > 0 ? { width: `${operationProgress.percent}%` } : undefined}
                              />
                            </div>
                            <p className="mt-1.5 flex items-center gap-1 text-[0.71rem] text-[var(--fg-muted)]">
                              <ShieldAlert className="h-3 w-3 shrink-0 text-amber-500" />
                              {moduleT('systemSlim.progressElapsed', { time: formatElapsed(elapsedMs) })} · {moduleT('systemSlim.progressDoNotClose')}
                            </p>
                          </div>
                        )}
                      </div>

                      {/* Action buttons */}
                      <div className="shrink-0">
                        <button
                          onClick={() => handleAction(item)}
                          /*
                            Disable all buttons whenever any operation is running: concurrent DISM and powercfg interfere with each other.
                            The disable condition must use the same decision as the style below, otherwise "looks clickable but nothing happens" occurs.
                          */
                          disabled={isItemButtonDisabled}
                          title={isActionBusy ? moduleT('systemSlim.waitForCurrent') : undefined}
                          className={`
                            px-3 py-1.5 rounded-lg text-xs font-medium transition-all flex items-center gap-1.5
                            ${isItemButtonDisabled
                              ? 'cursor-not-allowed bg-[var(--bg-hover)] text-[var(--fg-muted)]'
                              : item.id === 'winsxs_resetbase'
                                // Baseline compaction is irreversible and makes installed updates impossible to uninstall - use the danger color instead of brand green,
                                // so that "this item is not the same risk level as the others" is visible before clicking.
                                ? 'bg-[var(--color-danger)] text-white hover:bg-[var(--color-danger)]/90 active:scale-95'
                                : 'bg-emerald-500 text-white hover:bg-emerald-600 active:scale-95'
                            }
                          `}
                        >
                          {isLoading ? (
                            <>
                              <Loader2 className="w-3 h-3 animate-spin" />
                              <span>{moduleT('systemSlim.executing')}</span>
                            </>
                          ) : (
                            <>
                              <span>
                                {/*
                                  The button label must follow the real state, so pick the i18n text by state:
                                  show "Disable hibernation" when hibernation is on and "Enable hibernation" when off (previously it always statically showed "Disable hibernation").
                                  For the rest, look up the i18n key from the backend action_text; only fall back to the backend's raw text if no key is found.
                                */}
                                {getActionLabel(item, moduleT)}
                              </span>
                              <ChevronRight className="w-3 h-3" />
                            </>
                          )}
                        </button>
                      </div>
                    </div>
                  </div>
                </div>
              );
            })}
          </div>
        )}

        {/* Footer note */}
        {status && (
          <div className="glass-block bg-[var(--bg-elevated)] rounded-lg px-3 py-2 text-[0.71rem] text-[var(--fg-muted)] leading-relaxed">
            <strong className="text-[var(--fg-secondary)]">{moduleT('systemSlim.tip')}</strong>
            {moduleT('systemSlim.tipDesc')}
          </div>
        )}
      </div>
      </ModuleCard>
    </>
  );
}

export default SystemSlimModule;
