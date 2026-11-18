// ============================================================================
// File directory tree list
// Scan results are displayed collapsed by directory; checking a directory selects the entire subtree in bulk
//
// Explanation: originally it was a flat "one file per row" list, so clearing a cache directory meant checking items one by one.
// Here it is made into an expandable/collapsible tree like a file manager, with directory rows carrying a tri-state checkbox —
// one check selects all files under that directory.
// ============================================================================

import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
import { ChevronRight, Folder, FolderOpen, File as FileIcon } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import {
  buildFileTree,
  flattenFileTree,
  collectFolderKeys,
  computeSelectedCounts,
  sortTree,
  type FileTreeNode,
  type TreeSort,
  type TreeSortKey,
  type TreeSourceFile,
} from '../utils/fileTree';
import { FileSortControl, FILE_SORT_KEYS } from './FileSortControl';
import { formatDate, formatSize } from '../utils/format';

export interface FileTreeListProps<T extends TreeSourceFile> {
  /** The current group of files (one category, or a filtered result) */
  files: T[];
  /** Set of checked full paths (module-level, may include files from other categories) */
  selectedPaths: Set<string>;
  /** Set the checked state for the whole batch: directory checks are committed at once, avoiding per-row setState */
  onSetSelection: (paths: string[], selected: boolean) => void;
  /** Check a single file: preserve the module's own side effects (e.g. reverse-syncing the whole-category selection marker) */
  onToggleFile: (file: T) => void;
  /** Whether this file can be checked; when not checkable it is shown as disabled (such as social cleanup chat records) */
  isSelectable?: (file: T) => boolean;
  /** Extra inline info: risk tags, source app, etc., rendered between the file name and the size */
  renderFileMeta?: (file: T) => ReactNode;
  /** Row-end action buttons: open the containing folder, search suggestions, etc. */
  renderFileActions?: (file: T) => ReactNode;
  /** Display name of the file row; by default the last segment of the tree */
  getFileTitle?: (file: T) => string;
  /** Scroll area height (px). The whole list is always virtualized, so no amount of rows causes lag */
  height?: number;
  rowHeight?: number;
  /** Extra note on the right of the tree header, e.g. "loaded 500 / total 30,000" */
  toolbarExtra?: ReactNode;
  /** Entire group not checkable (e.g. a system-managed category): only browsing and expanding remain, no check entry at all */
  disabled?: boolean;
  /** Show the sort control (name / size / modified time). The narrow list inside a card hides it by default */
  sortable?: boolean;
  /** Available keys for the sort control; if the data source has no modified time, pass ['name','size'] to avoid a button that does nothing */
  sortKeys?: TreeSortKey[];
  className?: string;
}

const INDENT_STEP = 14;

export function FileTreeList<T extends TreeSourceFile>({
  files,
  selectedPaths,
  onSetSelection,
  onToggleFile,
  isSelectable,
  renderFileMeta,
  renderFileActions,
  getFileTitle,
  height = 256,
  rowHeight = 44,
  toolbarExtra,
  disabled = false,
  sortable = false,
  sortKeys = FILE_SORT_KEYS,
  className,
}: FileTreeListProps<T>) {
  const { t } = useTranslation('common');
  const scrollRef = useRef<HTMLDivElement>(null);

  /*
    Expansion strategy: expand small lists fully, collapse large lists by default.

    Explanation: when everything is collapsed, a category with only three or five files takes an extra click to see its contents;
    when everything is expanded, hundreds or thousands of files turn the list into a wall and the directory structure becomes blurred.
    So we branch by file count: ≤ 30 are laid out expanded, more show only the directory level first.
    States the user manually expanded/collapsed are not overwritten (only reset when crossing the threshold or when the whole batch of data changes).
  */
  const [expandedKeys, setExpandedKeys] = useState<Set<string>>(new Set());
  const [sort, setSort] = useState<TreeSort>({ key: 'name', desc: false });
  const baseTree = useMemo(() => buildFileTree(files), [files]);
  // Sorting only reorders child nodes at each level (does not rebuild the tree), so switching sort is much cheaper than swapping in a new batch of data
  const tree = useMemo(() => sortTree(baseTree, sort), [baseTree, sort]);
  const autoExpandMode = tree.fileCount <= 30 ? 'expand' : 'collapse';
  const autoExpandRef = useRef<string | null>(null);
  useEffect(() => {
    if (autoExpandRef.current === autoExpandMode) return;
    autoExpandRef.current = autoExpandMode;
    setExpandedKeys(autoExpandMode === 'expand' ? new Set(collectFolderKeys(tree.nodes)) : new Set());
  }, [autoExpandMode, tree]);

  const rows = useMemo(() => {
    // When there is no real hierarchy (all files on the same level), do not show a tree, avoiding a meaningless extra wrapper layer
    if (!tree.hasBranches) return tree.nodes;
    return flattenFileTree(tree.nodes, expandedKeys);
  }, [tree, expandedKeys]);

  const selectedCounts = useMemo(
    () => computeSelectedCounts(tree.nodes, selectedPaths),
    [tree, selectedPaths],
  );

  // Only checkable files participate in "Select All / check whole directory". Social cleanup chat-record databases
  // are not deletable, and putting them in the checked set would make the deletion flow try to touch them.
  //
  // Explanation: keep only one "checkable path set" plus a checkable count per directory here,
  // no longer storing an array of descendant paths for each directory — that would copy hundreds of thousands of references for tens of thousands of files,
  // and would be fully recomputed whenever the parent's predicate function changes its reference.
  const selectableInfo = useMemo(() => {
    if (disabled) {
      return { set: new Set<string>(), counts: new Map<string, number>(), paths: [] as string[] };
    }
    if (!isSelectable) {
      // No predicate passed = everything is checkable, skipping a whole filter round
      return {
        set: null as Set<string> | null,
        counts: null as Map<string, number> | null,
        paths: files.map((file) => file.path),
      };
    }
    const set = new Set<string>();
    const paths: string[] = [];
    for (const file of files) {
      if (isSelectable(file)) {
        set.add(file.path);
        paths.push(file.path);
      }
    }
    const counts = new Map<string, number>();
    const walk = (nodes: FileTreeNode<T>[]): number => {
      let selected = 0;
      for (const node of nodes) {
        if (node.isDir) {
          const child = walk(node.children);
          counts.set(node.key, child);
          selected += child;
        } else if (set.has(node.path)) {
          selected += 1;
        }
      }
      return selected;
    };
    walk(tree.nodes);
    return { set, counts, paths };
  }, [files, tree, isSelectable, disabled]);

  /** All checkable paths under a directory. Only counted when the checkbox is really clicked, not expanded during render. */
  const selectablePathsOf = useCallback(
    (node: FileTreeNode<T>): string[] => {
      if (node.isDir === false) return [];
      if (!selectableInfo.set) return node.paths;
      return node.paths.filter((path) => selectableInfo.set!.has(path));
    },
    [selectableInfo],
  );

  // The whole group of checkable files: the tree-header checkbox and "Select All" use this set, not all files
  const allPaths = selectableInfo.paths;
  const selectedTotal = useMemo(() => {
    let count = 0;
    for (const path of allPaths) {
      if (selectedPaths.has(path)) count += 1;
    }
    return count;
  }, [allPaths, selectedPaths]);

  const toggleExpand = useCallback((key: string) => {
    setExpandedKeys((previous) => {
      const next = new Set(previous);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  const expandAll = useCallback(() => {
    setExpandedKeys(new Set(collectFolderKeys(tree.nodes)));
  }, [tree]);

  const collapseAll = useCallback(() => {
    setExpandedKeys(new Set());
  }, []);

  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => rowHeight,
    overscan: 12,
  });

  if (files.length === 0) return null;

  const allSelected = selectedTotal === allPaths.length && allPaths.length > 0;
  const partialSelected = selectedTotal > 0 && !allSelected;

  const renderRow = (node: FileTreeNode<T>) => {
    if (node.isDir) {
      const selectedInFolder = selectedCounts.get(node.key) ?? 0;
      // When there are no checkable files under a directory (e.g. the whole category is chat records), gray out the checkbox
      const selectableCount = selectableInfo.counts
        ? (selectableInfo.counts.get(node.key) ?? 0)
        : node.fileCount;
      const blocked = disabled || selectableCount === 0;
      // Compare against the checkable count (not the total file count): when a directory mixes in undeletable files,
      // using the total count never fills up and the checkbox stays stuck in the "partially selected" half state.
      const folderAll = selectableCount > 0 && selectedInFolder >= selectableCount;
      const folderPartial = selectedInFolder > 0 && !folderAll;
      const expanded = expandedKeys.has(node.key);

      return (
        <div
          key={node.key}
          className="flex items-center gap-2 px-3 cursor-pointer select-none transition-colors hover:bg-[var(--bg-hover)]"
          style={{ height: rowHeight, paddingLeft: 12 + node.depth * INDENT_STEP }}
          onClick={() => toggleExpand(node.key)}
          title={node.path}
        >
          <ChevronRight
            className={`h-3.5 w-3.5 shrink-0 text-[var(--text-faint)] transition-transform duration-150 ${expanded ? 'rotate-90' : ''}`}
          />
          <TreeCheckbox
            checked={folderAll}
            partial={folderPartial}
            disabled={blocked || node.fileCount === 0}
            title={folderAll ? t('deselectAll') : t('selectAll')}
            onToggle={() => onSetSelection(selectablePathsOf(node), !folderAll)}
          />
          {expanded
            ? <FolderOpen className="h-4 w-4 shrink-0 text-amber-500" />
            : <Folder className="h-4 w-4 shrink-0 text-amber-500" />}
          <span className="min-w-0 flex-1 truncate text-[0.93rem] font-medium text-[var(--text-primary)]">
            {node.name}
          </span>
          {/* When sorting by time, directories must also show a date, otherwise only files have values in the column and the sort basis is invisible.
              A directory uses its "newest inner file", not the directory's own modified time. */}
          {sortable && sort.key === 'time' && (
            <span
              className="hidden shrink-0 text-[0.79rem] tabular-nums text-[var(--text-faint)] sm:inline"
              title={t('folderLatestTime')}
            >
              {node.latestModified > 0 ? formatDate(node.latestModified) : '—'}
            </span>
          )}
          <span className="shrink-0 text-[0.79rem] tabular-nums text-[var(--text-muted)]">
            {t('fileCount', { count: node.fileCount.toLocaleString() })}
            {node.totalSize > 0 && ` · ${formatSize(node.totalSize)}`}
          </span>
        </div>
      );
    }

    const selectable = !disabled && (isSelectable ? isSelectable(node.file) : true);
    const checked = selectedPaths.has(node.path);
    const title = getFileTitle ? getFileTitle(node.file) : node.name;

    return (
      <div
        key={node.key}
        className={`flex items-center gap-2 px-3 transition-colors
          ${selectable ? 'cursor-pointer' : 'cursor-not-allowed'}
          ${checked && selectable ? 'bg-[var(--brand-green-10)]' : 'hover:bg-[var(--bg-hover)]'}`}
        style={{ height: rowHeight, paddingLeft: 12 + node.depth * INDENT_STEP }}
        onClick={() => { if (selectable) onToggleFile(node.file); }}
        title={node.path}
      >
        {/* Directory rows have a collapse-arrow placeholder, and file rows keep the same width, so checkboxes at the same level align */}
        <span className="h-3.5 w-3.5 shrink-0" />
        <TreeCheckbox
          checked={checked && selectable}
          disabled={!selectable}
          title={selectable ? undefined : t('undeletable')}
          onToggle={() => onToggleFile(node.file)}
        />
        <FileIcon className="h-4 w-4 shrink-0 text-[var(--text-faint)]" />
        <span className="min-w-0 flex-1 truncate text-[0.93rem] text-[var(--text-secondary)]" title={node.path}>
          {title}
        </span>
        {renderFileMeta?.(node.file)}
        {sortable && sort.key === 'time' && (
          <span className="hidden shrink-0 text-[0.79rem] tabular-nums text-[var(--text-faint)] sm:inline">
            {node.modifiedTime > 0 ? formatDate(node.modifiedTime) : '—'}
          </span>
        )}
        <span className={`shrink-0 text-[0.86rem] tabular-nums ${selectable ? 'text-[var(--text-muted)]' : 'text-[var(--text-faint)]'}`}>
          {formatSize(node.file.size)}
        </span>
        {renderFileActions?.(node.file)}
      </div>
    );
  };

  return (
    <div className={className}>
      {/* Tree header: common parent directory + overview + whole-group check + expand/collapse */}
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-[var(--border-default)] bg-[var(--bg-hover)]/40 px-3 py-2">
        <TreeCheckbox
          checked={allSelected}
          partial={partialSelected}
          disabled={disabled || allPaths.length === 0}
          onToggle={() => onSetSelection(allPaths, !allSelected)}
          title={allSelected ? t('deselectAll') : t('selectAll')}
        />
        <span className="min-w-0 flex-1 truncate text-[0.86rem] text-[var(--text-muted)]" title={tree.rootPath}>
          {tree.rootPath || t('allLocations')}
          <span className="ml-2 tabular-nums">
            {t('fileCount', { count: tree.fileCount.toLocaleString() })}
            {tree.totalSize > 0 && ` · ${formatSize(tree.totalSize)}`}
          </span>
        </span>
        {toolbarExtra}
        {sortable && (
          <FileSortControl sort={sort} onSortChange={setSort} keys={sortKeys} />
        )}
        {tree.hasBranches && (
          <span className="flex shrink-0 items-center gap-1">
            <button
              onClick={expandAll}
              className="rounded px-1.5 py-0.5 text-[0.79rem] text-[var(--text-muted)] transition hover:bg-[var(--bg-hover)] hover:text-[var(--brand-green)]"
            >
              {t('expandAll')}
            </button>
            <span className="text-[var(--text-faint)]">/</span>
            <button
              onClick={collapseAll}
              className="rounded px-1.5 py-0.5 text-[0.79rem] text-[var(--text-muted)] transition hover:bg-[var(--bg-hover)] hover:text-[var(--brand-green)]"
            >
              {t('collapseAll')}
            </button>
          </span>
        )}
      </div>

      <div ref={scrollRef} className="overflow-auto" style={{ height }}>
        <div style={{ height: `${virtualizer.getTotalSize()}px`, width: '100%', position: 'relative' }}>
          {virtualizer.getVirtualItems().map((virtualRow) => {
            const node = rows[virtualRow.index];
            return (
              <div
                key={node.key}
                style={{
                  position: 'absolute',
                  top: 0,
                  left: 0,
                  width: '100%',
                  transform: `translateY(${virtualRow.start}px)`,
                }}
              >
                {renderRow(node)}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

/** Tri-state checkbox: all / partially selected (dash) / none / disabled */
function TreeCheckbox({
  checked,
  partial = false,
  disabled = false,
  title,
  onToggle,
}: {
  checked: boolean;
  partial?: boolean;
  disabled?: boolean;
  title?: string;
  onToggle: () => void;
}) {
  const active = checked || partial;
  return (
    <div
      onClick={(event) => {
        event.stopPropagation();
        if (!disabled) onToggle();
      }}
      title={title}
      className={`flex h-4 w-4 shrink-0 items-center justify-center rounded border-2 transition-colors
        ${disabled
          ? 'cursor-not-allowed border-[var(--text-faint)]/40 opacity-40'
          : active
            ? `cursor-pointer border-[var(--brand-green)] ${checked ? 'bg-[var(--brand-green)]' : 'bg-[var(--brand-green)]/40'}`
            : 'cursor-pointer border-[var(--text-faint)] hover:border-[var(--text-muted)]'}`}
    >
      {disabled ? null : checked ? (
        <svg className="h-2.5 w-2.5 text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={3.5}>
          <path strokeLinecap="round" strokeLinejoin="round" d="M5 13l4 4L19 7" />
        </svg>
      ) : partial ? (
        <svg className="h-2.5 w-2.5 text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={3.5}>
          <path strokeLinecap="round" d="M5 12h14" />
        </svg>
      ) : null}
    </div>
  );
}
