// ============================================================================
// Growth result tree
//
// Folds the backend's flat changed-directory list into a folder tree.
//
// Why this exists: `compare_snapshots` returns every changed directory at a
// mixed depth (1-4) sorted by |diff|, and `remove_redundant_parent_entries`
// drops parents whose change equals a single child. Rendered as-is, `C:/Windows`
// and `C:/Users/x/AppData/Roaming` sit side by side with no hint that one is
// nested in the other, and some parent levels simply do not exist in the data.
//
// Two rules keep the tree honest:
// - Directories missing from the result become placeholder nodes whose figures
//   are the sum of their changed children, flagged via `hasOwnStats = false`
//   so the UI can mark them with "≈".
// - Sorting only reorders siblings; a parent never moves below its children,
//   otherwise "sort by size" would degrade back into a flat list.
// ============================================================================

import type { DiskGrowthDetailEntry, DiskGrowthEntry } from '../api/commands';
import type { GrowthLevel } from './growthFormat';

const GIGABYTE = 1024 * 1024 * 1024;
const FAST_THRESHOLD = 300 * 1024 * 1024;

/** Which change direction the result list should keep */
export type GrowthFilterMode = 'all' | 'increase' | 'decrease';

/** How "change amount" is compared: absolute value treats growth and shrink as equally large changes */
export type GrowthDiffMode = 'abs' | 'signed';

export type GrowthSortKey = 'name' | 'size' | 'diff' | 'time';

export type GrowthViewMode = 'tree' | 'flat';

export interface GrowthSort {
  key: GrowthSortKey;
  desc: boolean;
  diffMode: GrowthDiffMode;
}

/** Default order: biggest absolute change first, matching the backend's own ordering */
export const DEFAULT_GROWTH_SORT: GrowthSort = { key: 'diff', desc: true, diffMode: 'abs' };

const DEFAULT_SORT_DESC: Record<GrowthSortKey, boolean> = {
  name: false,
  size: true,
  diff: true,
  time: true,
};

/** Clicking the same column toggles direction; clicking another column uses that column's natural direction */
export function nextGrowthSort(current: GrowthSort, key: GrowthSortKey): GrowthSort {
  return current.key === key
    ? { ...current, key, desc: !current.desc }
    : { ...current, key, desc: DEFAULT_SORT_DESC[key] };
}

export interface GrowthNode {
  /** Normalized full path (drive included, lowercase) - stable key for expansion state */
  key: string;
  /** Last path segment, used for display */
  name: string;
  /** Full path with original casing */
  path: string;
  /** Level below the scanned drive root, 0 = first level */
  depth: number;
  /** Backend entry for this directory; null for placeholder and lazily loaded nodes */
  entry: DiskGrowthEntry | null;
  /** false = figures are the sum of changed children only, UI shows "≈" */
  hasOwnStats: boolean;
  oldSize: number;
  size: number;
  diff: number;
  diffPercent: number;
  modified: number;
  level: GrowthLevel;
  /** Explanation sentence from the backend when available */
  explanation: string;
  children: GrowthNode[];
  /** Directories below this one that changed (self included when it has own stats) */
  changedCount: number;
}

export interface GrowthTree {
  nodes: GrowthNode[];
  /** Total number of nodes, placeholders included */
  total: number;
  /** Whether any node has children, i.e. whether a tree view adds anything over a flat list */
  hasBranches: boolean;
}

/** Same thresholds as the backend: 1 GB significant, 300 MB fast */
export function growthLevelFromDiff(diff: number, oldSize: number): GrowthLevel {
  if (oldSize === 0 && diff > 0) return 'new';
  if (diff >= GIGABYTE) return 'significant';
  if (diff >= FAST_THRESHOLD) return 'fast';
  if (diff > 0) return 'minor';
  if (diff < 0) return 'decreased';
  return 'stable';
}

interface SplitPath {
  /** Lowercase drive prefix such as "c:"; empty for paths without a drive */
  drive: string;
  /** Path segments below the drive */
  segments: string[];
}

function splitPath(path: string): SplitPath {
  const normalized = path.replace(/\\/g, '/').replace(/\/+$/g, '');
  const driveMatch = /^([a-zA-Z]:)(\/|$)/.exec(normalized);
  if (driveMatch) {
    return {
      drive: driveMatch[1].toLowerCase(),
      segments: normalized.slice(driveMatch[0].length).split('/').filter(Boolean),
    };
  }
  return { drive: '', segments: normalized.split('/').filter(Boolean) };
}

function createNode(name: string, path: string, key: string): GrowthNode {
  return {
    key,
    name,
    path,
    depth: 0,
    entry: null,
    hasOwnStats: false,
    oldSize: 0,
    size: 0,
    diff: 0,
    diffPercent: 0,
    modified: 0,
    level: 'stable',
    explanation: '',
    children: [],
    changedCount: 0,
  };
}

export function filterGrowthEntries(entries: DiskGrowthEntry[], filter: GrowthFilterMode): DiskGrowthEntry[] {
  if (filter === 'increase') return entries.filter((entry) => entry.diff > 0);
  if (filter === 'decrease') return entries.filter((entry) => entry.diff < 0);
  return entries;
}

/**
 * Build the folder tree from the flat changed-directory list.
 *
 * The scanned drive is not a node of its own: it is already shown in the module
 * header ("当前分析：C 盘"), so the first level here is `Users`, `Windows`, ...
 */
export function buildGrowthTree(entries: DiskGrowthEntry[]): GrowthTree {
  const byKey = new Map<string, GrowthNode>();
  const roots: GrowthNode[] = [];

  for (const entry of entries) {
    const { drive, segments } = splitPath(entry.path);
    if (segments.length === 0) continue;
    let parentKey = drive;
    for (let index = 0; index < segments.length; index += 1) {
      const name = segments[index];
      const key = `${parentKey}/${name.toLowerCase()}`;
      let node = byKey.get(key);
      if (!node) {
        const path = segments.slice(0, index + 1).join('/');
        node = createNode(name, drive ? `${drive.toUpperCase()}/${path}` : path, key);
        byKey.set(key, node);
        const parent = parentKey === drive ? null : byKey.get(parentKey);
        if (parent) parent.children.push(node);
        else roots.push(node);
      }
      if (index === segments.length - 1) {
        // A directory that is itself in the result always wins over the placeholder created on the way down.
        node.entry = entry;
        node.path = entry.path;
        node.hasOwnStats = true;
        node.oldSize = entry.old_size;
        node.size = entry.new_size;
        node.diff = entry.diff;
        node.diffPercent = entry.diff_percent;
        node.modified = entry.modified;
        node.level = entry.level;
        node.explanation = entry.explanation;
      }
      parentKey = key;
    }
  }

  /**
   * Collapse runs of placeholders that each have a single placeholder child.
   *
   * `C:/Users/x/AppData` alone in the result makes both `Users` and `x` placeholders,
   * and two stacked "≈" rows with identical numbers read as a bug. Joining them into
   * `Users/x` keeps the hierarchy readable without inventing figures.
   */
  const collapseChains = (node: GrowthNode): GrowthNode => {
    node.children = node.children.map(collapseChains);
    const [onlyChild] = node.children;
    if (!node.hasOwnStats && node.children.length === 1 && onlyChild && !onlyChild.hasOwnStats) {
      return { ...onlyChild, name: `${node.name}/${onlyChild.name}` };
    }
    return node;
  };
  const collapsedRoots = roots.map(collapseChains);

  let total = 0;
  const finalize = (node: GrowthNode, depth: number): void => {
    node.depth = depth;
    total += 1;
    if (node.children.length === 0) {
      node.changedCount = node.hasOwnStats ? 1 : 0;
      return;
    }
    let oldSize = 0;
    let size = 0;
    let diff = 0;
    let modified = 0;
    let changedCount = node.hasOwnStats ? 1 : 0;
    for (const child of node.children) {
      finalize(child, depth + 1);
      oldSize += child.oldSize;
      size += child.size;
      diff += child.diff;
      if (child.modified > modified) modified = child.modified;
      changedCount += child.changedCount;
    }
    node.changedCount = changedCount;
    if (!node.hasOwnStats) {
      // Sum of changed children only: unchanged siblings are invisible in the snapshot diff,
      // which is why the UI marks these nodes with "≈" instead of presenting them as exact sizes.
      node.oldSize = oldSize;
      node.size = size;
      node.diff = diff;
      node.diffPercent = oldSize > 0 ? (diff / oldSize) * 100 : 100;
      node.modified = modified;
      node.level = growthLevelFromDiff(diff, oldSize);
    }
  };
  for (const root of collapsedRoots) finalize(root, 0);

  return {
    nodes: collapsedRoots,
    total,
    hasBranches: collapsedRoots.some((root) => root.children.length > 0),
  };
}

/** Build the next level of nodes from a directory-detail response (children below the snapshot's aggregation depth) */
export function growthNodesFromDetails(
  details: DiskGrowthDetailEntry[],
  parentDepth: number,
): GrowthNode[] {
  return details.map((detail) => {
    const { drive, segments } = splitPath(detail.path);
    const node = createNode(
      detail.name,
      detail.path,
      `${drive}${segments.map((segment) => `/${segment}`).join('').toLowerCase()}`,
    );
    node.depth = parentDepth + 1;
    node.hasOwnStats = true;
    node.oldSize = detail.old_size;
    node.size = detail.new_size;
    node.diff = detail.diff;
    node.diffPercent = detail.old_size > 0 ? (detail.diff / detail.old_size) * 100 : 100;
    node.modified = detail.modified;
    node.level = detail.level;
    node.changedCount = 1;
    return node;
  });
}

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

function sortValue(node: GrowthNode, sort: GrowthSort): number {
  if (sort.key === 'size') return node.size;
  if (sort.key === 'time') return node.modified;
  return sort.diffMode === 'abs' ? Math.abs(node.diff) : node.diff;
}

function compareNodes(left: GrowthNode, right: GrowthNode, sort: GrowthSort): number {
  if (sort.key === 'name') {
    const result = collator.compare(left.name, right.name);
    return sort.desc ? -result : result;
  }
  if (sort.key === 'time') {
    // Missing timestamps always sort last, never flipped to the front by descending order.
    if (left.modified === right.modified) return collator.compare(left.name, right.name);
    if (left.modified === 0) return 1;
    if (right.modified === 0) return -1;
  }
  const delta = sortValue(left, sort) - sortValue(right, sort);
  if (delta !== 0) return sort.desc ? -delta : delta;
  return collator.compare(left.name, right.name);
}

/**
 * Reorder the tree by the given key.
 *
 * Sorting never rebuilds the tree: path splitting and aggregation are order
 * independent, so only each level's child array is re-sorted (nodes are reused,
 * only the shell object is recreated).
 */
export function sortGrowthNodes(nodes: GrowthNode[], sort: GrowthSort): GrowthNode[] {
  return nodes
    .map((node) => (node.children.length > 0 ? { ...node, children: sortGrowthNodes(node.children, sort) } : node))
    .sort((left, right) => compareNodes(left, right, sort));
}

/** Flat list ordering must match the tree ordering, otherwise "by size" means two different things in the two views */
export function sortGrowthEntries(entries: DiskGrowthEntry[], sort: GrowthSort): DiskGrowthEntry[] {
  const next = entries.slice();
  next.sort((left, right) => {
    if (sort.key === 'name') {
      const result = collator.compare(left.path, right.path);
      return sort.desc ? -result : result;
    }
    if (sort.key === 'time') {
      if (left.modified === right.modified) return collator.compare(left.path, right.path);
      if (left.modified === 0) return 1;
      if (right.modified === 0) return -1;
    }
    const leftValue = sort.key === 'size'
      ? left.new_size
      : sort.diffMode === 'abs' ? Math.abs(left.diff) : left.diff;
    const rightValue = sort.key === 'size'
      ? right.new_size
      : sort.diffMode === 'abs' ? Math.abs(right.diff) : right.diff;
    const delta = leftValue - rightValue;
    if (delta !== 0) return sort.desc ? -delta : delta;
    return collator.compare(left.path, right.path);
  });
  return next;
}

/**
 * Flatten the tree into visible rows (descendants of collapsed directories are omitted).
 *
 * `resolveChildren` lets the caller inject children loaded on demand for nodes
 * the snapshot aggregation stopped at; expansion state stays keyed by full path,
 * so the same directories stay expanded after a rescan.
 */
export function flattenGrowthNodes(
  nodes: GrowthNode[],
  expandedKeys: Set<string>,
  resolveChildren?: (node: GrowthNode) => GrowthNode[],
): GrowthNode[] {
  const rows: GrowthNode[] = [];
  const walk = (list: GrowthNode[]): void => {
    for (const node of list) {
      rows.push(node);
      if (!expandedKeys.has(node.key)) continue;
      const children = resolveChildren ? resolveChildren(node) : node.children;
      if (children.length > 0) walk(children);
    }
  };
  walk(nodes);
  return rows;
}
