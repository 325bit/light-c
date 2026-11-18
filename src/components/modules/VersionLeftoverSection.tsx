/**
 * Multi-version leftover section (live in the "leftover & cache" page).
 *
 * Why it is a section instead of its own page: it answers the same question the page already asks —
 * "what did software leave behind" — and the answer is a handful of version directories sitting next to each other
 * (Edge / WebView2 / Copilot / GitHub Desktop), only one of which is in use.
 *
 * Why the action row is **inside** the section instead of the page's floating toolbar:
 * these directories are deleted straight away (`remove_dir_all`, no recycle bin) and go through the dev-cache backend's
 * path validation, while the entries above the section go through the leftover backend (which can skip executables and
 * supports a separate "deep clean"). Merging the two into one button would mean one confirm dialog explaining two
 * different deletion contracts, and one click that can half-succeed in two different ways.
 *
 * The section owns its selection and deletion; the parent only receives "these paths are gone" so it can keep the
 * module card's file/size counters in sync.
 */
import { useCallback, useMemo, useState } from 'react';
import { AnimatePresence, motion } from 'framer-motion';
import { useTranslation } from 'react-i18next';
import { ChevronRight, FolderOpen, Layers, Trash2 } from 'lucide-react';
import { ConfirmDialog } from '../ConfirmDialog';
import { SelectBox } from '../ui/SelectBox';
import { useToast } from '../Toast';
import { openInFolder, recordCleanupAction, type CleanupLogEntryInput } from '../../api/commands';
import { deleteDevCachePaths } from '../../api/devCache';
import type { VersionGroup } from '../../types/devCache';
import { formatSize } from '../../utils/format';
import { errorText } from '../../api/errors';

export interface VersionLeftoverSectionProps {
  groups: VersionGroup[];
  /** Total of every version directory (including the one in use) */
  totalSize: number;
  /** Total excluding the version currently in use */
  reclaimableSize: number;
  /** Version directories skipped while measuring (permissions / in use); 0 hides the hint */
  skipped?: number;
  /** Called with the paths that were actually deleted, so the parent can update module stats */
  onDeleted: (deletedPaths: string[]) => void;
}

export function VersionLeftoverSection({
  groups,
  totalSize,
  reclaimableSize,
  skipped = 0,
  onDeleted,
}: VersionLeftoverSectionProps) {
  const { t } = useTranslation('modules');
  const { showToast } = useToast();

  /** Expanded card (accordion: only one open at a time) */
  const [expanded, setExpanded] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [deleting, setDeleting] = useState(false);

  const versionCount = useMemo(
    () => groups.reduce((sum, group) => sum + group.entries.length, 0),
    [groups],
  );

  /** Path → size, so the checked total does not need to walk every group on each render */
  const sizeByPath = useMemo(() => {
    const map = new Map<string, number>();
    groups.forEach((group) => group.entries.forEach((entry) => map.set(entry.path, entry.size)));
    return map;
  }, [groups]);

  const selectedSize = useMemo(() => {
    let total = 0;
    selected.forEach((path) => {
      total += sizeByPath.get(path) ?? 0;
    });
    return total;
  }, [selected, sizeByPath]);

  /** Old versions only: the version in use never gets a checkbox, so it can never be selected or counted */
  const selectablePaths = useMemo(
    () =>
      groups.flatMap((group) =>
        group.entries.filter((entry) => !entry.isCurrent).map((entry) => entry.path),
      ),
    [groups],
  );

  const allSelected =
    selectablePaths.length > 0 && selectablePaths.every((path) => selected.has(path));

  const toggleEntry = useCallback((path: string, isCurrent: boolean) => {
    if (isCurrent) return;
    setSelected((previous) => {
      const next = new Set(previous);
      if (next.has(path)) {
        next.delete(path);
      } else {
        next.add(path);
      }
      return next;
    });
  }, []);

  const toggleGroup = useCallback((paths: string[]) => {
    setSelected((previous) => {
      const next = new Set(previous);
      const everySelected = paths.length > 0 && paths.every((path) => next.has(path));
      paths.forEach((path) => (everySelected ? next.delete(path) : next.add(path)));
      return next;
    });
  }, []);

  const toggleSelectAll = useCallback(() => {
    setSelected((previous) => {
      const everySelected =
        selectablePaths.length > 0 && selectablePaths.every((path) => previous.has(path));
      return everySelected ? new Set() : new Set(selectablePaths);
    });
  }, [selectablePaths]);

  const handleDelete = useCallback(async () => {
    setConfirmOpen(false);
    if (selected.size === 0) return;

    setDeleting(true);
    const paths = Array.from(selected);
    try {
      const result = await deleteDevCachePaths(paths);

      // The backend reports failures per path; only the rest may leave the list, otherwise the UI would hide
      // directories that are still on disk (typically versions locked by a running browser).
      const failedPaths = new Set(result.failed.map((failure) => failure.path));
      const deletedPaths = paths.filter((path) => !failedPaths.has(path));

      // Cleanup log: same contract as the leftover list above, so the history page shows one page, one category
      const logEntries: CleanupLogEntryInput[] = paths.map((path) => ({
        category: '残留&缓存',
        path,
        size: sizeByPath.get(path) ?? 0,
        success: !failedPaths.has(path),
      }));
      recordCleanupAction(logEntries).catch((error) => {
        console.warn('记录清理日志失败:', error);
      });

      if (result.deleted > 0) {
        showToast({
          type: 'success',
          title: t('leftoversVersion.deletedTitle', { count: result.deleted }),
          description: t('leftoversVersion.deletedDesc', { size: formatSize(result.freed) }),
        });
      }
      if (result.failed.length > 0) {
        showToast({
          type: result.deleted > 0 ? 'warning' : 'error',
          title:
            result.deleted > 0
              ? t('leftoversVersion.deletePartialTitle', {
                  success: result.deleted,
                  failed: result.failed.length,
                })
              : t('leftoversVersion.deleteFailed'),
          description:
            result.deleted > 0
              ? t('leftoversVersion.deletePartialDesc')
              : t('leftoversVersion.deleteFailedDesc'),
        });
      }

      setSelected(new Set(result.failed.map((failure) => failure.path)));
      if (deletedPaths.length > 0) onDeleted(deletedPaths);
    } catch (error) {
      console.error('删除旧版本目录失败:', error);
      showToast({
        type: 'error',
        title: t('leftoversVersion.deleteFailed'),
        description: errorText(error),
      });
    } finally {
      setDeleting(false);
    }
  }, [onDeleted, selected, showToast, sizeByPath, t]);

  /** Group header checkbox: "select the whole group's old versions"; stop propagation so it does not also collapse the card */
  const renderGroupCheckbox = (paths: string[]) => {
    const selectedCount = paths.filter((path) => selected.has(path)).length;
    const everySelected = paths.length > 0 && selectedCount === paths.length;
    return (
      <div
        onClick={(event) => {
          event.stopPropagation();
          toggleGroup(paths);
        }}
        className={paths.length > 0 ? 'cursor-pointer' : ''}
      >
        <SelectBox checked={everySelected} partial={selectedCount > 0 && !everySelected} />
      </div>
    );
  };

  return (
    <section className="space-y-3">
      {/* Section header: always shown after a scan, even with 0 groups — otherwise users cannot tell
          whether "no multi-version leftovers" was actually checked or the feature is missing */}
      <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1 px-1">
        <Layers className="h-4 w-4 shrink-0 translate-y-0.5 text-[var(--brand-green)]" />
        <h3 className="text-[0.96rem] font-bold text-[var(--text-primary)]">
          {t('leftoversVersion.title')}
        </h3>
        {versionCount > 0 && (
          <span className="text-[0.82rem] text-[var(--text-muted)]">
            {t('leftoversVersion.headerSummary', {
              count: versionCount,
              total: formatSize(totalSize),
              reclaimable: formatSize(reclaimableSize),
            })}
          </span>
        )}
      </div>

      {skipped > 0 && versionCount > 0 && (
        <p className="px-1 text-[0.78rem] text-[var(--text-faint)]">
          {t('leftoversVersion.skipped', { count: skipped })}
        </p>
      )}

      {versionCount === 0 ? (
        <p className="rounded-xl bg-[var(--bg-main)] px-4 py-3 text-[0.86rem] text-[var(--text-muted)]">
          {t('leftoversVersion.empty')}
        </p>
      ) : (
        <>
          {groups.map((group) => {
            const selectable = group.entries
              .filter((entry) => !entry.isCurrent)
              .map((entry) => entry.path);
            const isOpen = expanded === group.id;

            return (
              <div key={group.id} className="overflow-hidden rounded-2xl bg-[var(--bg-card)] shadow-sm">
                <div
                  className="flex cursor-pointer items-center gap-4 px-5 py-4 transition-colors hover:bg-[var(--bg-hover)]"
                  onClick={() => setExpanded(isOpen ? null : group.id)}
                >
                  <ChevronRight
                    className={`h-5 w-5 shrink-0 text-[var(--text-faint)] transition-transform ${isOpen ? 'rotate-90' : ''}`}
                  />
                  {renderGroupCheckbox(selectable)}
                  <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-[var(--brand-green-10)] text-[var(--brand-green)]">
                    <Layers className="h-5 w-5" />
                  </div>
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-[1rem] font-bold text-[var(--text-primary)]">
                      {group.name}
                    </p>
                    {/* Show the base directory: the same software may be installed twice (system drive + user drive),
                        a name alone would leave users unable to tell which copy is being deleted.
                        The old "N versions · reclaimable X" line does not carry the path, so it moves to `title`. */}
                    <p
                      className="mt-1 truncate text-[0.93rem] text-[var(--text-muted)]"
                      title={group.basePath}
                    >
                      {t('leftoversVersion.groupSummary', {
                        count: group.entries.length,
                        reclaimable: formatSize(group.reclaimableSize),
                      })}
                      <span className="text-[var(--text-faint)]"> · {group.basePath}</span>
                    </p>
                  </div>
                  <p className="shrink-0 text-[1.07rem] font-bold tabular-nums text-[var(--text-primary)]">
                    {formatSize(group.totalSize)}
                  </p>
                </div>

                <AnimatePresence initial={false}>
                  {isOpen && (
                    <motion.div
                      className="overflow-hidden border-t border-[var(--border-color)] bg-[var(--bg-main)]"
                      initial={{ height: 0, opacity: 0 }}
                      animate={{ height: 'auto', opacity: 1 }}
                      exit={{ height: 0, opacity: 0 }}
                      transition={{ duration: 0.24, ease: [0.22, 1, 0.36, 1] }}
                    >
                      {/* Height cap + internal scrolling: a machine that never cleaned up can hold a dozen versions */}
                      <div className="max-h-[15em] overflow-y-auto px-5 py-2">
                        {group.entries.map((entry) => (
                          <div
                            key={entry.path}
                            className={`flex items-center gap-3 py-2 text-[0.86rem] ${
                              entry.isCurrent ? 'cursor-default' : 'cursor-pointer'
                            }`}
                            onClick={() => toggleEntry(entry.path, entry.isCurrent)}
                          >
                            {entry.isCurrent ? (
                              /* No checkbox for the version in use, but keep a placeholder for alignment */
                              <div className="h-5 w-5 shrink-0" />
                            ) : (
                              <SelectBox checked={selected.has(entry.path)} />
                            )}
                            <span className="w-36 shrink-0 truncate font-medium tabular-nums text-[var(--text-secondary)]">
                              {entry.version}
                            </span>
                            {entry.isCurrent && (
                              <span className="shrink-0 rounded bg-[var(--brand-green-10)] px-1.5 py-0.5 text-[0.72rem] font-medium text-[var(--brand-green)]">
                                {t('leftoversVersion.currentTag')}
                              </span>
                            )}
                            {/* current_reason explains "why this version is judged as in use",
                                without it users can only trust the version number — which may not be the truth */}
                            <span
                              className="min-w-0 flex-1 truncate text-[var(--text-muted)]"
                              title={`${entry.path}\n${entry.currentReason}`}
                            >
                              {entry.isCurrent
                                ? entry.currentReason
                                : t('leftoversVersion.files', {
                                    count: entry.fileCount.toLocaleString(),
                                  })}
                            </span>
                            <span className="w-20 shrink-0 text-right tabular-nums text-[var(--text-muted)]">
                              {formatSize(entry.size)}
                            </span>
                            <button
                              type="button"
                              onClick={(event) => {
                                event.stopPropagation();
                                openInFolder(entry.path);
                              }}
                              title={t('leftoversVersion.openFolder')}
                              className="shrink-0 rounded p-0.5 text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--brand-green)]"
                            >
                              <FolderOpen className="h-4 w-4" />
                            </button>
                          </div>
                        ))}
                      </div>
                    </motion.div>
                  )}
                </AnimatePresence>
              </div>
            );
          })}

          {/* Inline action row: stays with the section (see the file header for why it is not merged into the floating toolbar) */}
          {selectablePaths.length > 0 && (
            <div className="flex flex-wrap items-center gap-2 rounded-xl bg-[var(--bg-main)] px-4 py-3">
              <button
                type="button"
                onClick={toggleSelectAll}
                disabled={deleting}
                className="rounded-lg px-3 py-1.5 text-[0.86rem] font-medium text-[var(--text-secondary)] transition-colors hover:bg-[var(--bg-hover)] disabled:opacity-50"
              >
                {allSelected
                  ? t('leftoversVersion.deselectAll')
                  : t('leftoversVersion.selectAll')}
              </button>
              <span className="whitespace-nowrap px-1 text-[0.82rem] tabular-nums text-[var(--text-muted)]">
                {t('leftoversVersion.selected', {
                  count: selected.size,
                  size: formatSize(selectedSize),
                })}
              </span>
              <button
                type="button"
                onClick={() => setConfirmOpen(true)}
                disabled={selected.size === 0 || deleting}
                className="ml-auto inline-flex items-center gap-1.5 rounded-lg bg-[var(--color-danger)] px-3 py-1.5 text-[0.86rem] font-medium text-white transition-opacity hover:opacity-90 disabled:opacity-40"
              >
                <Trash2 className="h-3.5 w-3.5" />
                {t('leftoversVersion.delete', { count: selected.size })}
              </button>
            </div>
          )}
        </>
      )}

      <ConfirmDialog
        isOpen={confirmOpen}
        title={t('leftoversVersion.confirmTitle', { count: selected.size })}
        description={t('leftoversVersion.confirmDesc', { size: formatSize(selectedSize) })}
        warning={t('leftoversVersion.confirmWarning')}
        confirmText={t('leftoversVersion.delete', { count: selected.size })}
        isDanger
        onConfirm={handleDelete}
        onCancel={() => setConfirmOpen(false)}
      />
    </section>
  );
}

export default VersionLeftoverSection;
