// ============================================================================
// File directory tree
// Folds the flat file list from scan results into a directory tree by path
//
// Note: scan results were originally one file per line, so "delete everything in this folder" meant clicking one by one.
// Here we build a tree by common path prefixes; checking a directory selects the whole subtree at once.
// ============================================================================

import { stripWindowsDevicePrefix } from './searchEngine';

/** Building the tree only needs "path + size"; other fields are carried on the leaf as is */
export interface TreeSourceFile {
  path: string;
  size: number;
  /** Modification time (Unix seconds). Sources without this field use 0 and sort last uniformly */
  modified_time?: number;
}

export interface FileTreeFolder<T extends TreeSourceFile> {
  /** Stable key: directories use `d|full path`, files use `f|full path` */
  key: string;
  /** Display name (last path segment) */
  name: string;
  /** Full path (directories without trailing backslash) */
  path: string;
  /** Level relative to the tree root, 0 = first level */
  depth: number;
  isDir: true;
  /** Number of descendant files */
  fileCount: number;
  /** Total size of descendant files */
  totalSize: number;
  /** Newest modification time among descendant files (Unix seconds); used by "sort by time" to rank directories */
  latestModified: number;
  /**
   * Paths of all descendant files.
   *
   * Note: checking a directory relies on this to submit a whole batch of paths at once — toggling individually would trigger dozens of
   * setState calls, visibly lagging with hundreds of files. Computed once at tree build, submitted in bulk when checked.
   */
  paths: string[];
  children: FileTreeNode<T>[];
}

export interface FileTreeFile<T extends TreeSourceFile> {
  key: string;
  name: string;
  path: string;
  depth: number;
  isDir: false;
  /** Modification time (Unix seconds); 0 if the source did not provide it */
  modifiedTime: number;
  file: T;
}

export type FileTreeNode<T extends TreeSourceFile> =
  | FileTreeFolder<T>
  | FileTreeFile<T>;

/** Sort key: name (natural order) / size / modification time */
export type TreeSortKey = 'name' | 'size' | 'time';

export interface TreeSort {
  key: TreeSortKey;
  /** true = largest to smallest, newest to oldest; for name sort, true = reverse order */
  desc: boolean;
}

/** Default direction for each sort key: size and time default to "largest/newest first" */
export const DEFAULT_SORT_DESC: Record<TreeSortKey, boolean> = {
  name: false,
  size: true,
  time: true,
};

export interface FileTree<T extends TreeSourceFile> {
  /** Common ancestor directory of all files (shown as the tree header, e.g. C:\Users\x\AppData\Local\Temp) */
  rootPath: string;
  /** First-level nodes under the root */
  nodes: FileTreeNode<T>[];
  /** Whether there is a real hierarchy; no need to draw a tree if everything is on one level */
  hasBranches: boolean;
  fileCount: number;
  totalSize: number;
}

/** Normalize separators and device prefixes so a given path has only one form when compared */
function normalizePath(value: string): string {
  // stripWindowsDevicePrefix internally does two split+join passes, which is costly for tens of thousands of paths;
  // device prefixes appear only in a few scan results, so check the start first and skip the vast majority directly.
  let path = value.startsWith('\\\\') || value.startsWith('\\??\\')
    ? stripWindowsDevicePrefix(value)
    : value;
  if (path.indexOf('/') !== -1) path = path.replace(/\//g, '\\');
  if (path.endsWith('\\')) path = path.replace(/\\+$/, '');
  return path;
}

/** Segment comparison: case-insensitive (Windows paths are case-insensitive), with an equality fast path to avoid needless toLowerCase */
function sameSegment(left: string, right: string): boolean {
  return left === right || left.toLowerCase() === right.toLowerCase();
}

function joinPath(parent: string, name: string): string {
  return parent ? `${parent}\\${name}` : name;
}

/** Comparator: directories first, then by natural name order within the same kind (2 before 10) */
const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

function sortNodes<T extends TreeSourceFile>(nodes: FileTreeNode<T>[]): void {
  nodes.sort((left, right) => {
    if (left.isDir !== right.isDir) return left.isDir ? -1 : 1;
    return collator.compare(left.name, right.name);
  });
  for (const node of nodes) {
    if (node.isDir) sortNodes(node.children);
  }
}

/** Aggregate file count and size bottom-up for directories, while collecting descendant file paths */
function aggregate<T extends TreeSourceFile>(node: FileTreeFolder<T>): void {
  if (node.children.length === 0) {
    // Empty directories never appear in scan results, but keep a fallback to avoid division by zero or empty-array exceptions
    node.paths = [];
    return;
  }
  const paths: string[] = [];
  let size = 0;
  let latest = 0;
  for (const child of node.children) {
    if (child.isDir) {
      aggregate(child);
      // Push one by one rather than spread into arguments: spreading tens of thousands of descendant paths hits the argument-count limit
      for (const path of child.paths) paths.push(path);
      size += child.totalSize;
      if (child.latestModified > latest) latest = child.latestModified;
    } else {
      paths.push(child.path);
      size += child.file.size;
      if (child.modifiedTime > latest) latest = child.modifiedTime;
    }
  }
  node.paths = paths;
  node.fileCount = paths.length;
  node.totalSize = size;
  node.latestModified = latest;
}

/**
 * Build a tree from a flat file list.
 *
 * `getPath` defaults to `file.path`; modules like social cleanup with a different file object shape can supply their own.
 */
export function buildFileTree<T extends TreeSourceFile>(
  files: T[],
  getPath: (file: T) => string = (file) => file.path,
  getModified: (file: T) => number = (file) => file.modified_time ?? 0,
): FileTree<T> {
  const total = files.length;
  const paths = new Array<string>(total);
  const segmentLists = new Array<string[]>(total);
  for (let index = 0; index < total; index += 1) {
    const path = normalizePath(getPath(files[index]));
    paths[index] = path;
    segmentLists[index] = path.split('\\').filter(Boolean);
  }

  // Common ancestor directory: keep only "directory segments", narrowing the prefix length per file.
  // Use indices instead of slicing the array each time; with 34k files this saves tens of thousands of temporary array allocations.
  let prefixLength = total === 0 ? 0 : Math.max(0, segmentLists[0].length - 1);
  const firstSegments = segmentLists[0] ?? [];
  for (let index = 1; index < total && prefixLength > 0; index += 1) {
    const segments = segmentLists[index];
    const limit = Math.min(prefixLength, Math.max(0, segments.length - 1));
    let matched = 0;
    while (matched < limit && sameSegment(firstSegments[matched], segments[matched])) {
      matched += 1;
    }
    prefixLength = matched;
  }
  const rootPath = firstSegments.slice(0, prefixLength).join('\\');

  const root: FileTreeFolder<T> = {
    key: `d|${rootPath}`,
    name: rootPath || '',
    path: rootPath,
    depth: 0,
    isDir: true,
    fileCount: 0,
    totalSize: 0,
    latestModified: 0,
    paths: [],
    children: [],
  };

  /*
    Child directory index: parent node → lowercase directory name → child node.

    Note: originally each level used children.find for a linear scan; with thousands of child directories under one parent
    it degraded to quadratic (34k files took over 200ms). With a Map, each level is O(1).
  */
  const childIndex = new Map<FileTreeFolder<T>, Map<string, FileTreeFolder<T>>>();

  for (let index = 0; index < total; index += 1) {
    const segments = segmentLists[index];
    // Segments after the file name are directory levels; a path equal to the root itself has no file name, skip it
    if (segments.length <= prefixLength) continue;

    let cursor = root;
    for (let depth = prefixLength; depth < segments.length - 1; depth += 1) {
      const name = segments[depth];
      let buckets = childIndex.get(cursor);
      if (!buckets) {
        buckets = new Map();
        childIndex.set(cursor, buckets);
      }
      const lowered = name.toLowerCase();
      let next = buckets.get(lowered);
      if (!next) {
        const path = joinPath(cursor.path, name);
        next = {
          key: `d|${path}`,
          name,
          path,
          depth: cursor.depth + 1,
          isDir: true,
          fileCount: 0,
          totalSize: 0,
          latestModified: 0,
          paths: [],
          children: [],
        };
        buckets.set(lowered, next);
        cursor.children.push(next);
      }
      cursor = next;
    }

    cursor.children.push({
      key: `f|${paths[index]}`,
      name: segments[segments.length - 1],
      path: paths[index],
      depth: cursor.depth + 1,
      isDir: false,
      modifiedTime: getModified(files[index]),
      file: files[index],
    });
  }

  aggregate(root);
  sortNodes(root.children);

  return {
    rootPath,
    nodes: root.children,
    // If everything is "files directly under the root" the tree is meaningless; the caller just renders flat
    hasBranches: root.children.some((child) => child.isDir),
    fileCount: root.fileCount,
    totalSize: root.totalSize,
  };
}

/**
 * Reorder the whole tree by the given key.
 *
 * Note: do not rebuild the tree — path splitting, directory merging and size aggregation are order-independent,
 * reordering only sorts each level's child array by the new key (nodes are reused, only the directory shell is rebuilt).
 *
 * Two rules:
 * - Directories always come before files, matching Explorer; sort direction only applies within the same kind.
 * - A directory's "size" is the sum of descendants and its "time" is the newest among them —
 *   otherwise a directory with 80,000 files would never rank high and sorting by size would be pointless.
 */
export function sortTree<T extends TreeSourceFile>(
  tree: FileTree<T>,
  sort: TreeSort,
): FileTree<T> {
  const { key, desc } = sort;
  // Ascending name is already the default order from tree building, no need to sort again
  if (key === 'name' && !desc) return tree;

  const compare = (left: FileTreeNode<T>, right: FileTreeNode<T>): number => {
    if (left.isDir !== right.isDir) return left.isDir ? -1 : 1;
    let result: number;
    if (key === 'name') {
      result = collator.compare(left.name, right.name);
    } else if (key === 'size') {
      result = (left.isDir ? left.totalSize : left.file.size)
        - (right.isDir ? right.totalSize : right.file.size);
    } else {
      result = (left.isDir ? left.latestModified : left.modifiedTime)
        - (right.isDir ? right.latestModified : right.modifiedTime);
    }
    return desc ? -result : result;
  };

  const walk = (nodes: FileTreeNode<T>[]): FileTreeNode<T>[] => {
    const next = nodes.map((node): FileTreeNode<T> => (
      node.isDir ? { ...node, children: walk(node.children) } : node
    ));
    return next.sort(compare);
  };

  return { ...tree, nodes: walk(tree.nodes) };
}

/**
 * Flat list sort (used by the "All files" modal that does not build a tree).
 *
 * Note: places like AI cache details get a plain set of files with no directory levels to fold,
 * and the sort rules must match the directory-tree modal exactly, otherwise "by size" means different things in the two windows.
 * The same three rules apply: natural name order, size by bytes, time by Unix seconds;
 * missing times (0) always sort last, never flipped to the front by ascending/descending.
 */
export function sortFileList<T>(
  files: T[],
  sort: TreeSort,
  accessors: {
    getName: (file: T) => string;
    getSize: (file: T) => number;
    getTime: (file: T) => number;
  },
): T[] {
  const { key, desc } = sort;
  const next = files.slice();
  next.sort((left, right) => {
    if (key === 'name') {
      const result = collator.compare(accessors.getName(left), accessors.getName(right));
      return desc ? -result : result;
    }
    if (key === 'size') {
      const result = accessors.getSize(left) - accessors.getSize(right);
      return desc ? -result : result;
    }
    const leftTime = accessors.getTime(left);
    const rightTime = accessors.getTime(right);
    if (leftTime === rightTime) return 0;
    // Items without timestamps go last in both ascending and descending order: treating them as "oldest" would push them to the front in descending
    if (leftTime === 0) return 1;
    if (rightTime === 0) return -1;
    return desc ? rightTime - leftTime : leftTime - rightTime;
  });
  return next;
}

/**
 * Flatten the tree into visible rows (descendants of collapsed directories are omitted) for the virtual list.
 *
 * Expansion state is held by the caller: directory keys are stable (the full path), so after a rescan the same directories
 * remain expanded and users do not have to click them open again.
 */
export function flattenFileTree<T extends TreeSourceFile>(
  nodes: FileTreeNode<T>[],
  expandedKeys: Set<string>,
): FileTreeNode<T>[] {
  const rows: FileTreeNode<T>[] = [];
  const walk = (list: FileTreeNode<T>[]) => {
    for (const node of list) {
      rows.push(node);
      if (node.isDir && expandedKeys.has(node.key)) {
        walk(node.children);
      }
    }
  };
  walk(nodes);
  return rows;
}

/** Collect keys of all directory nodes, for "expand all" */
export function collectFolderKeys<T extends TreeSourceFile>(
  nodes: FileTreeNode<T>[],
): string[] {
  const keys: string[] = [];
  const walk = (list: FileTreeNode<T>[]) => {
    for (const node of list) {
      if (!node.isDir) continue;
      keys.push(node.key);
      walk(node.children);
    }
  };
  walk(nodes);
  return keys;
}

/**
 * Compute once how many selected files are under each directory, for tri-state checkboxes.
 *
 * Note: do not traverse descendant paths for every directory (rows × files degenerates to quadratic),
 * instead walk the tree once and aggregate leaf selection status upward.
 */
export function computeSelectedCounts<T extends TreeSourceFile>(
  nodes: FileTreeNode<T>[],
  selectedPaths: Set<string>,
): Map<string, number> {
  const counts = new Map<string, number>();

  const walk = (list: FileTreeNode<T>[]): number => {
    let selected = 0;
    for (const node of list) {
      if (node.isDir) {
        const childSelected = walk(node.children);
        counts.set(node.key, childSelected);
        selected += childSelected;
      } else if (selectedPaths.has(node.path)) {
        selected += 1;
      }
    }
    return selected;
  };

  walk(nodes);
  return counts;
}
