// ============================================================================
// Disk growth drill-down modal (single unified tree)
//
// Previously this modal had two panels: changed subdirectories on the left and
// changed files of the current directory on the right. Once the snapshot data
// ran out (depth 4) the left panel went empty and the right one dumped thousands
// of loose files, so `roaming/tg/...`, `roaming/GitHub Desktop/...` and
// `roaming/Tencent/...` all sat in one list.
//
// Now there is one tree: folders and files live in the same hierarchy, expansion
// replaces the old "enter directory" navigation, and drilling down always ends at
// real files. Nodes below the aggregation depth are synthesized from file paths.
// ============================================================================

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { useVirtualizer } from '@tanstack/react-virtual';
import { motion } from 'framer-motion';
import {
  ChevronRight,
  ChevronsDownUp,
  File as FileIcon,
  Folder,
  FolderOpen,
  Loader2,
  Search,
  X,
} from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { AiAskButton } from '../ai/AiAskButton';
import { buildItemQuestion } from '../ai/aiQuestions';
import {
  getDiskGrowthDirectoryDetails,
  getDiskGrowthFileDetails,
  type DiskGrowthEntry,
  type DiskGrowthDetailEntry,
  type DiskGrowthFileDetailEntry,
} from '../../api/commands';
import { formatSize } from '../../utils/format';
import { buildPathTitle, describeGrowth, formatDiff, getGrowthStyle, normalizeDiskPath } from '../../utils/growthFormat';
import { buildDetailTree, type GrowthDetailNode } from '../../utils/growthDetailTree';

const PAGE_SIZE = 200;

type Row =
  | { kind: 'node'; node: GrowthDetailNode }
  | { kind: 'loading'; node: GrowthDetailNode }
  | { kind: 'empty'; node: GrowthDetailNode }
  | { kind: 'failed'; node: GrowthDetailNode }
  | { kind: 'more'; node: GrowthDetailNode };

interface PageState {
  loaded: number;
  total: number;
  hasMore: boolean;
}

export interface GrowthDetailModalProps {
  entry: DiskGrowthEntry;
  driveLetter: string;
  onClose: () => void;
  onOpenFolder: (path: string) => void;
  onSearchPath: (path: string) => void;
}

export function GrowthDetailModal({
  entry,
  driveLetter,
  onClose,
  onOpenFolder,
  onSearchPath,
}: GrowthDetailModalProps) {
  const { t: moduleT } = useTranslation('modules');
  const rootPath = entry.path;

  const [dirsByParent, setDirsByParent] = useState<Map<string, DiskGrowthDetailEntry[]>>(() => new Map());
  const [filesByParent, setFilesByParent] = useState<Map<string, DiskGrowthFileDetailEntry[]>>(() => new Map());
  const [pages, setPages] = useState<Map<string, PageState>>(() => new Map());
  const [loadedKeys, setLoadedKeys] = useState<Set<string>>(() => new Set());
  const [loadingKeys, setLoadingKeys] = useState<Set<string>>(() => new Set());
  const [failedKeys, setFailedKeys] = useState<Set<string>>(() => new Set());
  const [expandedKeys, setExpandedKeys] = useState<Set<string>>(() => new Set());

  const loadingRef = useRef<Set<string>>(new Set());
  /** Root level auto-expands once per opened directory; manual collapse must not be overridden */
  const autoExpandDoneRef = useRef(false);
  const scrollRef = useRef<HTMLDivElement | null>(null);

  const loadChildren = useCallback(async (path: string, append = false) => {
    const key = normalizeDiskPath(path);
    if (loadingRef.current.has(key)) return;
    loadingRef.current.add(key);
    setLoadingKeys((current) => new Set(current).add(key));

    const offset = append ? (pages.get(key)?.loaded ?? 0) : 0;
    try {
      // Directories only exist down to the snapshot's aggregation depth; beyond that
      // the file call returns the changed subtree, and paths rebuild the missing levels.
      const [dirsResult, filesResult] = await Promise.allSettled([
        offset === 0
          ? getDiskGrowthDirectoryDetails(path, 0, PAGE_SIZE, driveLetter)
          : Promise.resolve(null),
        getDiskGrowthFileDetails(path, offset, PAGE_SIZE, driveLetter),
      ]);

      if (offset === 0 && dirsResult.status === 'fulfilled' && dirsResult.value) {
        const entries = dirsResult.value.entries;
        setDirsByParent((current) => new Map(current).set(key, entries));
      }

      if (filesResult.status === 'fulfilled') {
        const result = filesResult.value;
        setFilesByParent((current) => {
          const next = new Map(current);
          const existing = offset === 0 ? [] : (current.get(key) ?? []);
          next.set(key, [...existing, ...result.entries]);
          return next;
        });
        setPages((current) => new Map(current).set(key, {
          loaded: offset + result.returned_files,
          total: result.total_changed_files,
          hasMore: result.has_more,
        }));
      }

      const bothFailed = filesResult.status === 'rejected'
        && (dirsResult.status === 'rejected' || dirsResult.value === null);
      if (bothFailed) {
        setFailedKeys((current) => new Set(current).add(key));
      } else {
        setLoadedKeys((current) => new Set(current).add(key));
      }

      // Opening a folder with only a couple of changed subfolders: show one level already,
      // otherwise the first screen is a nearly empty list and the finer structure stays hidden.
      if (!append && !autoExpandDoneRef.current && key === normalizeDiskPath(rootPath)
        && dirsResult.status === 'fulfilled' && dirsResult.value && dirsResult.value.entries.length > 0
        && dirsResult.value.entries.length <= 3) {
        autoExpandDoneRef.current = true;
        const keys = dirsResult.value.entries.map((dir) => normalizeDiskPath(dir.path));
        setExpandedKeys((current) => new Set([...current, ...keys]));
        for (const dir of dirsResult.value.entries) {
          void loadChildren(dir.path);
        }
      }
    } finally {
      loadingRef.current.delete(key);
      setLoadingKeys((current) => {
        const next = new Set(current);
        next.delete(key);
        return next;
      });
    }
  }, [driveLetter, pages, rootPath]);

  // Opening another directory starts from a clean tree
  useEffect(() => {
    setDirsByParent(new Map());
    setFilesByParent(new Map());
    setPages(new Map());
    setLoadedKeys(new Set());
    setLoadingKeys(new Set());
    setFailedKeys(new Set());
    setExpandedKeys(new Set());
    loadingRef.current = new Set();
    autoExpandDoneRef.current = false;
    void loadChildren(rootPath);
    // loadChildren depends on `pages`, which is reset above; the path is what identifies the request
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rootPath, driveLetter]);

  const tree = useMemo(
    () => buildDetailTree(rootPath, dirsByParent, filesByParent),
    [rootPath, dirsByParent, filesByParent],
  );

  const rows = useMemo(() => {
    const out: Row[] = [];
    const walk = (nodes: GrowthDetailNode[]) => {
      for (const node of nodes) {
        out.push({ kind: 'node', node });
        if (!node.isDir || !expandedKeys.has(node.key)) continue;
        if (loadingKeys.has(node.key)) {
          out.push({ kind: 'loading', node });
          continue;
        }
        if (failedKeys.has(node.key)) {
          out.push({ kind: 'failed', node });
          continue;
        }
        if (node.children.length === 0) {
          out.push({ kind: 'empty', node });
          continue;
        }
        walk(node.children);
        if (pages.get(node.key)?.hasMore) out.push({ kind: 'more', node });
      }
    };
    walk(tree);
    return out;
  }, [tree, expandedKeys, loadingKeys, failedKeys, pages]);

  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 40,
    overscan: 14,
  });

  const handleToggle = useCallback((node: GrowthDetailNode) => {
    if (!node.isDir) return;
    const willExpand = !expandedKeys.has(node.key);
    setExpandedKeys((current) => {
      const next = new Set(current);
      if (willExpand) next.add(node.key);
      else next.delete(node.key);
      return next;
    });
    if (willExpand && !loadedKeys.has(node.key) && !failedKeys.has(node.key)) {
      void loadChildren(node.path);
    }
  }, [expandedKeys, failedKeys, loadChildren, loadedKeys]);

  const rootStyle = getGrowthStyle(entry.level);
  const RootIcon = rootStyle.icon;
  const rootKey = normalizeDiskPath(rootPath);
  // Without this the first screen spins forever when the folder itself has no changed children
  const rootSettled = loadedKeys.has(rootKey) || failedKeys.has(rootKey);

  const renderNode = (node: GrowthDetailNode) => {
    const expanded = expandedKeys.has(node.key);
    const loading = loadingKeys.has(node.key);
    const isSynthetic = node.isDir && !node.hasOwnStats;
    const explanation = node.isDir
      ? (isSynthetic
        ? moduleT('diskGrowth.syntheticFolderHint')
        : describeGrowth(node.level, node.diff))
      : describeGrowth(node.level, node.diff);

    return (
      <div
        className="flex h-10 items-center gap-2 px-3 transition-colors hover:bg-[var(--bg-hover)]"
        key={`${node.key}-${node.depth}`}
      >
        <div className="flex min-w-0 flex-1 items-center gap-2" style={{ paddingLeft: node.depth * 14 }}>
          {node.isDir ? (
            <button
              type="button"
              data-growth-expand={expanded ? 'open' : 'closed'}
              onClick={() => handleToggle(node)}
              className="flex h-4 w-4 shrink-0 items-center justify-center rounded text-[var(--text-faint)] transition-colors hover:text-[var(--brand-green)]"
              title={expanded ? moduleT('diskGrowth.collapseLevel') : moduleT('diskGrowth.expandLevel')}
            >
              {loading
                ? <Loader2 className="h-3 w-3 animate-spin" />
                : <ChevronRight className={`h-3.5 w-3.5 transition-transform ${expanded ? 'rotate-90' : ''}`} />}
            </button>
          ) : (
            <span className="h-4 w-4 shrink-0" />
          )}

          {node.isDir
            ? (expanded
              ? <FolderOpen className="h-4 w-4 shrink-0 text-amber-500" />
              : <Folder className="h-4 w-4 shrink-0 text-amber-500" />)
            : <FileIcon className="h-4 w-4 shrink-0 text-[var(--text-faint)]" />}

          <button
            type="button"
            // Folders expand on click — drilling down is the main job here; the folder button on the
            // right still opens Explorer for the node itself.
            onClick={() => (node.isDir ? handleToggle(node) : onOpenFolder(node.path))}
            className="min-w-0 truncate text-left text-[0.93rem] text-[var(--text-primary)] transition-colors hover:text-[var(--brand-green)]"
            title={buildPathTitle(node.path, node.modified, explanation)}
          >
            {node.name}
          </button>

          {isSynthetic && (
            <span
              className="shrink-0 rounded bg-[var(--bg-hover)] px-1 py-px text-[0.7rem] text-[var(--text-faint)]"
              title={moduleT('diskGrowth.syntheticFolderHint')}
            >
              ≈
            </span>
          )}
          {node.isDir && node.fileCount > 0 && (
            <span className="shrink-0 text-[0.79rem] text-[var(--text-faint)] tabular-nums">
              {moduleT('diskGrowth.detailFileCount', { count: node.fileCount.toLocaleString() })}
            </span>
          )}
        </div>

        <span className="w-24 shrink-0 whitespace-nowrap text-right text-[0.86rem] text-[var(--text-muted)] tabular-nums">
          {isSynthetic ? `≈ ${formatSize(node.size)}` : formatSize(node.size)}
        </span>

        <span className={`w-28 shrink-0 whitespace-nowrap text-right text-[0.86rem] font-medium tabular-nums ${
          node.diff > 0 ? 'text-red-500' : node.diff < 0 ? 'text-green-500' : 'text-[var(--text-faint)]'
        }`}>
          {node.diff === 0 ? '-' : `${isSynthetic ? '≈ ' : ''}${formatDiff(node.diff)}`}
        </span>

        <div className="flex w-[5.5rem] shrink-0 items-center justify-end gap-0.5">
          {node.isDir && (
            <button
              type="button"
              onClick={() => onSearchPath(node.path)}
              className="rounded-lg p-1.5 text-[var(--text-muted)] transition hover:bg-[var(--brand-green-10)] hover:text-[var(--brand-green)]"
              title={moduleT('diskGrowth.searchPath')}
            >
              <Search className="h-4 w-4" />
            </button>
          )}
          <AiAskButton
            question={buildItemQuestion(node.path, node.isDir ? 'folder' : 'file')}
            skipLookup
            className="inline-flex h-7 w-7 items-center justify-center"
          />
          <button
            type="button"
            onClick={() => onOpenFolder(node.path)}
            className="rounded-lg p-1.5 text-[var(--text-muted)] transition hover:bg-[var(--brand-green-10)] hover:text-[var(--brand-green)]"
            title={node.isDir ? moduleT('diskGrowth.openFolder') : moduleT('diskGrowth.openLocation')}
          >
            <FolderOpen className="h-4 w-4" />
          </button>
        </div>
      </div>
    );
  };

  const renderMarker = (row: Row & { kind: 'loading' | 'empty' | 'failed' | 'more' }) => {
    // 12 = row padding, then one level of indent + chevron slot, so markers line up under the child names.
    // Fixed h-10 keeps the virtualizer's 40px estimate exact for these rows too.
    const indent = 12 + (row.node.depth + 1) * 14 + 48;
    const base = 'flex h-10 items-center gap-2 text-[0.86rem]';
    if (row.kind === 'loading') {
      return (
        <div className={`${base} text-[var(--text-muted)]`} style={{ paddingLeft: indent }}>
          <Loader2 className="h-3.5 w-3.5 animate-spin text-[var(--brand-green)]" />
          {moduleT('diskGrowth.loadingChildren')}
        </div>
      );
    }
    if (row.kind === 'failed') {
      return (
        <div className={`${base} text-[var(--text-muted)]`} style={{ paddingLeft: indent }}>
          {moduleT('diskGrowth.detailUnavailable')}
        </div>
      );
    }
    if (row.kind === 'empty') {
      return (
        <div className={`${base} text-[var(--text-faint)]`} style={{ paddingLeft: indent }}>
          {moduleT('diskGrowth.noDirectories')}
        </div>
      );
    }
    const page = pages.get(row.node.key);
    return (
      <button
        type="button"
        onClick={() => void loadChildren(row.node.path, true)}
        className={`${base} w-full text-left text-[var(--brand-green)] transition-colors hover:text-[var(--brand-green-hover)]`}
        style={{ paddingLeft: indent }}
      >
        {moduleT('diskGrowth.loadMore')}
        {page && (
          <span className="text-[0.79rem] text-[var(--text-faint)] tabular-nums">
            {moduleT('diskGrowth.showing', { current: page.loaded, total: page.total })}
          </span>
        )}
      </button>
    );
  };

  // The modal must be mounted under body to avoid the outer motion transform of the page mode changing the reference for fixed positioning.
  return createPortal(
    <motion.div
      className="fixed inset-0 z-[10000] flex items-center justify-center bg-black/40 backdrop-blur-sm glass-backdrop"
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      transition={{ duration: 0.18, ease: 'easeOut' }}
      onClick={onClose}
    >
      <motion.div
        className="w-[74.29rem] max-w-[calc(100vw-32px)] h-[90vh] max-h-[calc(100vh-32px)] rounded-2xl bg-[var(--bg-card)] glass-dialog shadow-2xl border border-[var(--border-color)] overflow-hidden flex flex-col"
        initial={{ opacity: 0, scale: 0.96, y: 12 }}
        animate={{ opacity: 1, scale: 1, y: 0 }}
        exit={{ opacity: 0, scale: 0.96, y: 12 }}
        transition={{ duration: 0.2, ease: [0.22, 1, 0.36, 1] }}
        onClick={(event) => event.stopPropagation()}
      >
        <div className="flex items-center gap-4 border-b border-[var(--border-color)] px-5 py-3">
          <div className="min-w-0 flex-1">
            <h3 className="text-sm font-semibold text-[var(--text-primary)]">{moduleT('diskGrowth.details')}</h3>
            <p className="mt-0.5 truncate text-xs text-[var(--text-muted)]" title={rootPath}>{rootPath}</p>
          </div>

          {/* Totals belong next to the title: the tree below needs every pixel of height it can get */}
          <div className="flex shrink-0 items-center gap-5">
            <div className="text-right">
              <p className="text-[0.7rem] leading-4 text-[var(--text-muted)]">{moduleT('diskGrowth.previousSize')}</p>
              <p className="text-[0.86rem] font-semibold leading-5 tabular-nums text-[var(--text-primary)]">
                {formatSize(entry.old_size)}
              </p>
            </div>
            <div className="text-right">
              <p className="text-[0.7rem] leading-4 text-[var(--text-muted)]">{moduleT('diskGrowth.currentSize')}</p>
              <p className="text-[0.86rem] font-semibold leading-5 tabular-nums text-[var(--text-primary)]">
                {formatSize(entry.new_size)}
              </p>
            </div>
            <div className="text-right">
              <p className="text-[0.7rem] leading-4 text-[var(--text-muted)]">{moduleT('diskGrowth.difference')}</p>
              <p className={`flex items-center justify-end gap-1 text-[0.86rem] font-semibold leading-5 tabular-nums ${rootStyle.color}`}>
                <RootIcon className="h-3 w-3 shrink-0" />
                {formatDiff(entry.diff)}
              </p>
            </div>
          </div>

          <button
            onClick={onClose}
            className="rounded-lg p-1.5 text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
          >
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="flex min-h-0 flex-1 flex-col p-4">
          <div className="glass-list flex min-h-0 flex-1 flex-col overflow-hidden rounded-xl border border-[var(--border-color)] bg-[var(--bg-main)]">
            <div ref={scrollRef} className="min-h-0 flex-1 overflow-auto">
              {/* Inside the scroller on purpose: the scrollbar steals ~10px from the rows, so a header
                  sitting outside it would drift right of every column. Sticky keeps the labels visible. */}
              <div className="sticky top-0 z-10 flex items-center gap-2 border-b border-[var(--border-color)] bg-[var(--bg-main)] px-3 py-1.5 text-[0.79rem] text-[var(--text-faint)]">
                <div className="min-w-0 flex-1">{moduleT('diskGrowth.nameColumn')}</div>
                <div className="w-24 shrink-0 text-right">{moduleT('diskGrowth.currentSizeHeader')}</div>
                <div className="w-28 shrink-0 text-right">{moduleT('diskGrowth.differenceHeader')}</div>
                {/* Sits in the action column so the header aligns with the row buttons and costs no extra row */}
                <div className="flex w-[5.5rem] shrink-0 items-center justify-end">
                  <button
                    type="button"
                    onClick={() => setExpandedKeys(new Set())}
                    disabled={expandedKeys.size === 0}
                    className="rounded p-1 text-[var(--text-muted)] transition hover:bg-[var(--bg-hover)] hover:text-[var(--brand-green)] disabled:opacity-30"
                    title={moduleT('diskGrowth.collapseAll')}
                  >
                    <ChevronsDownUp className="h-3.5 w-3.5" />
                  </button>
                </div>
              </div>

              {rows.length === 0 ? (
                rootSettled ? (
                  <div className="flex items-center justify-center px-4 py-16 text-sm text-[var(--text-faint)]">
                    {moduleT('diskGrowth.noDirectories')}
                  </div>
                ) : (
                  <div className="flex items-center justify-center gap-2 px-4 py-16 text-sm text-[var(--text-muted)]">
                    <Loader2 className="h-4 w-4 animate-spin text-[var(--brand-green)]" />
                    {moduleT('diskGrowth.loadingDirectories')}
                  </div>
                )
              ) : (
                <div style={{ height: `${virtualizer.getTotalSize()}px`, position: 'relative' }}>
                  {virtualizer.getVirtualItems().map((virtualRow) => {
                    const row = rows[virtualRow.index];
                    if (!row) return null;
                    return (
                      <div
                        key={`${row.kind}-${row.node.key}`}
                        style={{
                          position: 'absolute',
                          top: 0,
                          left: 0,
                          width: '100%',
                          transform: `translateY(${virtualRow.start}px)`,
                        }}
                      >
                        {row.kind === 'node' ? renderNode(row.node) : renderMarker(row)}
                      </div>
                    );
                  })}
                </div>
              )}
            </div>
          </div>

          <p className="mt-2 shrink-0 text-[0.75rem] leading-4 text-[var(--text-faint)]">
            {moduleT('diskGrowth.detailNote', { count: PAGE_SIZE })}
          </p>
        </div>
      </motion.div>
    </motion.div>,
    document.body,
  );
}

export default GrowthDetailModal;
