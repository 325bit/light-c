/**
 * Detail modal for a cache directory.
 *
 * Why build a separate one instead of reusing the social cleanup's: the data shapes differ (there, files carry risk levels
 * for social files; here it is just a path + size), so forcing it in would require adding many optional fields.
 *
 * Why virtual scrolling: a single cache directory may contain tens of thousands of files (WorkBuddy's logs alone have over a thousand),
 * rendering them all at once would freeze the modal. Virtualize by row, rendering only the dozen or so rows in the viewport.
 */
import { useEffect, useMemo, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';
import { AnimatePresence, motion } from 'framer-motion';
import { useVirtualizer } from '@tanstack/react-virtual';
import { FolderOpen, X } from 'lucide-react';
import { listAiCacheFiles } from '../../api/aiToolCleanup';
import { openInFolder } from '../../api/commands';
import type { AiCacheFileList } from '../../types/aiToolCleanup';
import { FileSortControl } from '../FileSortControl';
import { sortFileList, type TreeSort } from '../../utils/fileTree';
import { formatDate, formatSize } from '../../utils/format';

interface AiCacheFileModalProps {
  isOpen: boolean;
  /** Cache directory to expand; null means closed */
  dirPath: string | null;
  dirName: string;
  onClose: () => void;
}

export function AiCacheFileModal({ isOpen, dirPath, dirName, onClose }: AiCacheFileModalProps) {
  const { t } = useTranslation('modules');
  // The backend may truncate (`truncated`) when a directory is too large; when truncated, the user must be told,
  // otherwise "file count x size" would not match the directory's actual usage and would be mistaken for a statistics error.
  const [listing, setListing] = useState<AiCacheFileList>({ files: [], truncated: false });
  const files = listing.files;
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');
  const [sort, setSort] = useState<TreeSort>({ key: 'name', desc: false });
  const scrollRef = useRef<HTMLDivElement>(null);

  /*
    Sorting criteria are exactly the same as the "view all files" directory tree modal: natural order for names, bytes for sizes,
    Unix seconds for time; entries without a timestamp stay at the bottom in both ascending and descending order, never posing as "newest".
    There is no directory hierarchy here, so sort the array directly without building a tree.
  */
  const sortedFiles = useMemo(
    () => sortFileList(files, sort, {
      // Sorting by name uses the filename, not the full path: within one directory all filenames share the same prefix,
      // comparing full paths would be no sort at all. Take the last segment.
      getName: (file) => {
        const separator = Math.max(file.path.lastIndexOf('\\'), file.path.lastIndexOf('/'));
        return separator >= 0 ? file.path.slice(separator + 1) : file.path;
      },
      getSize: (file) => file.size,
      getTime: (file) => file.modifiedTime ?? 0,
    }),
    [files, sort],
  );

  useEffect(() => {
    if (!isOpen || !dirPath) {
      setListing({ files: [], truncated: false });
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError('');
    listAiCacheFiles(dirPath)
      .then((next) => {
        if (!cancelled) setListing(next);
      })
      .catch((loadError) => {
        if (!cancelled) setError(String(loadError));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    // Discard in-flight results when the modal closes or the directory changes, to avoid stale data overwriting the new directory
    return () => {
      cancelled = true;
    };
  }, [isOpen, dirPath]);

  const virtualizer = useVirtualizer({
    count: sortedFiles.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 34,
    overscan: 8,
  });

  /**
   * Force a re-measure once data is in place.
   *
   * When the modal first mounts, the panel is still doing its entrance scale, so the scroll container measures 0 height,
   * and the virtualizer computes the visible range from 0 height -> rendering no rows on the first frame, appearing as "opens empty,
   * shows content only after scrolling". Just re-measure when the file list changes.
   */
  useEffect(() => {
    virtualizer.measure();
    // Depend only on the sorted list: the virtualizer is a new object each render, including it would cause infinite re-renders
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sortedFiles]);

  const totalSize = files.reduce((sum, file) => sum + file.size, 0);

  /**
   * ⚠️ Must portal to body.
   * The module container has a `transform` (entrance animation), so `position: fixed` degrades to `absolute` -
   * the overlay would cover only the module area and the modal would not be centered. The social cleanup detail modal does the same.
   */
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
      <motion.div className="absolute inset-0 bg-black/50 backdrop-blur-sm glass-backdrop" onClick={onClose} />
      <motion.div
        className="ai-cache-modal glass-dialog relative flex max-h-[80vh] w-full max-w-5xl flex-col overflow-hidden rounded-2xl border border-[var(--border-color)] bg-[var(--bg-elevated)] shadow-2xl"
        initial={{ opacity: 0, scale: 0.96, y: 12 }}
        animate={{ opacity: 1, scale: 1, y: 0 }}
        exit={{ opacity: 0, scale: 0.96, y: 12 }}
        transition={{ duration: 0.2, ease: [0.22, 1, 0.36, 1] }}
      >
        <div className="flex shrink-0 items-start justify-between border-b border-[var(--border-color)] px-6 py-4">
          <div className="min-w-0">
            <h3 className="truncate text-lg font-semibold text-[var(--text-primary)]">{dirName}</h3>
            <p className="mt-0.5 text-xs text-[var(--text-muted)]">
              {t('aiTools.fileSummary', {
                count: files.length.toLocaleString(),
                size: formatSize(totalSize),
              })}
              {listing.truncated && (
                <span className="ml-2 text-[var(--text-faint)]">
                  {t('aiTools.filesTruncated', { count: files.length.toLocaleString() })}
                </span>
              )}
            </p>
          </div>
          <div className="flex shrink-0 items-center gap-2">
            {/* The sort control is the same set as other "view all files" modals: name / size / modified time,
                clicking the same key again toggles ascending/descending. Column headers are not separately sortable, to avoid two conflicting sort mechanisms */}
            <FileSortControl sort={sort} onSortChange={setSort} />
            <button
              type="button"
              onClick={onClose}
              className="p-2 rounded-lg hover:bg-[var(--bg-hover)] transition-colors"
              aria-label={t('common:close')}
            >
              <X className="h-5 w-5 text-[var(--text-muted)]" />
            </button>
          </div>
        </div>

        {/* Header row: column widths align with data rows, the index column is centered */}
        <div className="flex shrink-0 items-center gap-4 border-b border-[var(--border-color)] bg-[var(--bg-hover)] px-6 py-2 text-xs font-medium text-[var(--text-muted)]">
          <span className="w-10 shrink-0 text-center">#</span>
          <span className="flex-1">{t('aiTools.columnPath')}</span>
          {/* The time column appears only when sorting by time: normally it takes no width, and provides a basis when sorting */}
          {sort.key === 'time' && (
            <span className="w-24 shrink-0 text-right">{t('aiTools.columnTime')}</span>
          )}
          <span className="w-24 shrink-0 text-right">{t('aiTools.columnSize')}</span>
          {/* Spacer: aligns with the "open folder" button in data rows */}
          <span className="w-5 shrink-0" />
        </div>

        <div ref={scrollRef} className="min-h-0 flex-1 overflow-auto">
          {loading ? (
            <div className="flex h-32 items-center justify-center text-sm text-[var(--text-muted)]">
              {t('aiTools.loadingFiles')}
            </div>
          ) : error ? (
            <div className="flex h-32 items-center justify-center px-6 text-center text-sm text-[var(--color-danger)]">
              {error}
            </div>
          ) : sortedFiles.length === 0 ? (
            <div className="flex h-32 items-center justify-center text-sm text-[var(--text-muted)]">
              {t('aiTools.noFiles')}
            </div>
          ) : (
            <div style={{ height: `${virtualizer.getTotalSize()}px`, position: 'relative' }}>
              {virtualizer.getVirtualItems().map((virtualRow) => {
                const file = sortedFiles[virtualRow.index];
                return (
                  <div
                    key={virtualRow.key}
                    className="absolute left-0 top-0 flex w-full items-center gap-4 px-6 py-2 text-xs transition-colors hover:bg-[var(--bg-hover)]"
                    style={{
                      height: `${virtualRow.size}px`,
                      transform: `translateY(${virtualRow.start}px)`,
                    }}
                  >
                    <span className="w-10 shrink-0 text-center tabular-nums text-[var(--text-faint)]">
                      {virtualRow.index + 1}
                    </span>
                    <span
                      className="min-w-0 flex-1 truncate text-[var(--text-secondary)]"
                      title={file.path}
                    >
                      {file.path}
                    </span>
                    {sort.key === 'time' && (
                      <span className="w-24 shrink-0 text-right tabular-nums text-[var(--text-faint)]">
                        {file.modifiedTime > 0 ? formatDate(file.modifiedTime) : '—'}
                      </span>
                    )}
                    <span className="w-24 shrink-0 text-right tabular-nums text-[var(--text-muted)]">
                      {formatSize(file.size)}
                    </span>
                    <button
                      type="button"
                      onClick={() => openInFolder(file.path)}
                      title={t('aiTools.openFolder')}
                      className="shrink-0 rounded p-0.5 text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--brand-green)]"
                    >
                      <FolderOpen className="h-4 w-4" />
                    </button>
                  </div>
                );
              })}
            </div>
          )}
        </div>

        {/* Bottom info bar: total count on the left, close button on the right */}
        <div className="flex shrink-0 items-center justify-between border-t border-[var(--border-color)] px-6 py-3 text-xs text-[var(--text-muted)]">
          <span>
            {t('aiTools.fileSummary', {
              count: files.length.toLocaleString(),
              size: formatSize(totalSize),
            })}
          </span>
          <button
            type="button"
            onClick={onClose}
            className="rounded-lg bg-[var(--brand-green)] px-4 py-1.5 text-xs font-medium text-white transition-opacity hover:opacity-90"
          >
            {t('common:close')}
          </button>
        </div>
      </motion.div>
    </motion.div>
      )}
    </AnimatePresence>,
    document.body,
  );
}
