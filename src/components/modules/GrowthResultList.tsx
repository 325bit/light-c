// ============================================================================
// Disk growth result list (tree / flat, sortable)
//
// The backend returns a flat list of changed directories at mixed depths sorted
// by |diff|. Rendered directly it reads as 300 unrelated rows: a 2-level folder
// and a 4-level folder look identical. This component folds the list into a
// folder tree, sorts siblings by a chosen column, and lazily loads one more level
// for directories the snapshot aggregation stopped at.
//
// Sorting only reorders siblings inside the tree: a parent always stays above its
// children, otherwise "sort by current size" would collapse back into a flat list.
// ============================================================================

import { Fragment, useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  ArrowDown,
  ArrowUp,
  ChevronRight,
  ChevronsUpDown,
  Folder,
  FolderOpen,
  Loader2,
  Search,
} from 'lucide-react';
import { AiAskButton } from '../ai/AiAskButton';
import { buildItemQuestion } from '../ai/aiQuestions';
import { getDiskGrowthDirectoryDetails, type DiskGrowthEntry } from '../../api/commands';
import { formatSize } from '../../utils/format';
import {
  buildPathTitle,
  describeGrowth,
  formatDiff,
  formatModifiedTime,
  getGrowthStyle,
  growthEntryFromNode,
  normalizeDiskPath,
  simplifyPath,
} from '../../utils/growthFormat';
import {
  buildGrowthTree,
  filterGrowthEntries,
  flattenGrowthNodes,
  growthNodesFromDetails,
  nextGrowthSort,
  sortGrowthEntries,
  sortGrowthNodes,
  DEFAULT_GROWTH_SORT,
  type GrowthFilterMode,
  type GrowthNode,
  type GrowthSort,
  type GrowthSortKey,
  type GrowthViewMode,
} from '../../utils/growthTree';

const VIEW_MODE_STORAGE_KEY = 'lightc.diskGrowth.viewMode';
const SORT_STORAGE_KEY = 'lightc.diskGrowth.sort';
/** One level of detail per expansion; the backend caps a page at 1000 */
const LAZY_PAGE_SIZE = 200;
const FLAT_PAGE_SIZE = 20;

/** Indentation per tree level, and the width taken by "chevron + folder icon" before the name */
const INDENT_STEP = 14;
const NAME_OFFSET = 38;

function readViewMode(): GrowthViewMode {
  try {
    return localStorage.getItem(VIEW_MODE_STORAGE_KEY) === 'flat' ? 'flat' : 'tree';
  } catch {
    return 'tree';
  }
}

function readSort(): GrowthSort {
  try {
    const raw = localStorage.getItem(SORT_STORAGE_KEY);
    if (!raw) return DEFAULT_GROWTH_SORT;
    const parsed = JSON.parse(raw) as Partial<GrowthSort> | null;
    if (!parsed || typeof parsed !== 'object') return DEFAULT_GROWTH_SORT;
    const key = parsed.key;
    if (key !== 'name' && key !== 'size' && key !== 'diff' && key !== 'time') return DEFAULT_GROWTH_SORT;
    return {
      key,
      desc: typeof parsed.desc === 'boolean' ? parsed.desc : true,
      diffMode: parsed.diffMode === 'signed' ? 'signed' : 'abs',
    };
  } catch {
    return DEFAULT_GROWTH_SORT;
  }
}

export interface GrowthResultListProps {
  /** Changed directories; for a first scan this is the usage baseline with zero diffs */
  entries: DiskGrowthEntry[];
  driveLetter: string;
  onOpenFolder: (path: string) => void;
  onSearchPath: (path: string) => void;
  onShowDetails: (entry: DiskGrowthEntry) => void;
  /** Baseline results carry no change amounts, so the increase/decrease filter and diff mode are pointless there */
  hasDiff: boolean;
}

function SortHeader({
  label,
  active,
  desc,
  align = 'left',
  onClick,
}: {
  label: string;
  active: boolean;
  desc: boolean;
  align?: 'left' | 'right';
  onClick: () => void;
}) {
  const { t } = useTranslation('common');
  const hint = active
    ? (desc ? t('sortDescendingHint') : t('sortAscendingHint'))
    : t('sortSwitchHint');
  return (
    <button
      type="button"
      onClick={onClick}
      title={hint}
      className={`flex w-full min-w-0 items-center gap-1 transition-colors hover:text-[var(--text-primary)] ${
        align === 'right' ? 'justify-end' : ''
      } ${active ? 'text-[var(--brand-green)]' : ''}`}
    >
      <span className="truncate">{label}</span>
      {active
        ? (desc ? <ArrowDown className="h-3 w-3 shrink-0" /> : <ArrowUp className="h-3 w-3 shrink-0" />)
        : <ChevronsUpDown className="h-3 w-3 shrink-0 opacity-40" />}
    </button>
  );
}

/** Action buttons share one fixed width so the header and both views line up */
const ACTIONS_COLUMN = 'w-[6.25rem]';

export function GrowthResultList({
  entries,
  driveLetter,
  onOpenFolder,
  onSearchPath,
  onShowDetails,
  hasDiff,
}: GrowthResultListProps) {
  const { t: moduleT } = useTranslation('modules');
  const [viewMode, setViewMode] = useState<GrowthViewMode>(readViewMode);
  const [sort, setSort] = useState<GrowthSort>(readSort);
  const [filter, setFilter] = useState<GrowthFilterMode>('all');
  const [expandedKeys, setExpandedKeys] = useState<Set<string>>(() => new Set());
  const [lazyChildren, setLazyChildren] = useState<Map<string, GrowthNode[]>>(() => new Map());
  const [lazyLoading, setLazyLoading] = useState<Set<string>>(() => new Set());
  const [showAll, setShowAll] = useState(false);

  useEffect(() => {
    try {
      localStorage.setItem(VIEW_MODE_STORAGE_KEY, viewMode);
    } catch {
      // Storage can be unavailable in hardened webviews; the toggle still works for this session
    }
  }, [viewMode]);

  useEffect(() => {
    try {
      localStorage.setItem(SORT_STORAGE_KEY, JSON.stringify(sort));
    } catch {
      // Same as above: sorting keeps working, it just is not remembered
    }
  }, [sort]);

  // A new scan (or another drive) invalidates expansion state and lazily loaded levels
  useEffect(() => {
    setExpandedKeys(new Set());
    setLazyChildren(new Map());
    setLazyLoading(new Set());
    setShowAll(false);
  }, [entries]);

  const filteredEntries = useMemo(() => filterGrowthEntries(entries, filter), [entries, filter]);
  const tree = useMemo(() => buildGrowthTree(filteredEntries), [filteredEntries]);
  const sortedTree = useMemo(() => sortGrowthNodes(tree.nodes, sort), [tree, sort]);
  const sortedEntries = useMemo(() => sortGrowthEntries(filteredEntries, sort), [filteredEntries, sort]);

  // Lazily loaded levels follow the active column and the active filter too, otherwise
  // "only increases" would suddenly show a shrinking child under an expanded node.
  const resolvedLazyChildren = useMemo(() => {
    if (lazyChildren.size === 0) return lazyChildren;
    const next = new Map<string, GrowthNode[]>();
    for (const [key, children] of lazyChildren) {
      const kept = filter === 'all'
        ? children
        : children.filter((child) => (filter === 'increase' ? child.diff > 0 : child.diff < 0));
      next.set(key, sortGrowthNodes(kept, sort));
    }
    return next;
  }, [lazyChildren, sort, filter]);

  // Open the first two levels by default: the point of the tree is seeing which
  // folder inside `AppData` grew, and one click per node would hide exactly that.
  useEffect(() => {
    if (viewMode !== 'tree') return;
    const keys = new Set<string>();
    const walk = (list: GrowthNode[]) => {
      for (const node of list) {
        if (node.depth > 1 || node.children.length === 0) continue;
        keys.add(node.key);
        walk(node.children);
      }
    };
    walk(tree.nodes);
    setExpandedKeys(keys);
  }, [viewMode, tree]);

  const resolveChildren = useCallback(
    (node: GrowthNode) => resolvedLazyChildren.get(node.key) ?? node.children,
    [resolvedLazyChildren],
  );
  const rows = useMemo(
    () => (viewMode === 'tree' ? flattenGrowthNodes(sortedTree, expandedKeys, resolveChildren) : []),
    [viewMode, sortedTree, expandedKeys, resolveChildren],
  );

  const handleToggle = useCallback((node: GrowthNode) => {
    const willExpand = !expandedKeys.has(node.key);
    setExpandedKeys((current) => {
      const next = new Set(current);
      if (willExpand) next.add(node.key);
      else next.delete(node.key);
      return next;
    });
    if (!willExpand) return;
    // Children already present in the snapshot list: nothing to fetch
    if (node.children.length > 0) return;
    if (lazyChildren.has(node.key) || lazyLoading.has(node.key)) return;

    setLazyLoading((current) => new Set(current).add(node.key));
    getDiskGrowthDirectoryDetails(node.path, 0, LAZY_PAGE_SIZE, driveLetter)
      .then((result) => {
        const children = growthNodesFromDetails(result.entries, node.depth);
        setLazyChildren((map) => new Map(map).set(node.key, children));
      })
      .catch(() => {
        // Remember the empty result so the row stops retrying on every click
        setLazyChildren((map) => new Map(map).set(node.key, []));
      })
      .finally(() => {
        setLazyLoading((current) => {
          const next = new Set(current);
          next.delete(node.key);
          return next;
        });
      });
  }, [driveLetter, expandedKeys, lazyChildren, lazyLoading]);

  const handleSort = useCallback((key: GrowthSortKey) => {
    setSort((current) => nextGrowthSort(current, key));
  }, []);

  const renderRow = (node: GrowthNode) => {
    const expanded = expandedKeys.has(node.key);
    const loading = lazyLoading.has(node.key);
    const style = getGrowthStyle(node.level);
    const LevelIcon = style.icon;
    const modifiedLabel = formatModifiedTime(node.modified, true);
    const explanation = node.explanation
      || (node.hasOwnStats
        ? describeGrowth(node.level, node.diff)
        : moduleT('diskGrowth.aggregatedSummary', { count: node.changedCount }));
    const detailEntry = node.entry ?? (node.hasOwnStats ? growthEntryFromNode(node) : null);
    const isPlaceholder = !node.hasOwnStats;
    const indent = viewMode === 'tree' ? node.depth * INDENT_STEP : 0;

    return (
      <div className="flex items-center gap-3 px-4 py-2.5 hover:bg-[var(--bg-hover)] transition-colors group">
        <div
          className={`w-1.5 h-8 rounded-full shrink-0 ${
            node.diff > 0 ? 'bg-red-400' : node.diff < 0 ? 'bg-green-400' : 'bg-gray-300'
          }`}
        />

        <div className="flex-1 min-w-0">
          <div className="flex min-w-0 items-center gap-1" style={{ paddingLeft: indent }}>
            {viewMode === 'tree' && (
              <button
                type="button"
                onClick={() => handleToggle(node)}
                className="flex h-4 w-4 shrink-0 items-center justify-center rounded text-[var(--text-faint)] transition-colors hover:text-[var(--brand-green)]"
                title={expanded ? moduleT('diskGrowth.collapseLevel') : moduleT('diskGrowth.expandLevel')}
              >
                {loading
                  ? <Loader2 className="h-3 w-3 animate-spin" />
                  : <ChevronRight className={`h-3.5 w-3.5 transition-transform ${expanded ? 'rotate-90' : ''}`} />}
              </button>
            )}
            <Folder
              className={`h-3.5 w-3.5 shrink-0 ${isPlaceholder ? 'text-[var(--text-faint)]' : 'text-[var(--text-muted)]'}`}
            />
            <button
              type="button"
              onClick={() => onOpenFolder(node.path)}
              className={`min-w-0 truncate text-left text-[0.93rem] transition-colors hover:text-[var(--brand-green)] ${
                isPlaceholder ? 'text-[var(--text-muted)]' : 'text-[var(--text-primary)]'
              }`}
              title={buildPathTitle(node.path, node.modified, explanation)}
            >
              {viewMode === 'tree' ? node.name : simplifyPath(node.path)}
            </button>
            {isPlaceholder && (
              <span
                className="shrink-0 rounded bg-[var(--bg-hover)] px-1 py-px text-[0.7rem] text-[var(--text-faint)]"
                title={moduleT('diskGrowth.aggregatedHint')}
              >
                ≈ {moduleT('diskGrowth.aggregatedBadge')}
              </span>
            )}
          </div>
          <p
            className="mt-0.5 truncate text-[0.79rem] text-[var(--text-faint)]"
            style={{ paddingLeft: indent + (viewMode === 'tree' ? NAME_OFFSET : 0) }}
            title={explanation}
          >
            {explanation}
          </p>
        </div>

        <span
          className="w-20 shrink-0 whitespace-nowrap text-right text-[0.79rem] text-[var(--text-muted)] tabular-nums"
          title={modifiedLabel === '-'
            ? moduleT('diskGrowth.modifiedTimeMissing')
            : moduleT('diskGrowth.modifiedTimeTitle', { time: formatModifiedTime(node.modified) })}
        >
          {modifiedLabel}
        </span>

        <span className={`flex w-24 shrink-0 items-center justify-end gap-1 text-[0.86rem] ${
          isPlaceholder ? 'text-[var(--text-faint)]' : style.color
        }`}>
          {isPlaceholder ? '-' : (
            <>
              <LevelIcon className="w-3.5 h-3.5" />
              {style.label}
            </>
          )}
        </span>

        <span className="w-24 shrink-0 whitespace-nowrap text-right text-[0.93rem] font-medium text-[var(--text-primary)] tabular-nums">
          {isPlaceholder ? `≈ ${formatSize(node.size)}` : formatSize(node.size)}
        </span>

        <button
          type="button"
          onClick={() => detailEntry && onShowDetails(detailEntry)}
          disabled={!detailEntry}
          className={`flex w-28 shrink-0 items-center justify-end gap-1 whitespace-nowrap text-[0.93rem] font-medium tabular-nums ${
            node.diff > 0 ? 'text-red-500' : node.diff < 0 ? 'text-green-500' : 'text-[var(--text-faint)]'
          } ${detailEntry ? 'hover:underline underline-offset-4 cursor-pointer' : 'cursor-default'}`}
          title={detailEntry ? moduleT('diskGrowth.viewDetails') : undefined}
        >
          {isPlaceholder ? `≈ ${formatDiff(node.diff)}` : (node.diff === 0 ? '-' : formatDiff(node.diff))}
          {detailEntry && <ChevronRight className="hint-nudge h-3 w-3 shrink-0" aria-hidden="true" />}
        </button>

        <div className={`flex ${ACTIONS_COLUMN} shrink-0 items-center justify-end gap-1`}>
          <button
            type="button"
            onClick={() => onSearchPath(node.path)}
            className="p-1.5 rounded-lg text-[var(--text-muted)] hover:bg-[var(--brand-green-10)] hover:text-[var(--brand-green)] transition"
            title={moduleT('diskGrowth.searchPath')}
          >
            <Search className="w-4 h-4" />
          </button>
          <AiAskButton
            question={buildItemQuestion(node.path, 'folder')}
            skipLookup={isPlaceholder}
            className="inline-flex h-7 w-7 items-center justify-center"
          />
          <button
            type="button"
            onClick={() => onOpenFolder(node.path)}
            className="p-1.5 rounded-lg text-[var(--text-muted)] hover:bg-[var(--brand-green-10)] hover:text-[var(--brand-green)] transition"
            title={moduleT('diskGrowth.openFolder')}
          >
            <FolderOpen className="w-4 h-4" />
          </button>
        </div>
      </div>
    );
  };

  const buildFlatNode = (entry: DiskGrowthEntry): GrowthNode => ({
    key: normalizeDiskPath(entry.path),
    name: entry.path,
    path: entry.path,
    depth: 0,
    entry,
    hasOwnStats: true,
    oldSize: entry.old_size,
    size: entry.new_size,
    diff: entry.diff,
    diffPercent: entry.diff_percent,
    modified: entry.modified,
    level: entry.level,
    explanation: entry.explanation,
    children: [],
    changedCount: 1,
  });

  const visibleEntries = sortedEntries.slice(0, showAll ? sortedEntries.length : FLAT_PAGE_SIZE);
  const hasMore = viewMode === 'flat' && !showAll && sortedEntries.length > FLAT_PAGE_SIZE;

  return (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-1.5">
          {hasDiff && (
            <div className="flex items-center gap-0.5 rounded-lg bg-[var(--bg-hover)] p-0.5">
              {(['all', 'increase', 'decrease'] as GrowthFilterMode[]).map((mode) => {
                const label = mode === 'all'
                  ? moduleT('diskGrowth.filterAll')
                  : mode === 'increase' ? moduleT('diskGrowth.filterIncrease') : moduleT('diskGrowth.filterDecrease');
                const active = filter === mode;
                return (
                  <button
                    key={mode}
                    type="button"
                    onClick={() => setFilter(mode)}
                    className={`rounded-md px-2 py-0.5 text-[0.79rem] transition ${
                      active
                        ? 'bg-[var(--bg-card)] font-medium text-[var(--brand-green)] shadow-sm'
                        : 'text-[var(--text-muted)] hover:text-[var(--brand-green)]'
                    }`}
                  >
                    {label}
                  </button>
                );
              })}
            </div>
          )}
          {hasDiff && (
            <button
              type="button"
              onClick={() => setSort((current) => ({
                ...current,
                diffMode: current.diffMode === 'abs' ? 'signed' : 'abs',
              }))}
              className={`rounded-lg px-2 py-1 text-[0.79rem] transition ${
                sort.key === 'diff' && sort.diffMode === 'signed'
                  ? 'bg-[var(--bg-hover)] text-[var(--text-primary)]'
                  : 'text-[var(--text-muted)] hover:text-[var(--brand-green)]'
              }`}
              title={moduleT('diskGrowth.diffModeHint')}
            >
              {sort.diffMode === 'abs' ? moduleT('diskGrowth.diffModeAbs') : moduleT('diskGrowth.diffModeSigned')}
            </button>
          )}
        </div>

        <div className="flex items-center gap-0.5 rounded-lg bg-[var(--bg-hover)] p-0.5">
          {(['tree', 'flat'] as GrowthViewMode[]).map((mode) => {
            const active = viewMode === mode;
            return (
              <button
                key={mode}
                type="button"
                onClick={() => setViewMode(mode)}
                className={`rounded-md px-2 py-0.5 text-[0.79rem] transition ${
                  active
                    ? 'bg-[var(--bg-card)] font-medium text-[var(--brand-green)] shadow-sm'
                    : 'text-[var(--text-muted)] hover:text-[var(--brand-green)]'
                }`}
              >
                {mode === 'tree' ? moduleT('diskGrowth.viewTree') : moduleT('diskGrowth.viewFlat')}
              </button>
            );
          })}
        </div>
      </div>

      <div className="glass-list bg-[var(--bg-main)] rounded-xl overflow-hidden">
        <div className="flex items-center gap-3 px-4 py-2 border-b border-[var(--border-color)] text-[0.79rem] text-[var(--text-faint)]">
          <div className="w-1.5 shrink-0" />
          <div className="flex-1 min-w-0">
            <SortHeader
              label={moduleT('diskGrowth.folderHeader')}
              active={sort.key === 'name'}
              desc={sort.desc}
              onClick={() => handleSort('name')}
            />
          </div>
          <div className="w-20 shrink-0">
            <SortHeader
              label={moduleT('diskGrowth.changeTime')}
              active={sort.key === 'time'}
              desc={sort.desc}
              align="right"
              onClick={() => handleSort('time')}
            />
          </div>
          <div className="w-24 shrink-0 text-right">{moduleT('diskGrowth.changeLevel')}</div>
          <div className="w-24 shrink-0">
            <SortHeader
              label={moduleT('diskGrowth.currentSizeHeader')}
              active={sort.key === 'size'}
              desc={sort.desc}
              align="right"
              onClick={() => handleSort('size')}
            />
          </div>
          <div className="w-28 shrink-0">
            <SortHeader
              label={moduleT('diskGrowth.differenceHeader')}
              active={sort.key === 'diff'}
              desc={sort.desc}
              align="right"
              onClick={() => handleSort('diff')}
            />
          </div>
          {/* Same width as the row action cluster, otherwise the numbers drift one step left of their header */}
          <div className={`${ACTIONS_COLUMN} shrink-0`} />
        </div>

        {entries.length === 0 && (
          <div className="px-4 py-6 text-center text-[0.86rem] text-[var(--text-muted)]">
            {moduleT('diskGrowth.firstSnapshot')}
          </div>
        )}

        {viewMode === 'tree'
          ? rows.map((node) => {
            const expanded = expandedKeys.has(node.key);
            const loading = lazyLoading.has(node.key);
            const showEmptyHint = expanded
              && !loading
              && resolveChildren(node).length === 0;
            return (
              <Fragment key={node.key}>
                {renderRow(node)}
                {showEmptyHint && (
                  <p
                    className="pb-2 pr-4 text-[0.79rem] text-[var(--text-faint)]"
                    style={{ paddingLeft: 16 + NAME_OFFSET + (node.depth + 1) * INDENT_STEP }}
                  >
                    {moduleT('diskGrowth.noDeeperChange')}
                  </p>
                )}
              </Fragment>
            );
          })
          : visibleEntries.map((entry) => (
            <Fragment key={entry.path}>{renderRow(buildFlatNode(entry))}</Fragment>
          ))}

        {entries.length > 0
          && (viewMode === 'tree' ? rows.length === 0 : visibleEntries.length === 0)
          && (
            <div className="px-4 py-6 text-center text-[0.86rem] text-[var(--text-muted)]">
              {moduleT('diskGrowth.filterEmpty')}
            </div>
          )}

        {hasMore && (
          <button
            type="button"
            onClick={() => setShowAll(true)}
            className="w-full py-3 text-center text-[0.93rem] text-[var(--brand-green)] hover:bg-[var(--bg-hover)] transition-colors"
          >
            {moduleT('diskGrowth.showAll', { count: sortedEntries.length })}
          </button>
        )}
      </div>
    </div>
  );
}

export default GrowthResultList;
