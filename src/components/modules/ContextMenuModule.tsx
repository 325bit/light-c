import { useAiModuleBridge } from '../ai/useAiModuleBridge';
// ============================================================================
// Context menu cleanup module
// Scan context menu entries registered in the Windows registry,
// identify invalid entries pointing to nonexistent executables, and provide one-click cleanup
// ============================================================================

import { useState, useCallback, useMemo, useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import {
  MousePointerClick,
  Loader2,
  Trash2,
  AlertTriangle,
  Shield,
  ChevronDown,
  ChevronRight,
  FolderOpen,
  Terminal,
} from 'lucide-react';
import { ModuleCard } from '../ModuleCard';
import { ModuleDeleteProgress } from '../ModuleDeleteProgress';
import { ModuleOperationToolbar } from '../ModuleOperationToolbar';
import { ModuleScanProgress } from '../ModuleScanProgress';
import { AdminRequiredHint, useAdminPrivilege } from '../AdminRequiredHint';
import { ConfirmDialog } from '../ConfirmDialog';
import { EmptyState } from '../EmptyState';
import { useModuleDashboard } from '../../contexts/DashboardContext';
import {
  scanContextMenu,
  deleteContextMenuEntries,
  openContextMenuBackupDir,
  recordCleanupAction,
  type ContextMenuScanResult,
  type ContextMenuEntry,
  type ContextMenuDeleteRequest,
  type CleanupLogEntryInput,
} from '../../api/commands';
import type { ModuleRenderProps } from './moduleProps';

// ============================================================================
// Utility functions
// ============================================================================

/**
 * scope id -> color. The id comes from the backend (file / folder / background / drive / library),
 * labels always go through i18n - the backend no longer decides what text appears in the UI.
 */
const SCOPE_STYLE: Record<string, string> = {
  file: 'status-chip text-blue-600',
  folder: 'status-chip text-yellow-600',
  background: 'status-chip text-purple-600',
  drive: 'status-chip text-orange-600',
  library: 'status-chip text-teal-600',
};

function getScopeStyle(scope: string, translate: (key: string) => string): { label: string; className: string } {
  const key = `contextMenu.scope.${scope}`;
  const label = translate(key);
  return {
    // When an i18n key is missing, the key itself is returned; in that case show the id at least, do not let "contextMenu.scope.xxx" appear in the UI
    label: label === key ? scope : label,
    className: SCOPE_STYLE[scope] ?? 'bg-[var(--bg-hover)] text-[var(--text-muted)]',
  };
}

/** Keep the same invalidation rule as the backend: only "path given but file missing" counts as invalid. */
function isInvalidContextMenuEntry(entry: Pick<ContextMenuEntry, 'exe_status'>): boolean {
  return entry.exe_status === 'missing';
}

/**
 * Entries with no extractable path that cannot be determined.
 *
 * These (COM handlers, rundll32 proxies, cascading menus) should not be marked red or cleaned as junk -
 * give them a neutral badge meaning "the program cannot determine this", rather than implying they are broken.
 */
function isUnknownContextMenuEntry(entry: Pick<ContextMenuEntry, 'exe_status'>): boolean {
  return entry.exe_status === 'unknown';
}

// ============================================================================
// Subcomponent: entry row
// ============================================================================

interface EntryRowProps {
  entry: ContextMenuEntry;
  isSelected: boolean;
  onToggle: (entry: ContextMenuEntry) => void;
}

function EntryRow({ entry, isSelected, onToggle }: EntryRowProps) {
  const { t } = useTranslation('common');
  const { t: moduleT } = useTranslation('modules');
  const [expanded, setExpanded] = useState(false);
  const scope = getScopeStyle(entry.scope, moduleT);
  const isInvalid = isInvalidContextMenuEntry(entry);
  const isUnknown = isUnknownContextMenuEntry(entry);
  const isProtected = entry.is_system_protected;

  return (
    <div
      className={`
        rounded-xl border transition-colors
        ${isProtected
          ? 'bg-[var(--color-danger)]/5 border-[var(--color-danger)]/15 cursor-not-allowed'
          : isSelected
            ? 'bg-[var(--brand-green-10)] border-[var(--brand-green-20)] cursor-pointer'
            : 'bg-[var(--bg-main)] border-transparent hover:bg-[var(--bg-hover)] cursor-pointer'
        }
      `}
      onClick={() => !isProtected && onToggle(entry)}
    >
      {/* Main row */}
      <div className="flex items-center gap-3 p-3">
        {/* Checkbox */}
        <div
          className={`
            w-5 h-5 rounded border-2 flex items-center justify-center shrink-0
            ${isProtected
              ? 'border-[var(--text-faint)] bg-[var(--bg-hover)] opacity-40'
              : isSelected
                ? 'bg-[var(--brand-green)] border-[var(--brand-green)]'
                : 'border-[var(--text-faint)]'
            }
          `}
        >
          {isSelected && !isProtected && (
            <svg className="w-3 h-3 text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={3}>
              <path strokeLinecap="round" strokeLinejoin="round" d="M5 13l4 4L19 7" />
            </svg>
          )}
          {isProtected && (
            <Shield className="w-3 h-3 text-[var(--text-faint)]" />
          )}
        </div>

        {/* Icon area */}
        <div
          className={`
            w-9 h-9 rounded-lg flex items-center justify-center shrink-0
            ${isProtected
              ? 'bg-[var(--color-danger)]/15'
              : isInvalid
                ? 'bg-[var(--color-danger)]/10'
                : isUnknown
                  ? 'bg-[var(--bg-hover)]'
                  : 'bg-[var(--brand-green-10)]'
            }
          `}
        >
          {isProtected
            ? <Shield className="w-4 h-4 text-[var(--color-danger)]" />
            : isInvalid
              ? <AlertTriangle className="w-4 h-4 text-[var(--color-danger)]" />
              : isUnknown
                // If it cannot be determined, use a neutral icon: a question mark/warning would make it look broken
                ? <MousePointerClick className="w-4 h-4 text-[var(--text-muted)]" />
                : <MousePointerClick className="w-4 h-4 text-[var(--brand-green)]" />
          }
        </div>

        {/* Text info */}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 flex-wrap">
            <p className="text-sm font-medium text-[var(--text-primary)] truncate max-w-[17.14rem]">
              {entry.display_name || entry.key_name}
            </p>

            {/* Risk level badge */}
            {entry.risk_level === 'danger' && (
              <span className="px-1.5 py-0.5 rounded text-[0.71rem] font-medium bg-[var(--color-danger)]/15 text-[var(--color-danger)] shrink-0">
                {moduleT('contextMenu.systemProtected')}
              </span>
            )}
            {entry.risk_level === 'caution' && (
              <span className="px-1.5 py-0.5 rounded text-[0.71rem] font-medium bg-[var(--color-warning)]/10 text-[var(--color-warning)] shrink-0">
                {moduleT('contextMenu.caution')}
              </span>
            )}

            {/* Scope badge */}
            <span className={`px-1.5 py-0.5 rounded text-[0.71rem] font-medium shrink-0 ${scope.className}`}>
              {scope.label}
            </span>

            {/* Registry root */}
            <span className="px-1.5 py-0.5 rounded text-[0.71rem] font-medium bg-[var(--bg-hover)] text-[var(--text-muted)] shrink-0">
              {entry.reg_root}
            </span>

            {/* Invalid marker */}
            {isInvalid && (
              <span className="px-1.5 py-0.5 rounded text-[0.71rem] font-medium bg-[var(--color-danger)]/10 text-[var(--color-danger)] shrink-0">
                {moduleT('contextMenu.missing')}
              </span>
            )}

            {/* Cannot determine: neutral color, do not make the user think this one is broken too */}
            {isUnknown && (
              <span className="px-1.5 py-0.5 rounded text-[0.71rem] font-medium bg-[var(--bg-hover)] text-[var(--text-muted)] shrink-0">
                {moduleT('contextMenu.unknown')}
              </span>
            )}

            {/* Needs admin */}
            {entry.needs_admin && (
              <span className="px-1.5 py-0.5 rounded text-[0.71rem] font-medium bg-[var(--color-warning)]/10 text-[var(--color-warning)] shrink-0">
                {moduleT('contextMenu.admin')}
              </span>
            )}
          </div>

          {/* exe path preview */}
          {entry.exe_path && (
            <p
              className={`text-xs mt-0.5 truncate ${
                isInvalid ? 'text-[var(--color-danger)]/70' : 'text-[var(--text-muted)]'
              }`}
              title={entry.exe_path}
            >
              <FolderOpen className="inline w-3 h-3 mr-1 opacity-60" />
              {entry.exe_path}
            </p>
          )}

          {/* System protection hint */}
          {isProtected && (
            <p className="text-xs mt-0.5 text-[var(--color-danger)]/70">
              {moduleT('contextMenu.comWarning')}
            </p>
          )}
        </div>

        {/* Expand/collapse button */}
        <button
          onClick={(e) => {
            e.stopPropagation();
            setExpanded((v) => !v);
          }}
          className="shrink-0 p-1 rounded text-[var(--text-faint)] hover:text-[var(--text-muted)]"
          title={t('viewDetails')}
        >
          {expanded
            ? <ChevronDown className="w-4 h-4" />
            : <ChevronRight className="w-4 h-4" />
          }
        </button>
      </div>

      {/* Expanded details */}
      {expanded && (
        <div
          className="px-4 pb-3 space-y-1.5 border-t border-[var(--border-muted)]"
          onClick={(e) => e.stopPropagation()}
        >
          {/* Full registry path */}
          <div className="flex items-start gap-2 pt-2">
            <span className="text-[0.79rem] text-[var(--text-faint)] shrink-0 w-20">{moduleT('contextMenu.registryPath')}</span>
            <span className="text-[0.79rem] text-[var(--text-muted)] break-all font-mono">
              {entry.registry_path}
            </span>
          </div>

          {/* Raw command */}
          {entry.command && (
            <div className="flex items-start gap-2">
              <span className="text-[0.79rem] text-[var(--text-faint)] shrink-0 w-20">{moduleT('contextMenu.command')}</span>
              <span
                className="text-[0.79rem] text-[var(--text-muted)] break-all font-mono"
                title={entry.command}
              >
                <Terminal className="inline w-3 h-3 mr-1 opacity-60" />
                {entry.command.length > 120
                  ? entry.command.slice(0, 120) + '…'
                  : entry.command}
              </span>
            </div>
          )}

          {/* Icon path */}
          {entry.icon_path && (
            <div className="flex items-start gap-2">
              <span className="text-[0.79rem] text-[var(--text-faint)] shrink-0 w-20">{moduleT('contextMenu.icon')}</span>
              <span className="text-[0.79rem] text-[var(--text-muted)] break-all font-mono">
                {entry.icon_path}
              </span>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Main component
// ============================================================================

export function ContextMenuModule({ isPageActive = true }: ModuleRenderProps) {
  const { t: navT } = useTranslation('nav');
  const { t } = useTranslation('common');
  const { t: moduleT } = useTranslation('modules');
  const {
    moduleState,
    updateModuleState,
    oneClickScanTrigger,
  } = useModuleDashboard('contextMenu');
  const lastScanTriggerRef = useRef(0);

  // ── Local state ──────────────────────────────────────────────────────────────
  const [scanResult, setScanResult] = useState<ContextMenuScanResult | null>(null);
  // Integrate with the AI assistant (read-only): provide a summary only, no actionable operations.
  useAiModuleBridge('context-menu', {
    // Read-only: let the assistant "look deeper" at which entries exist (one page at a time; the model decides whether to continue).
    readItems: (_targetId, offset, limit) => {
      const list = scanResult?.entries ?? [];
      if (list.length === 0) return null;
      return {
        total: list.length,
        lines: list.slice(offset, offset + limit).map(
          (item) => `${item.display_name}  ${item.reg_root}\\${item.reg_subpath}  （键名 ${item.key_name}）`,
        ),
      };
    },
    // Read-only: single-item detail. For itemId, use the display name at the start of the read_items line - each line in this module begins with it,
    // so the model naturally copies it back. Display names can repeat (the same app has one entry for "folder" and one for "any file"),
    // so first match exactly by id / registry path, then fall back to display name and key name.
    readItemDetail: (itemId) => {
      const wanted = itemId.trim();
      if (!wanted) return null;
      const list = scanResult?.entries ?? [];
      const item =
        list.find((entry) => entry.id === wanted) ??
        list.find((entry) => entry.registry_path === wanted) ??
        list.find((entry) => entry.display_name.trim() === wanted) ??
        list.find((entry) => entry.key_name.trim() === wanted);
      if (!item) return null;
      // Give only facts: "whether it can be deleted" is decided by the module's own logic (system protection / whether the target program exists),
      // do not let the model guess from the menu name.
      const target = item.exe_path
        ? `${item.exe_path}（${
            item.exe_status === 'missing'
              ? '**文件已不存在** —— 这个入口很可能已经失效'
              : '文件存在'
          }）`
        : '（命令里没有绝对路径，无法判断）';
      return [
        `名称：${item.display_name}`,
        `注册表位置：${item.registry_path}`,
        // scope is now an English id; what the model sees is the translated Chinese label
        `作用范围：${item.scope ? getScopeStyle(item.scope, moduleT).label : '未标注'}`,
        `命令：${item.command || '（未读取到）'}`,
        `目标程序：${target}`,
        `图标来源：${item.icon_path || '（未设置）'}`,
        item.is_system_protected
          ? '结论：这是**系统保护项**，界面上不允许选中删除。'
          : `结论：可以清理${item.needs_admin ? '，但需要管理员权限' : ''}（删除前程序会自动备份注册表）。`,
      ].join('\n');
    },
    summary: () => {
      if (!scanResult) return null;
      return `扫描到 ${scanResult.entries.length} 项右键菜单入口，其中 ${scanResult.invalid_count} 项指向已失效的程序。`;
    },
  });

  /** Maintain the selection set keyed by id */
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState<string | null>(null);
  const [deleteErrors, setDeleteErrors] = useState<string[]>([]);
  const [showErrorDetails, setShowErrorDetails] = useState(false);
  /** Collapse state grouped by scope */
  const [collapsedScopes, setCollapsedScopes] = useState<Set<string>>(new Set());
  /** Filter: show invalid entries only (off by default - the backend deliberately returns all entries; the user may also want to disable an unused menu) */
  const [showInvalidOnly, setShowInvalidOnly] = useState(false);

  // ── Derived data ──────────────────────────────────────────────────────────────

  /** Entries currently shown after filtering */
  const filteredEntries = useMemo(() => {
    if (!scanResult) return [];
    if (showInvalidOnly) {
      return scanResult.entries.filter(isInvalidContextMenuEntry);
    }
    return scanResult.entries;
  }, [scanResult, showInvalidOnly]);

  /** Group by scope */
  const groupedEntries = useMemo(() => {
    const groups = new Map<string, ContextMenuEntry[]>();
    for (const entry of filteredEntries) {
      if (!groups.has(entry.scope)) groups.set(entry.scope, []);
      groups.get(entry.scope)!.push(entry);
    }
    return groups;
  }, [filteredEntries]);

  /** Number of currently selected entries */
  const selectedCount = selectedIds.size;

  /**
   * Whether running as administrator.
   *
   * Entries under HKLM need registry writes; without admin rights `KEY_WRITE` is denied outright - measured on an ordinary
   * machine, 30 of 34 entries were in HKLM, so without elevation the whole module cannot delete anything. State this up front,
   * rather than waiting until the user clicks delete and sees a screen of "access denied".
   */
  const isAdmin = useAdminPrivilege();

  /** How many selected entries are in HKLM (all will fail without admin rights) */
  const selectedAdminCount = useMemo(() => {
    if (!scanResult || isAdmin !== false) return 0;
    return scanResult.entries.filter(
      (entry) => selectedIds.has(entry.id) && entry.needs_admin,
    ).length;
  }, [scanResult, selectedIds, isAdmin]);

  // ── Event handlers ─────────────────────────────────────────────────────────────

  /** Start scanning */
  const handleScan = useCallback(async () => {
    updateModuleState('contextMenu', { status: 'scanning', error: null });
    setScanResult(null);
    setSelectedIds(new Set());
    setDeleteError(null);
    setDeleteErrors([]);
    setShowErrorDetails(false);

    try {
      const result = await scanContextMenu();
      setScanResult(result);

      // By default, select only "truly invalid" entries (excluding system-protected ones).
      // After the three-state determination, no need to check exe_path again here: anything marked missing must have had a path.
      const defaultSelected = new Set(
        result.entries
          .filter((e) => isInvalidContextMenuEntry(e) && !e.is_system_protected)
          .map((e) => e.id)
      );
      setSelectedIds(defaultSelected);

      updateModuleState('contextMenu', {
        status: 'done',
        fileCount: result.invalid_count,
        totalSize: 0, // registry entries have no file-size concept
      });
    } catch (err) {
      console.error('右键菜单扫描失败:', err);
      updateModuleState('contextMenu', { status: 'error', error: String(err) });
    }
  }, [updateModuleState]);

  /** Listen for the one-click scan trigger */
  useEffect(() => {
    if (oneClickScanTrigger > 0 && oneClickScanTrigger !== lastScanTriggerRef.current) {
      lastScanTriggerRef.current = oneClickScanTrigger;
      handleScan();
    }
  }, [oneClickScanTrigger, handleScan]);

  /** Toggle a single entry's selection */
  const toggleSelect = useCallback((entry: ContextMenuEntry) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(entry.id)) {
        next.delete(entry.id);
      } else {
        next.add(entry.id);
      }
      return next;
    });
  }, []);

  /** Select all / deselect all (only affects currently filtered, non-system-protected entries) */
  const toggleSelectAll = useCallback(() => {
    if (!scanResult) return;
    // Selectable IDs excluding system-protected entries
    const selectableIds = filteredEntries
      .filter((e) => !e.is_system_protected)
      .map((e) => e.id);
    const allSelected =
      selectableIds.length > 0 &&
      selectableIds.every((id) => selectedIds.has(id));

    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (allSelected) {
        selectableIds.forEach((id) => next.delete(id));
      } else {
        selectableIds.forEach((id) => next.add(id));
      }
      return next;
    });
  }, [scanResult, filteredEntries, selectedIds]);

  /** Toggle a scope group's collapse state */
  const toggleScope = useCallback((scope: string) => {
    setCollapsedScopes((prev) => {
      const next = new Set(prev);
      if (next.has(scope)) {
        next.delete(scope);
      } else {
        next.add(scope);
      }
      return next;
    });
  }, []);

  /** Open the registry backup directory (manually double-click a .reg to restore after accidental deletion) */
  const handleOpenBackupDir = useCallback(async () => {
    try {
      await openContextMenuBackupDir();
    } catch (error) {
      console.error('打开备份目录失败:', error);
    }
  }, []);

  /** Perform deletion */
  const handleDelete = useCallback(async () => {
    if (selectedIds.size === 0 || !scanResult) return;

    setIsDeleting(true);
    setDeleteError(null);
    setDeleteErrors([]);
    setShowErrorDetails(false);

    try {
      // Build the deletion request list
      const requests: ContextMenuDeleteRequest[] = scanResult.entries
        .filter((e) => selectedIds.has(e.id))
        .map((e) => ({
          id: e.id,
          reg_root: e.reg_root,
          reg_subpath: e.reg_subpath,
        }));

      const result = await deleteContextMenuEntries(requests);

      // Record the cleanup log
      const failedIds = new Set(
        result.details.filter((d) => !d.success).map((d) => d.id)
      );
      const logEntries: CleanupLogEntryInput[] = requests.map((req) => {
        const detail = result.details.find((d) => d.id === req.id);
        return {
          category: '右键菜单清理',
          path: `${req.reg_root}\\${req.reg_subpath}`,
          size: 0,
          success: !failedIds.has(req.id),
          error_message: detail?.error ?? undefined,
        };
      });
      recordCleanupAction(logEntries).catch((err) =>
        console.warn('记录清理日志失败:', err)
      );

      // Collect failure information
      const errorMessages = result.details
        .filter((d) => !d.success && d.error)
        .map((d) => d.error as string);

      if (errorMessages.length > 0) {
        setDeleteError(t('failedCount', { count: errorMessages.length }));
        setDeleteErrors(errorMessages);
      }

      // Remove successfully deleted entries from the result
      const successIds = new Set(
        result.details.filter((d) => d.success).map((d) => d.id)
      );
      const remainingEntries = scanResult.entries.filter((e) => !successIds.has(e.id));
      const newInvalidCount = remainingEntries.filter(
        isInvalidContextMenuEntry
      ).length;

      setScanResult({
        ...scanResult,
        entries: remainingEntries,
        invalid_count: newInvalidCount,
      });

      // Update the selection state (keep only failures)
      setSelectedIds((prev) => {
        const next = new Set(prev);
        successIds.forEach((id) => next.delete(id));
        return next;
      });

      updateModuleState('contextMenu', {
        fileCount: newInvalidCount,
      });
    } catch (err) {
      console.error('删除右键菜单条目失败:', err);
      setDeleteError(String(err));
      setDeleteErrors([String(err)]);
    } finally {
      setIsDeleting(false);
      setShowDeleteConfirm(false);
    }
  }, [selectedIds, scanResult, updateModuleState]);

  // ── Rendering ─────────────────────────────────────────────────────────────────
  /** Selectable entries (excluding system-protected ones) */
  const selectableEntries = useMemo(
    () => filteredEntries.filter((e) => !e.is_system_protected),
    [filteredEntries]
  );
  const allFilteredSelected =
    selectableEntries.length > 0 &&
    selectableEntries.every((e) => selectedIds.has(e.id));

  if (!isPageActive && !isDeleting && !showDeleteConfirm) {
    return null;
  }

  return (
    <>
      <ModuleCard
        id="contextMenu"
        title={navT('contextMenu')}
        description={navT('contextMenuDesc')}
        icon={<MousePointerClick className="w-6 h-6 text-[var(--brand-green)]" />}
        status={moduleState.status}
        fileCount={moduleState.fileCount}
        totalSize={0}
        onScan={handleScan}
        error={moduleState.error}
        headerExtra={
          <span className="px-2 py-0.5 rounded text-[0.71rem] font-medium bg-[var(--color-warning)]/10 text-[var(--color-warning)] border border-[var(--color-warning)]/20">
            {moduleT('contextMenu.highRisk')}
          </span>
        }
      >
        {/* Delete progress is now attached to the floating action area (see busyContent above):
            it is a position: fixed layer that does not occupy document flow, so the result area neither expands nor shifts -
            the context menu needs to keep the reference of "which entries I am deleting". */}
        {moduleState.status === 'idle' && !scanResult && (
          <div className="p-5">
            <EmptyState
              icon={MousePointerClick}
              title={t('notScannedContextMenu')}
              description={moduleT('contextMenu.idleDesc')}
            />
          </div>
        )}

        {moduleState.status === 'scanning' && !scanResult && (
          <div className="p-5">
            <ModuleScanProgress
              title={t('scanningShort')}
              description={moduleT('contextMenu.idleDesc')}
              icon={<Loader2 className="h-7 w-7 animate-spin text-[var(--brand-green)]" />}
            />
          </div>
        )}

        {/* ── Scan result content ── */}
        {scanResult && (
          // pb-20: the bottom action bar is a fixed floating layer, so add bottom padding to keep the last row from being covered
          <div className="p-5 space-y-4 pb-20">

            {/* Without elevation, state it up front: HKLM entries cannot be deleted; do not wait until after delete to see a screen of access-denied errors */}
            <AdminRequiredHint description={moduleT('contextMenu.adminHint')} />

            {/* Safety banner */}
            <div className="flex items-start gap-3 p-4 bg-[var(--brand-green-10)] rounded-xl border border-[var(--brand-green-20)]">
              <Shield className="w-5 h-5 text-[var(--brand-green)] shrink-0 mt-0.5" />
              <div>
                <p className="text-sm font-medium text-[var(--text-primary)]">{moduleT('contextMenu.safetyTitle')}</p>
                <p className="text-xs text-[var(--text-muted)] mt-1">
                  {moduleT('contextMenu.safetyDesc')}
                </p>
              </div>
            </div>

            {/* Scan statistics */}
            <div className="grid grid-cols-3 gap-3">
              <div className="glass-block p-3 bg-[var(--bg-main)] rounded-xl text-center">
                <p className="text-xl font-bold text-[var(--text-primary)] tabular-nums">
                  {scanResult.entries.length}
                </p>
                <p className="text-xs text-[var(--text-muted)] mt-0.5">{moduleT('contextMenu.scanned')}</p>
              </div>
              <div className="p-3 bg-[var(--color-danger)]/5 rounded-xl text-center">
                <p className="text-xl font-bold text-[var(--color-danger)] tabular-nums">
                  {scanResult.invalid_count}
                </p>
                <p className="text-xs text-[var(--text-muted)] mt-0.5">{moduleT('contextMenu.invalid')}</p>
              </div>
              <div className="p-3 bg-[var(--brand-green-10)] rounded-xl text-center">
                <p className="text-xl font-bold text-[var(--brand-green)] tabular-nums">
                  {scanResult.scan_duration_ms}ms
                </p>
                <p className="text-xs text-[var(--text-muted)] mt-0.5">{moduleT('contextMenu.duration')}</p>
              </div>
            </div>

            {/* Action bar: filtering and count stay here; select-all and delete buttons move to the floating action area,
                sharing the same components as junk cleanup and other modules - buttons stay visible as the list scrolls */}
            <div className="flex items-center gap-3">
              {/* Show invalid entries only toggle */}
              <button
                onClick={() => setShowInvalidOnly((v) => !v)}
                className={`
                  text-xs px-2.5 py-1 rounded-lg border transition-colors
                  ${showInvalidOnly
                    ? 'bg-[var(--color-danger)]/10 text-[var(--color-danger)] border-[var(--color-danger)]/20'
                    : 'bg-[var(--bg-hover)] text-[var(--text-muted)] border-transparent'
                  }
                `}
              >
                {moduleT('contextMenu.invalidOnly')}
              </button>

              <span className="text-sm text-[var(--text-muted)]">
                {moduleT('contextMenu.selected', { count: selectedCount })}
              </span>

              {/* The only recovery method after accidental deletion is the .reg in the backup directory; put the entry here instead of hiding it in the settings page */}
              <button
                type="button"
                onClick={handleOpenBackupDir}
                className="ml-auto flex items-center gap-1 rounded-lg px-2 py-1 text-xs text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
              >
                <FolderOpen className="h-3.5 w-3.5" />
                {moduleT('contextMenu.openBackupDir')}
              </button>
            </div>

            {/* Action buttons area: fixed at the bottom of the viewport; no need to scroll back to find the delete button however long the list is */}
            <ModuleOperationToolbar
              moduleId="context"
              busy={isDeleting}
              busyContent={
                <ModuleDeleteProgress
                  variant="floating"
                  phaseLabel={moduleT('contextMenu.cleaning')}
                  progressLabel={moduleT('contextMenu.cleaningDesc', { count: selectedCount })}
                  hint={t('doNotCloseWindow')}
                />
              }
            >
              <button
                onClick={toggleSelectAll}
                className="module-operation-toolbar__button module-operation-toolbar__button--muted"
              >
                {allFilteredSelected ? t('deselectAll') : t('selectAll')}
              </button>
              <button
                onClick={() => setShowDeleteConfirm(true)}
                disabled={selectedCount === 0 || isDeleting}
                className="module-operation-toolbar__button module-operation-toolbar__button--danger"
              >
                {isDeleting
                  ? <Loader2 className="w-4 h-4 animate-spin" />
                  : <Trash2 className="w-4 h-4" />
                }
                {moduleT('contextMenu.deleteSelected')}
              </button>
            </ModuleOperationToolbar>

            {/* Error message */}
            {deleteError && (
              <div className="p-3 bg-[var(--color-danger)]/10 rounded-xl border border-[var(--color-danger)]/20">
                <div className="flex items-center justify-between">
                  <span className="text-sm text-[var(--color-danger)]">{deleteError}</span>
                  {deleteErrors.length > 0 && (
                    <button
                      onClick={() => setShowErrorDetails(!showErrorDetails)}
                      className="text-xs text-[var(--color-danger)] hover:underline"
                    >
                      {showErrorDetails ? t('close') : t('viewDetails')}
                    </button>
                  )}
                </div>
                {showErrorDetails && (
                  <div className="mt-2 pt-2 border-t border-[var(--color-danger)]/20 space-y-1 max-h-32 overflow-auto">
                    {deleteErrors.map((err, idx) => (
                      <p key={idx} className="text-xs text-[var(--color-danger)]/80 break-all">
                        • {err}
                      </p>
                    ))}
                  </div>
                )}
              </div>
            )}

            {/* Entry list (grouped by scope) */}
            {filteredEntries.length > 0 ? (
              <div className="space-y-3">
                {Array.from(groupedEntries.entries()).map(([scope, entries]) => {
                  const isCollapsed = collapsedScopes.has(scope);
                  const scopeStyle = getScopeStyle(scope, moduleT);

                  return (
                    <div key={scope} className="space-y-1.5">
                      {/* Group title */}
                      <button
                        onClick={() => toggleScope(scope)}
                        className="flex items-center gap-2 w-full text-left px-1 py-0.5 hover:opacity-80 transition-opacity"
                      >
                        {isCollapsed
                          ? <ChevronRight className="w-3.5 h-3.5 text-[var(--text-faint)]" />
                          : <ChevronDown className="w-3.5 h-3.5 text-[var(--text-faint)]" />
                        }
                        <span className={`px-2 py-0.5 rounded text-[0.79rem] font-semibold ${scopeStyle.className}`}>
                          {scopeStyle.label}
                        </span>
                        <span className="text-xs text-[var(--text-faint)]">
                          {t('fileCount', { count: entries.length })}
                        </span>
                      </button>

                      {/* Entry list */}
                      {!isCollapsed && (
                        <div className="space-y-1.5 pl-4">
                          {entries.map((entry) => (
                            <EntryRow
                              key={entry.id}
                              entry={entry}
                              isSelected={selectedIds.has(entry.id)}
                              onToggle={toggleSelect}
                            />
                          ))}
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            ) : (
              /* No entries after filtering */
              <EmptyState
                icon={MousePointerClick}
                title={showInvalidOnly ? moduleT('contextMenu.noInvalid') : moduleT('contextMenu.noEntries')}
                description={showInvalidOnly ? moduleT('contextMenu.validOnly') : moduleT('contextMenu.empty')}
                tone={showInvalidOnly ? 'success' : 'neutral'}
                compact
              />
            )}
          </div>
        )}

        {/* Empty state: scan completed but no entries */}
        {scanResult && scanResult.entries.length === 0 && (
          <div className="p-5">
            <EmptyState
              icon={MousePointerClick}
                title={t('noContextMenuIssues')}
              description={moduleT('contextMenu.allValid')}
              tone="success"
            />
          </div>
        )}
      </ModuleCard>

      {/* Delete confirmation dialog */}
      <ConfirmDialog
        isOpen={showDeleteConfirm}
        onCancel={() => setShowDeleteConfirm(false)}
        onConfirm={handleDelete}
        title={t('confirmDeleteContextMenu')}
        description={moduleT('contextMenu.confirmDesc', { count: selectedCount })}
        // When HKLM entries are selected without elevation, put "this will fail" into the warning up front; more useful than erroring after deletion
        warning={
          selectedAdminCount > 0
            ? moduleT('contextMenu.adminBlockedWarning', { count: selectedAdminCount })
            : t('contextMenuDeleteWarning')
        }
        confirmText={t('delete')}
        cancelText={t('cancel')}
        isDanger={true}
      />
    </>
  );
}

export default ContextMenuModule;
