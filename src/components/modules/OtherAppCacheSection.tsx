/**
 * Other-software-cache section (lives on the "leftover & cache" page).
 *
 * Scope: directories that neither Windows/browser junk cleanup nor driver cleanup touches — downloaded driver
 * installers (NVIDIA / Epic / DaVinci), launcher web caches (Steam, Epic), crash dumps (Overwolf) and so on.
 * They used to be the last group of the dev cache page; they moved here because they answer this page's question
 * ("what did software leave behind") and because they are not development tooling at all.
 *
 * Why the action row is **inside** the section instead of the page's floating toolbar — same reason as the
 * multi-version section: the entries above go through the leftover backend (which skips directories containing
 * executables and has a separate "deep clean"), while these go through the dev-cache backend's path validation and
 * are deleted straight away. One merged button would mean one confirm dialog explaining two deletion contracts.
 *
 * The section owns its selection and deletion; the parent only receives "these paths are gone" so the module card's
 * counters can stay in sync.
 */
import { useCallback, useMemo, useState } from 'react';
import { AnimatePresence, motion } from 'framer-motion';
import { useTranslation } from 'react-i18next';
import { ChevronRight, FolderOpen, HardDrive, Trash2 } from 'lucide-react';
import { ConfirmDialog } from '../ConfirmDialog';
import { LinkBadge } from '../LinkBadge';
import { SelectBox } from '../ui/SelectBox';
import { RiskTag } from '../ui/RiskTag';
import { useToast } from '../Toast';
import { openInFolder, recordCleanupAction, type CleanupLogEntryInput } from '../../api/commands';
import { deleteDevCachePaths } from '../../api/devCache';
import type { DevCacheGroup } from '../../types/devCache';
import { formatSize } from '../../utils/format';

export interface OtherAppCacheSectionProps {
  groups: DevCacheGroup[];
  /** Total of every entry (including "would lose data" ones) */
  totalSize: number;
  /** Entries with risk `cache`: what the user can delete without side effects */
  safeSize: number;
  /** Directories skipped while measuring (permissions / in use); 0 hides the hint */
  skipped?: number;
  /** Called with the paths that were actually deleted, so the parent can update module stats */
  onDeleted: (deletedPaths: string[]) => void;
}

export function OtherAppCacheSection({
  groups,
  totalSize,
  safeSize,
  skipped = 0,
  onDeleted,
}: OtherAppCacheSectionProps) {
  const { t } = useTranslation('modules');
  const { showToast } = useToast();

  /** Expanded card (accordion: only one open at a time) */
  const [expanded, setExpanded] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [deleting, setDeleting] = useState(false);

  const entryCount = useMemo(
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

  /** Read-only entries never get a checkbox (there are none in this group today, but the contract allows it) */
  const selectablePaths = useMemo(
    () =>
      groups.flatMap((group) =>
        group.entries.filter((entry) => entry.risk !== 'readonly').map((entry) => entry.path),
      ),
    [groups],
  );

  const allSelected =
    selectablePaths.length > 0 && selectablePaths.every((path) => selected.has(path));

  const toggleEntry = useCallback((path: string, risk: string) => {
    if (risk === 'readonly') return;
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

      // Report per path: launchers keep their cache files open while running, so partial failure is the normal case;
      // only the rest may leave the list, otherwise the UI would hide directories that are still on disk.
      const failedPaths = new Set(result.failed.map((failure) => failure.path));
      const deletedPaths = paths.filter((path) => !failedPaths.has(path));

      // Cleanup log: same category as the rest of the page, so the history page shows one page, one category
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
          title: t('leftoversOtherCache.deletedTitle', { count: result.deleted }),
          description: t('leftoversOtherCache.deletedDesc', { size: formatSize(result.freed) }),
        });
      }
      if (result.failed.length > 0) {
        showToast({
          type: result.deleted > 0 ? 'warning' : 'error',
          title:
            result.deleted > 0
              ? t('leftoversOtherCache.deletePartialTitle', {
                  success: result.deleted,
                  failed: result.failed.length,
                })
              : t('leftoversOtherCache.deleteFailed'),
          description:
            result.deleted > 0
              ? t('leftoversOtherCache.deletePartialDesc')
              : t('leftoversOtherCache.deleteFailedDesc'),
        });
      }

      setSelected(new Set(result.failed.map((failure) => failure.path)));
      if (deletedPaths.length > 0) onDeleted(deletedPaths);
    } catch (error) {
      console.error('删除其它软件缓存目录失败:', error);
      showToast({
        type: 'error',
        title: t('leftoversOtherCache.deleteFailed'),
        description: String(error),
      });
    } finally {
      setDeleting(false);
    }
  }, [onDeleted, selected, showToast, sizeByPath, t]);

  /** Group header checkbox: "select the whole group"; stop propagation so it does not also collapse the card */
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
      {/* Section header: always shown after a scan, even with 0 groups — otherwise users cannot tell whether
          "no other software caches" was actually checked or the feature is missing */}
      <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1 px-1">
        <HardDrive className="h-4 w-4 shrink-0 translate-y-0.5 text-[var(--brand-green)]" />
        <h3 className="text-[0.96rem] font-bold text-[var(--text-primary)]">
          {t('leftoversOtherCache.title')}
        </h3>
        {entryCount > 0 && (
          <span className="text-[0.82rem] text-[var(--text-muted)]">
            {t('leftoversOtherCache.headerSummary', {
              count: entryCount,
              total: formatSize(totalSize),
              safe: formatSize(safeSize),
            })}
          </span>
        )}
      </div>

      {skipped > 0 && entryCount > 0 && (
        <p className="px-1 text-[0.78rem] text-[var(--text-faint)]">
          {t('leftoversOtherCache.skipped', { count: skipped })}
        </p>
      )}

      {entryCount === 0 ? (
        <p className="rounded-xl bg-[var(--bg-main)] px-4 py-3 text-[0.86rem] text-[var(--text-muted)]">
          {t('leftoversOtherCache.empty')}
        </p>
      ) : (
        <>
          {groups.map((group) => {
            const selectable = group.entries
              .filter((entry) => entry.risk !== 'readonly')
              .map((entry) => entry.path);
            const isOpen = expanded === group.groupId;
            const cardKey = group.groupId || group.groupName;

            return (
              <div key={cardKey} className="overflow-hidden rounded-2xl bg-[var(--bg-card)] shadow-sm">
                <div
                  className="flex cursor-pointer items-center gap-4 px-5 py-4 transition-colors hover:bg-[var(--bg-hover)]"
                  onClick={() => setExpanded(isOpen ? null : group.groupId)}
                >
                  <ChevronRight
                    className={`h-5 w-5 shrink-0 text-[var(--text-faint)] transition-transform ${isOpen ? 'rotate-90' : ''}`}
                  />
                  {renderGroupCheckbox(selectable)}
                  <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-[var(--brand-green-10)] text-[var(--brand-green)]">
                    <HardDrive className="h-5 w-5" />
                  </div>
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-[1rem] font-bold text-[var(--text-primary)]">
                      {group.groupName}
                    </p>
                    <p className="mt-1 truncate text-[0.93rem] text-[var(--text-muted)]">
                      {t('leftoversOtherCache.groupSummary', {
                        dirs: group.entries.length,
                        files: group.fileCount.toLocaleString(),
                      })}
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
                      {/* Height cap + internal scrolling: a machine with many launchers can hold a dozen rows */}
                      <div className="max-h-[15em] overflow-y-auto px-5 py-2">
                        {group.entries.map((entry) => {
                          const isReadonly = entry.risk === 'readonly';
                          return (
                            <div
                              key={entry.path}
                              className={`flex items-center gap-3 py-2 text-[0.86rem] ${
                                isReadonly ? 'cursor-default' : 'cursor-pointer'
                              }`}
                              onClick={() => toggleEntry(entry.path, entry.risk)}
                            >
                              {isReadonly ? (
                                /* Placeholder for read-only rows: keeps the left alignment of the column */
                                <div className="h-5 w-5 shrink-0" />
                              ) : (
                                <SelectBox checked={selected.has(entry.path)} />
                              )}
                              <span className="w-36 shrink-0 truncate font-medium text-[var(--text-secondary)]">
                                {entry.name}
                              </span>
                              <RiskTag
                                risk={entry.risk}
                                label={
                                  isReadonly
                                    ? t('leftoversOtherCache.riskReadonly')
                                    : t('leftoversOtherCache.riskConfirm')
                                }
                              />
                              <LinkBadge link={entry.link} />
                              {/* note is "what happens if deleted", always shown; the path completes it via title */}
                              <span className="min-w-0 flex-1 text-[var(--text-muted)]">
                                <span
                                  className="block truncate"
                                  title={`${entry.path}\n${entry.note}`}
                                >
                                  {entry.note || entry.path}
                                </span>
                                {/* Relocated directories: the real data is on another drive, spell it out on its own
                                    line — a tooltip alone is as good as not showing it */}
                                {entry.link && (
                                  <span
                                    className="block truncate text-[0.78rem] text-[var(--brand-green)]"
                                    title={`${entry.link.linkPath} → ${entry.link.target}`}
                                  >
                                    {t('link.realPath')}：{entry.link.resolved}
                                  </span>
                                )}
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
                                title={t('leftoversOtherCache.openFolder')}
                                className="shrink-0 rounded p-0.5 text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--brand-green)]"
                              >
                                <FolderOpen className="h-4 w-4" />
                              </button>
                            </div>
                          );
                        })}
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
                  ? t('leftoversOtherCache.deselectAll')
                  : t('leftoversOtherCache.selectAll')}
              </button>
              <span className="whitespace-nowrap px-1 text-[0.82rem] tabular-nums text-[var(--text-muted)]">
                {t('leftoversOtherCache.selected', {
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
                {t('leftoversOtherCache.delete', { count: selected.size })}
              </button>
            </div>
          )}
        </>
      )}

      <ConfirmDialog
        isOpen={confirmOpen}
        title={t('leftoversOtherCache.confirmTitle', { count: selected.size })}
        description={t('leftoversOtherCache.confirmDesc', { size: formatSize(selectedSize) })}
        warning={t('leftoversOtherCache.confirmWarning')}
        confirmText={t('leftoversOtherCache.delete', { count: selected.size })}
        isDanger
        onConfirm={handleDelete}
        onCancel={() => setConfirmOpen(false)}
      />
    </section>
  );
}

export default OtherAppCacheSection;
