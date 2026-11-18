// ============================================================================
// Junk cleanup - "View All Files" modal
//
// Note: deep scan shows only 500 entries on the first screen per category, and the 256px window in the card
// can neither scroll through tens of thousands of files nor avoid making users click "Load More" page by page. Changed to a single click
// "View All Files", fetching the whole category result at once and displaying it as a full directory tree in this modal,
// sharing the same selection state as the module (after closing the modal, the card's selections and the bottom "clean" count stay consistent).
// ============================================================================

import { useCallback, useEffect, useMemo } from 'react';
import { createPortal } from 'react-dom';
import { AnimatePresence, motion } from 'framer-motion';
import { ExternalLink, FolderOpen, Search, Trash2, X } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { FileTreeList } from '../FileTreeList';
import { formatSize } from '../../utils/format';
import { openFile, openInFolder, openRecycleBin } from '../../api/commands';
import type { CategoryScanResult, FileInfo } from '../../types';

interface JunkFileTreeModalProps {
  /** Category to display; null means closed */
  category: CategoryScanResult | null;
  selectedPaths: Set<string>;
  onToggleFile: (path: string) => void;
  onSetSelectionForPaths: (paths: string[], selected: boolean) => void;
  onSearchFile: (file: FileInfo) => void;
  /** Request deletion: the module shows a unified confirmation dialog, with the same scope as the bottom toolbar */
  onRequestDelete: () => void;
  onClose: () => void;
}

export function JunkFileTreeModal({
  category,
  selectedPaths,
  onToggleFile,
  onSetSelectionForPaths,
  onSearchFile,
  onRequestDelete,
  onClose,
}: JunkFileTreeModalProps) {
  const { t } = useTranslation('junkClean');
  const { t: commonT } = useTranslation('common');

  // System-managed categories (like Windows.old) cannot be deleted in-app, so selection is disabled in the modal too
  const isSystemManaged = category?.system_managed === true;
  const isSelectable = useCallback(() => !isSystemManaged, [isSystemManaged]);

  // Esc to close: the modal covers the whole page, and a single X in the top-right is hard to find in a tens-of-thousands-row list
  useEffect(() => {
    if (!category) return;
    const handleKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [category, onClose]);

  const files = category?.files;
  const { selectedCount, selectedSize } = useMemo(() => {
    if (!files) return { selectedCount: 0, selectedSize: 0 };
    let count = 0;
    let size = 0;
    for (const file of files) {
      if (selectedPaths.has(file.path)) {
        count += 1;
        size += file.size;
      }
    }
    return { selectedCount: count, selectedSize: size };
  }, [files, selectedPaths]);

  const categoryName = category
    ? t(`categories.${category.category}.name`, { defaultValue: category.display_name })
    : '';

  // For recycle bin entries, "Open containing folder" should open the recycle bin, not the file's original directory
  const handleOpenLocation = useCallback((file: FileInfo) => {
    if (file.category === 'RecycleBin') openRecycleBin();
    else openInFolder(file.path);
  }, []);

  return createPortal(
    <AnimatePresence>
      {category && (
        <motion.div
          className="fixed inset-0 z-[9999] flex items-center justify-center p-4"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.18, ease: 'easeOut' }}
        >
          <div className="absolute inset-0 bg-black/50 backdrop-blur-sm glass-backdrop" onClick={onClose} />
          <motion.div
            className="glass-dialog relative flex max-h-[86vh] w-full max-w-5xl flex-col overflow-hidden rounded-2xl border border-[var(--border-default)] bg-[var(--bg-card)] shadow-2xl"
            initial={{ opacity: 0, scale: 0.96, y: 12 }}
            animate={{ opacity: 1, scale: 1, y: 0 }}
            exit={{ opacity: 0, scale: 0.96, y: 12 }}
            transition={{ duration: 0.2, ease: [0.22, 1, 0.36, 1] }}
            onClick={(event) => event.stopPropagation()}
          >
            <div className="flex shrink-0 items-center justify-between border-b border-[var(--border-default)] px-6 py-4">
              <div className="min-w-0">
                <h3 className="truncate text-lg font-semibold text-[var(--fg-primary)]">
                  {t('viewAllTitle', { name: categoryName })}
                </h3>
                <p className="mt-0.5 text-xs text-[var(--fg-muted)]">
                  {t('viewAllSummary', {
                    count: category.files.length.toLocaleString(),
                    size: formatSize(category.total_size),
                  })}
                </p>
              </div>
              <button onClick={onClose} className="rounded-lg p-2 transition hover:bg-[var(--bg-hover)]">
                <X className="h-5 w-5 text-[var(--fg-muted)]" />
              </button>
            </div>

            <div className="min-h-0 flex-1 px-4 py-3">
              <FileTreeList<FileInfo>
                files={category.files}
                selectedPaths={selectedPaths}
                onSetSelection={onSetSelectionForPaths}
                onToggleFile={(file) => onToggleFile(file.path)}
                isSelectable={isSelectable}
                disabled={isSystemManaged}
                getFileTitle={(file) => file.name}
                // The modal is the only place this list is "for viewing", so sorting goes here;
                // putting sorting controls in the 256px narrow list in the card would crowd out the path
                sortable
                foldersOnlyVariant="label"
                height={Math.round(Math.min(560, Math.max(280, window.innerHeight * 0.5)))}
                rowHeight={44}
                renderFileActions={(file) => (
                  <div className="flex shrink-0 items-center gap-1">
                    <button
                      onClick={(event) => { event.stopPropagation(); onSearchFile(file); }}
                      className="rounded-lg p-1.5 text-[var(--text-muted)] transition hover:bg-[var(--bg-active)] hover:text-[var(--brand-green)]"
                      title={commonT('searchDeletionAdvice')}
                    >
                      <Search className="h-4 w-4" />
                    </button>
                    <button
                      onClick={(event) => { event.stopPropagation(); handleOpenLocation(file); }}
                      className="rounded-lg p-1.5 text-[var(--text-muted)] transition hover:bg-[var(--bg-active)] hover:text-[var(--brand-green)]"
                      title={file.category === 'RecycleBin' ? commonT('openRecycleBin') : commonT('openInFolder')}
                    >
                      <FolderOpen className="h-4 w-4" />
                    </button>
                    <button
                      onClick={(event) => {
                        event.stopPropagation();
                        if (file.category === 'RecycleBin') openRecycleBin();
                        else openFile(file.path);
                      }}
                      className="rounded-lg p-1.5 text-[var(--text-muted)] transition hover:bg-[var(--bg-active)] hover:text-[var(--brand-green)]"
                      title={file.category === 'RecycleBin' ? commonT('openRecycleBin') : commonT('openFile')}
                    >
                      <ExternalLink className="h-4 w-4" />
                    </button>
                  </div>
                )}
              />
            </div>

            <div className="flex shrink-0 flex-wrap items-center gap-3 border-t border-[var(--border-default)] px-6 py-3">
              <span className="text-xs text-[var(--fg-muted)]">
                {selectedCount > 0
                  ? t('viewAllSelected', { count: selectedCount.toLocaleString(), size: formatSize(selectedSize) })
                  : t('viewAllNone')}
              </span>
              {isSystemManaged && (
                <span className="text-xs text-[var(--color-warning)]">{commonT('systemManagedHint')}</span>
              )}
              <div className="ml-auto flex items-center gap-2">
                <button
                  onClick={onClose}
                  className="rounded-lg px-3 py-1.5 text-xs text-[var(--fg-muted)] transition hover:bg-[var(--bg-hover)]"
                >
                  {commonT('cancel')}
                </button>
                <button
                  onClick={onRequestDelete}
                  disabled={isSystemManaged || selectedCount === 0}
                  className="flex items-center gap-1.5 rounded-lg bg-[var(--color-danger)] px-3 py-1.5 text-xs font-medium text-white transition hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-40"
                >
                  <Trash2 className="h-3.5 w-3.5" />
                  {commonT('deleteSelected')}
                </button>
              </div>
            </div>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>,
    document.body,
  );
}
