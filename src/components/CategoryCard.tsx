// ============================================================================
// Category card component - supports theme switching
// Uses a virtual list to optimize rendering performance for large numbers of files
// ============================================================================

import { useState, useMemo, useCallback } from 'react';
import { AnimatePresence, motion } from 'framer-motion';
import {
  ChevronDown,
  Folder,
  AlertTriangle,
  FolderOpen,
  ExternalLink,
  Search,
} from 'lucide-react';
import { openInFolder, openFile, openRecycleBin, openDiskCleanup } from '../api/commands';
import type { CategoryScanResult, FileInfo } from '../types';
import { formatSize } from '../utils/format';
import { FileTreeList } from './FileTreeList';
import { useTranslation } from 'react-i18next';

// WeChat-style risk level style configuration
const getRiskBadgeStyle = (level: number) => {
  switch (level) {
    case 1:
      return 'bg-[var(--brand-green-10)] text-[var(--brand-green)] border-[var(--brand-green-20)]';  // WeChat green - safe
    case 2:
      return 'bg-[var(--brand-green-10)] text-[var(--brand-green)] border-[var(--brand-green-20)]';  // WeChat green - low risk
    case 3:
      return 'bg-[var(--color-warning)]/10 text-[var(--color-warning)] border-[var(--color-warning)]/20';  // soft orange - medium
    case 4:
      return 'bg-[var(--color-warning)]/10 text-[var(--color-warning)] border-[var(--color-warning)]/20';  // soft orange - fairly high
    default:
      return 'bg-[var(--color-danger)]/10 text-[var(--color-danger)] border-[var(--color-danger)]/20';  // soft red - high risk
  }
};

const getRiskText = (level: number) => {
  switch (level) {
    case 1: return 'safe';
    case 2: return 'lowRisk';
    case 3: return 'mediumRisk';
    case 4: return 'highRisk';
    default: return 'criticalRisk';
  }
};

interface CategoryCardProps {
  category: CategoryScanResult;
  selectedPaths: Set<string>;
  onToggleFile: (path: string) => void;
  onToggleCategory: (categoryName: string, files: FileInfo[], selected: boolean) => void;
  /** Directory-level batch checking: submit a batch of paths at once (this is how checking a folder works, no per-row toggle) */
  onSetSelectionForPaths: (paths: string[], selected: boolean) => void;
  onSearchFile: (file: FileInfo) => void;
  /** Deep-scan categories only fetch the first page: provide a "View All Files" entry that opens the full directory tree in a modal */
  hasMore?: boolean;
  onViewAllFiles?: () => void;
  isLoadingAll?: boolean;
}

/**
 * Category card component - desktop app style
 */
export function CategoryCard({
  category,
  selectedPaths,
  onToggleFile,
  onToggleCategory,
  onSetSelectionForPaths,
  onSearchFile,
  hasMore = false,
  onViewAllFiles,
  isLoadingAll = false,
}: CategoryCardProps) {
  const { t: commonT } = useTranslation('common');
  const { t } = useTranslation('junkClean');
  const categoryName = t(`categories.${category.category}.name`, { defaultValue: category.display_name });
  const categoryDescription = t(`categories.${category.category}.description`, { defaultValue: category.description });
  const [expanded, setExpanded] = useState(false);

  // Use useMemo to cache computed results, avoiding repeated computation
  const { selectedCount, selectedSize, isAllSelected, isPartialSelected } = useMemo(() => {
    let count = 0;
    let size = 0;
    for (const f of category.files) {
      if (selectedPaths.has(f.path)) {
        count++;
        size += f.size;
      }
    }
    return {
      selectedCount: count,
      selectedSize: size,
      isAllSelected: count === category.files.length && category.files.length > 0,
      isPartialSelected: count > 0 && count < category.files.length,
    };
  }, [category.files, selectedPaths]);

  // Categories managed by the system (old Windows installation): cannot be deleted inside the app, and should not be deleted by it —
  // deleting them would take away the user's ability to roll back to the old version. Here we uniformly block checking, keeping only
  // the two paths of "view occupied size + guide to system disk cleanup".
  const isSystemManaged = category.system_managed === true;

  // Stable reference: FileTreeList uses it internally to decide "which files are checkable",
  // a fresh function on every render would make the whole tree's checkable set recompute.
  const isSelectable = useCallback(() => !isSystemManaged, [isSystemManaged]);

  const handleCategoryToggle = useCallback(() => {
    if (isSystemManaged) return;
    onToggleCategory(category.display_name, category.files, !isAllSelected);
  }, [category.display_name, category.files, isAllSelected, onToggleCategory, isSystemManaged]);

  const handleExpand = useCallback(() => {
    setExpanded(prev => !prev);
  }, []);

  /**
   * Directory tree checking.
   *
   * Explanation: when what is checked is "all files of the whole category", the original category-selection logic must be used — in deep scan mode
   * it also remembers "whole category selected" so deletion includes unpaged files; checking a subdirectory only touches that batch of paths.
   */
  const handleSetSelection = useCallback((paths: string[], selected: boolean) => {
    if (paths.length === category.files.length) {
      onToggleCategory(category.display_name, category.files, selected);
      return;
    }
    onSetSelectionForPaths(paths, selected);
  }, [category.display_name, category.files, onSetSelectionForPaths, onToggleCategory]);

  if (category.files.length === 0) return null;

  return (
    <div className="bg-[var(--bg-card)] rounded-2xl overflow-hidden shadow-sm">
      {/* Category header - increased padding */}
      <div
        className="px-5 py-4 cursor-pointer hover:bg-[var(--bg-hover)] transition-colors select-none"
        onClick={handleExpand}
      >
        {/* Category names have different lengths across languages; allow wrapping in narrow windows to avoid risk tags and statistics squeezing each other. */}
        <div className="flex flex-wrap items-center gap-3 sm:flex-nowrap sm:gap-4">
          {/* Expand icon */}
          <div className="text-[var(--text-faint)] transition-transform duration-200" style={{ transform: expanded ? 'rotate(0deg)' : 'rotate(-90deg)' }}>
            <ChevronDown className="w-5 h-5" />
          </div>

          {/* Checkbox - WeChat green; system-managed categories offer no checkbox and are shown as disabled */}
          <div onClick={(e) => { e.stopPropagation(); handleCategoryToggle(); }}>
            <div className={`w-5 h-5 rounded border-2 flex items-center justify-center transition-colors
              ${isSystemManaged
                ? 'border-[var(--text-faint)]/40 opacity-40 cursor-not-allowed'
                : `${isAllSelected ? 'bg-[var(--brand-green)] border-[var(--brand-green)]' : isPartialSelected ? 'bg-[var(--brand-green)]/50 border-[var(--brand-green)]' : 'border-[var(--text-faint)] hover:border-[var(--text-muted)]'} cursor-pointer`}`}>
              {(isAllSelected || isPartialSelected) && (
                <svg className="w-3 h-3 text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={3}>
                  {isAllSelected ? <path strokeLinecap="round" strokeLinejoin="round" d="M5 13l4 4L19 7" /> : <path strokeLinecap="round" d="M5 12h14" />}
                </svg>
              )}
            </div>
          </div>

          {/* Category icon - WeChat green at 10% opacity in a rounded container */}
          <div className="w-10 h-10 rounded-xl bg-[var(--brand-green-10)] flex items-center justify-center text-[var(--brand-green)]">
            <Folder className="w-5 h-5" />
          </div>

          {/* Category info */}
          <div className="min-w-[10.0rem] flex-1 sm:min-w-0">
            <div className="flex min-w-0 items-center gap-2">
              <span className="text-[1rem] font-bold text-[var(--text-primary)] truncate">
                {categoryName}
              </span>
              <span className={`shrink-0 whitespace-nowrap px-2 py-0.5 text-[0.79rem] font-medium rounded border ${getRiskBadgeStyle(category.risk_level)}`}>
                {commonT(getRiskText(category.risk_level))}
              </span>
            </div>
            <p className="text-[0.93rem] text-[var(--text-muted)] truncate mt-1">{categoryDescription}</p>
          </div>

          {/* Statistics - tabular-nums keeps numbers stable */}
          <div className="ml-auto shrink-0 text-right">
            <p className="text-[1.07rem] font-bold text-[var(--text-primary)] tabular-nums">
              {formatSize(category.total_size)}
            </p>
            <p className="text-[0.93rem] text-[var(--text-muted)] tabular-nums">
              {commonT('fileCount', { count: category.file_count.toLocaleString() })}
              {selectedCount > 0 && (
                <span className="text-[var(--brand-green)] ml-1">
                  {/* During deep category pagination, hint at the full deletion scope, to avoid mistaking the current page size for the category total. */}
                  {hasMore && selectedCount === category.files.length
                    ? commonT('selectedPage', { count: selectedCount.toLocaleString() })
                    : commonT('selectedFiles', { count: selectedCount.toLocaleString(), size: formatSize(selectedSize) })}
                </span>
              )}
            </p>
          </div>
        </div>
      </div>

      {/* File list - virtual scrolling. Wrapping the list in a height animation means expanding/collapsing no longer cuts abruptly. */}
      <AnimatePresence initial={false}>
        {expanded && (
          <motion.div
            className="overflow-hidden border-t border-[var(--border-color)]"
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: 'auto', opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.26, ease: [0.22, 1, 0.36, 1] }}
          >
            {/* System-managed categories: no delete entry, instead guide to system disk cleanup.
                Such directories cannot be deleted inside the app (permissions/locks), and deleting them would remove the user's ability to roll back,
                so the proper approach is to hand them to Windows' built-in disk cleanup. */}
            {isSystemManaged ? (
              <div className="px-5 py-2.5 bg-[var(--color-warning)]/10 border-b border-[var(--color-warning)]/20 flex flex-wrap items-center gap-2 text-[0.93rem] text-[var(--color-warning)]">
                <AlertTriangle className="w-4 h-4 shrink-0" />
                <span className="flex-1 min-w-[12em]">{commonT('systemManagedHint')}</span>
                <button
                  onClick={() => openDiskCleanup()}
                  className="shrink-0 px-2.5 py-1 rounded-lg border border-[var(--color-warning)]/40 hover:bg-[var(--color-warning)]/15 transition text-[0.86rem] font-medium"
                >
                  {commonT('openDiskCleanup')}
                </button>
              </div>
            ) : category.risk_level >= 3 && (
              <div className="px-5 py-2.5 bg-[var(--color-warning)]/10 border-b border-[var(--color-warning)]/20 flex items-center gap-2 text-[0.93rem] text-[var(--color-warning)]">
                <AlertTriangle className="w-4 h-4" />
                <span>{commonT('highRiskWarning')}</span>
              </div>
            )}

            {/* Directory tree: collapsed by path into a file-manager-like hierarchy,
                checking a directory = bulk-selecting all files under it (checking one by one is too tedious).
                Height stays at the original 256px, with virtual scrolling built in. */}
            <FileTreeList
              files={category.files}
              selectedPaths={selectedPaths}
              onSetSelection={handleSetSelection}
              onToggleFile={(file) => onToggleFile(file.path)}
              isSelectable={isSelectable}
              disabled={isSystemManaged}
              getFileTitle={(file) => file.name}
              height={256}
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
                    onClick={(event) => {
                      event.stopPropagation();
                      if (file.category === 'RecycleBin') openRecycleBin();
                      else openInFolder(file.path);
                    }}
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
            {/* No longer make users click "Load More" page by page: unified into one "View All Files" entry,
                with the full directory tree shown in a modal — this 256px window inside the card simply cannot hold
                thousands or tens of thousands of files, and the first 500 rows of a deep scan are just as unscrollable. */}
            {onViewAllFiles && (hasMore || category.files.length > 50) && (
              <div className="border-t border-[var(--border-color)] px-5 py-2.5">
                <button
                  onClick={onViewAllFiles}
                  disabled={isLoadingAll}
                  className="flex w-full items-center justify-center gap-1.5 rounded-lg py-1.5 text-[0.86rem] font-medium text-[var(--brand-green)] transition hover:bg-[var(--brand-green-10)] disabled:text-[var(--text-faint)]"
                >
                  {isLoadingAll
                    ? commonT('loadingAllFiles')
                    : commonT('viewAllFiles', { count: category.file_count.toLocaleString() })}
                  {!isLoadingAll && <span aria-hidden>→</span>}
                </button>
              </div>
            )}
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}
