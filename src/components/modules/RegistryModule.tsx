import { useAiModuleBridge } from '../ai/useAiModuleBridge';
// ============================================================================
// Registry leftover scan module
//
// Scans HKCR\Applications file-association references left by uninstalled programs.
// Every output meets the ironclad criteria (associated file missing + non-system path + non-system process),
// selected by default, one-click delete.
// ============================================================================

import { useState, useCallback, useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { Database, Loader2, Trash2, CheckCircle2, Shield } from 'lucide-react';
import { ModuleCard } from '../ModuleCard';
import { ModuleDeleteProgress } from '../ModuleDeleteProgress';
import { ModuleScanProgress } from '../ModuleScanProgress';
import { ConfirmDialog } from '../ConfirmDialog';
import { EmptyState } from '../EmptyState';
import { useModuleDashboard } from '../../contexts/DashboardContext';
import {
  scanRegistryRedundancy,
  deleteRegistryEntries,
  openRegistryBackupDir,
  recordCleanupAction,
  type RegistryScanResult,
  type RegistryEntry,
  type CleanupLogEntryInput,
} from '../../api/commands';
import type { ModuleRenderProps } from './moduleProps';

// ============================================================================
// Main component
// ============================================================================

export function RegistryModule({ isPageActive = true }: ModuleRenderProps) {
  const { t: navT } = useTranslation('nav');
  const { t } = useTranslation('common');
  const { t: moduleT } = useTranslation('modules');
  const { moduleState, updateModuleState, triggerHealthRefresh, oneClickScanTrigger } = useModuleDashboard('registry');

  const lastScanTriggerRef = useRef(0);

  const [scanResult, setScanResult] = useState<RegistryScanResult | null>(null);
  // Hook up the AI assistant (read-only): provide a summary only, no action tools.
  useAiModuleBridge('registry', {
    // Read-only: lets the assistant "look further" at what entries exist (one page at a time; the model decides whether to continue).
    readItems: (_targetId, offset, limit) => {
      const list = scanResult?.entries ?? [];
      if (list.length === 0) return null;
      return {
        total: list.length,
        lines: list.slice(offset, offset + limit).map(
          (item) => `${item.path}  ${item.name}${item.issue ? `  （问题：${item.issue}）` : ''}`,
        ),
      };
    },
    // Read-only: single-item details. itemId uses the registry path at the start of the read_items line (that is how the line begins).
    readItemDetail: (itemId) => {
      const wanted = itemId.trim();
      if (!wanted) return null;
      const list = scanResult?.entries ?? [];
      const item =
        list.find((entry) => entry.path.trim() === wanted) ??
        list.find((entry) => entry.name.trim() === wanted);
      if (!item) return null;
      return [
        `名称：${item.name}`,
        `注册表路径：${item.path}`,
        `引用的程序路径：${item.associated_path || '（未记录）'}`,
        `判定问题：${item.issue || '（未记录）'}`,
        '结论：这类残留指向的程序已经不在，清理只影响"右键打开方式/应用列表"里的失效条目；'
          + '删除前程序会自动备份注册表，可回退。',
      ].join('\n');
    },
    summary: () => {
      if (!scanResult) return null;
      return `扫描到 ${scanResult.entries.length} 项可疑的注册表残留，其中 ${scanResult.total_count} 项被判定为可安全清理。`;
    },
  });

  const [selectedEntries, setSelectedEntries] = useState<Set<string>>(new Set());
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState<string | null>(null);
  const [deleteErrors, setDeleteErrors] = useState<string[]>([]);
  const [showErrorDetails, setShowErrorDetails] = useState(false);
  const [backupPath, setBackupPath] = useState<string | null>(null);

  const selectedCount = selectedEntries.size;

  const handleScan = useCallback(async () => {
    updateModuleState('registry', { status: 'scanning', error: null });
    setScanResult(null);
    setSelectedEntries(new Set());
    setDeleteError(null);
    setDeleteErrors([]);
    setShowErrorDetails(false);
    setBackupPath(null);

    try {
      const result = await scanRegistryRedundancy();
      setScanResult(result);

      // All entries have passed the ironclad criteria; select all by default
      setSelectedEntries(new Set(result.entries.map(e => `${e.path}|${e.name}`)));

      updateModuleState('registry', {
        status: 'done',
        fileCount: result.total_count,
        totalSize: 0,
      });
    } catch (err) {
      console.error('注册表扫描失败:', err);
      updateModuleState('registry', { status: 'error', error: String(err) });
    }
  }, [updateModuleState]);

  // One-click scan trigger
  useEffect(() => {
    if (oneClickScanTrigger > 0 && oneClickScanTrigger !== lastScanTriggerRef.current) {
      lastScanTriggerRef.current = oneClickScanTrigger;
      handleScan();
    }
  }, [oneClickScanTrigger, handleScan]);

  const handleDelete = useCallback(async () => {
    if (selectedEntries.size === 0 || !scanResult) return;

    setIsDeleting(true);
    setDeleteError(null);
    setDeleteErrors([]);
    setShowErrorDetails(false);

    try {
      const entriesToDelete = scanResult.entries.filter(
        e => selectedEntries.has(`${e.path}|${e.name}`)
      );

      const result = await deleteRegistryEntries(entriesToDelete);
      setBackupPath(result.backup_path);

      // Cleanup log
      const failedSet = new Set(result.failed_entries);
      const logEntries: CleanupLogEntryInput[] = entriesToDelete.map((entry, index) => {
        const errorMsg = index < result.errors.length ? result.errors[index] : undefined;
        return {
          category: '注册表冗余',
          path: `${entry.path}\\${entry.name}`,
          size: 0,
          success: !failedSet.has(entry.path),
          error_message: failedSet.has(entry.path) ? errorMsg : undefined,
        };
      });
      recordCleanupAction(logEntries).catch((err) => {
        console.warn('记录清理日志失败:', err);
      });

      if (result.errors.length > 0) {
        setDeleteError(`${result.errors.length} ${t('files')} ${t('errorState')}`);
        setDeleteErrors(result.errors);
      }

      const remainingEntries = scanResult.entries.filter(
        e => !selectedEntries.has(`${e.path}|${e.name}`) || failedSet.has(e.path)
      );

      setScanResult({
        ...scanResult,
        entries: remainingEntries,
        total_count: remainingEntries.length,
      });

      const newSelected = new Set(
        Array.from(selectedEntries).filter(key => {
          const path = key.split('|').slice(0, -1).join('|');
          return failedSet.has(path);
        })
      );
      setSelectedEntries(newSelected);

      updateModuleState('registry', { fileCount: remainingEntries.length });
      triggerHealthRefresh();
    } catch (err) {
      console.error('删除失败:', err);
      setDeleteError(String(err));
      setDeleteErrors([String(err)]);
    } finally {
      setIsDeleting(false);
      setShowDeleteConfirm(false);
    }
  }, [selectedEntries, scanResult, updateModuleState, triggerHealthRefresh]);

  const toggleSelect = useCallback((entry: RegistryEntry) => {
    const key = `${entry.path}|${entry.name}`;
    setSelectedEntries(prev => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  const toggleSelectAll = useCallback(() => {
    if (!scanResult) return;
    if (selectedEntries.size === scanResult.entries.length) {
      setSelectedEntries(new Set());
    } else {
      setSelectedEntries(new Set(scanResult.entries.map(e => `${e.path}|${e.name}`)));
    }
  }, [scanResult, selectedEntries]);

  if (!isPageActive && !isDeleting && !showDeleteConfirm) {
    return null;
  }

  return (
    <>
      <ModuleCard
        id="registry"
        title={navT('registry')}
        description={navT('registryDesc')}
        icon={<Database className="w-6 h-6 text-[var(--brand-green)]" />}
        status={moduleState.status}
        fileCount={moduleState.fileCount}
        totalSize={0}
        onScan={handleScan}
        error={moduleState.error}
      >
        {/* Deletion progress replaces the delete button in the operation row in place (see below), keeping the result-area reference,
            without taking extra document flow — the registry has no floating toolbar available. */}
        {moduleState.status === 'idle' && !scanResult && (
          <div className="p-5">
            <EmptyState
              icon={Database}
              title={t('notScannedRegistry')}
              description={moduleT('registry.idleDesc')}
            />
          </div>
        )}

        {moduleState.status === 'scanning' && !scanResult && (
          <div className="p-5">
            <ModuleScanProgress
              title={t('scanningShort')}
              description={moduleT('registry.idleDesc')}
              icon={<Loader2 className="h-7 w-7 animate-spin text-[var(--brand-green)]" />}
            />
          </div>
        )}

        {scanResult && scanResult.entries.length > 0 && (
          <div className="p-5 space-y-4">
            {/* Safety hint */}
            <div className="flex items-start justify-between gap-3 p-4 bg-emerald-500/5 rounded-xl border border-emerald-500/15">
              <div className="flex items-start gap-3">
                <Shield className="w-5 h-5 text-emerald-600 shrink-0 mt-0.5" />
                <div>
                  <p className="text-sm font-medium text-[var(--text-primary)]">{moduleT('registry.result')}</p>
                  <p className="text-xs text-[var(--text-muted)] mt-1">
                    {t('registryDeleteWarning')}
                  </p>
                </div>
              </div>
              <button
                onClick={() => openRegistryBackupDir()}
                className="shrink-0 px-3 py-1.5 text-xs font-medium text-emerald-600 bg-white/50 hover:bg-white/80 rounded-lg border border-emerald-500/15 transition-colors"
              >
                {t('open')}
              </button>
            </div>

            {backupPath && (
              <div className="glass-block flex items-center justify-between p-3 bg-[var(--bg-main)] rounded-xl">
                <span className="text-xs text-[var(--text-muted)]">
                  {moduleT('registry.backup')}: {backupPath}
                </span>
                <button
                  onClick={() => openRegistryBackupDir()}
                  className="text-xs text-emerald-600 hover:underline"
                >
                  {t('open')}
                </button>
              </div>
            )}

            {/* Operation bar */}
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-3">
                <button onClick={toggleSelectAll} className="text-sm text-[var(--brand-green)] hover:underline">
                  {selectedEntries.size === scanResult.entries.length ? t('deselectAll') : t('selectAll')}
                </button>
                <span className="text-sm text-[var(--text-muted)]">
                  {moduleT('registry.selected', { count: selectedCount })} / {scanResult.entries.length}
                </span>
              </div>
              {/* During deletion, replace the button with progress in place: the registry has no floating toolbar,
                  placing it here neither shifts the result area nor loses the user's current viewing position. */}
              {isDeleting ? (
                <ModuleDeleteProgress
                  variant="floating"
                  phaseLabel={moduleT('registry.cleaning')}
                  progressLabel={moduleT('registry.cleaningDesc', { count: selectedCount })}
                />
              ) : (
                <button
                  onClick={() => setShowDeleteConfirm(true)}
                  disabled={selectedCount === 0}
                  className={`flex items-center gap-2 px-4 py-2 rounded-xl text-sm font-medium transition-colors ${
                    selectedCount === 0
                      ? 'bg-[var(--bg-hover)] text-[var(--text-faint)] cursor-not-allowed'
                      : 'bg-red-500 text-white hover:bg-red-600'
                  }`}
                >
                  <Trash2 className="w-4 h-4" />
                  {moduleT('registry.deleteSelected')}
                </button>
              )}
            </div>

            {deleteError && (
              <div className="p-3 bg-red-500/10 rounded-xl border border-red-500/20">
                <div className="flex items-center justify-between">
                  <span className="text-sm text-red-500">{deleteError}</span>
                  {deleteErrors.length > 0 && (
                    <button
                      onClick={() => setShowErrorDetails(!showErrorDetails)}
                      className="text-xs text-red-500 hover:underline"
                    >
                      {showErrorDetails ? t('close') : t('viewDetails')}
                    </button>
                  )}
                </div>
                {showErrorDetails && deleteErrors.length > 0 && (
                  <div className="mt-2 pt-2 border-t border-red-500/20 space-y-1 max-h-32 overflow-auto">
                    {deleteErrors.map((err, idx) => (
                      <p key={idx} className="text-xs text-red-500/80 break-all">{err}</p>
                    ))}
                  </div>
                )}
              </div>
            )}

            {/* Entry list */}
            <div className="space-y-2">
              {scanResult.entries.map((entry) => {
                const key = `${entry.path}|${entry.name}`;
                const isSelected = selectedEntries.has(key);

                return (
                  <div
                    key={key}
                    className={`p-4 rounded-xl transition-colors ${
                      isSelected
                        ? 'bg-emerald-500/5 cursor-pointer'
                        : 'bg-[var(--bg-main)] hover:bg-[var(--bg-hover)] cursor-pointer'
                    }`}
                    onClick={() => toggleSelect(entry)}
                  >
                    <div className="flex items-center gap-3">
                      {/* Checkbox */}
                      <div
                        className={`w-5 h-5 rounded border-2 flex items-center justify-center shrink-0 ${
                          isSelected
                            ? 'bg-emerald-500 border-emerald-500'
                            : 'border-[var(--text-faint)]'
                        }`}
                      >
                        {isSelected && (
                          <svg className="w-3 h-3 text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={3}>
                            <path strokeLinecap="round" strokeLinejoin="round" d="M5 13l4 4L19 7" />
                          </svg>
                        )}
                      </div>

                      {/* Info */}
                      <div className="flex-1 min-w-0">
                        <div className="flex items-center gap-2">
                          <p className="text-sm font-medium text-[var(--text-primary)] truncate">{entry.name}</p>
                        </div>
                        <p className="text-xs text-[var(--text-muted)] truncate mt-0.5" title={entry.path}>
                          {entry.path}
                        </p>
                        <p className="text-xs text-[var(--text-faint)] mt-1">{entry.issue}</p>
                        <p className="text-[0.71rem] text-[var(--text-faint)] mt-0.5 truncate">
                          {entry.associated_path}
                        </p>
                      </div>
                    </div>
                  </div>
                );
              })}
            </div>
          </div>
        )}

        {scanResult && scanResult.entries.length === 0 && (
          <div className="p-5">
            <EmptyState
              icon={CheckCircle2}
                title={t('noRegistryRedundancy')}
              description={moduleT('registry.noResult')}
              tone="success"
            />
          </div>
        )}
      </ModuleCard>

      <ConfirmDialog
        isOpen={showDeleteConfirm}
        onCancel={() => setShowDeleteConfirm(false)}
        onConfirm={handleDelete}
        title={t('confirmDeleteRegistry')}
        description={moduleT('registry.confirmDesc', { count: selectedCount })}
        warning={t('registryDeleteWarning')}
        confirmText={t('delete')}
        cancelText={t('cancel')}
        isDanger={true}
      />
    </>
  );
}

export default RegistryModule;
