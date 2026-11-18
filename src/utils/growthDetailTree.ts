// ============================================================================
// Growth detail tree (drill-down modal)
//
// The modal used to show two independent panels: changed subdirectories on the
// left, changed files of the current directory on the right. That splits one
// hierarchy into two lists, and once the snapshot aggregation runs out (depth 4)
// the left panel goes empty while the right one dumps thousands of loose files,
// so `roaming/tg/...` and `roaming/GitHub Desktop/...` end up mixed together.
//
// Here both responses are folded into ONE tree:
// - Directories reported by the snapshot keep their own sizes and diffs.
// - Files are placed by their path, and every missing intermediate directory is
//   synthesized (`hasOwnStats = false`, figures summed from the changed files
//   below it) so drilling down always lands on real files.
// ============================================================================

import type { DiskGrowthDetailEntry, DiskGrowthFileDetailEntry } from '../api/commands';
import type { GrowthLevel } from './growthFormat';
import { growthLevelFromDiff } from './growthTree';

export interface GrowthDetailNode {
  /** Normalized path, used as the expansion key */
  key: string;
  path: string;
  name: string;
  isDir: boolean;
  /** Level below the opened directory, 0 = direct child */
  depth: number;
  oldSize: number;
  size: number;
  diff: number;
  modified: number;
  level: GrowthLevel;
  /** false = folder that only exists as a common path of changed files; figures sum those files */
  hasOwnStats: boolean;
  /** Changed files below this folder (direct children only for files) */
  fileCount: number;
  children: GrowthDetailNode[];
}

interface MutableNode extends GrowthDetailNode {
  children: GrowthDetailNode[];
}

function normalizePath(value: string): string {
  return value.replace(/\\/g, '/').replace(/\/+$/g, '').toLowerCase();
}

/** Path segments of `child` relative to `parent`, or null when it is not below it */
function relativeSegments(child: string, parent: string): string[] | null {
  const childPath = child.replace(/\\/g, '/').replace(/\/+$/g, '');
  const parentPath = parent.replace(/\\/g, '/').replace(/\/+$/g, '');
  if (childPath.length <= parentPath.length + 1) return null;
  if (childPath.slice(0, parentPath.length).toLowerCase() !== parentPath.toLowerCase()) return null;
  if (childPath[parentPath.length] !== '/') return null;
  return childPath.slice(parentPath.length + 1).split('/').filter(Boolean);
}

function createDirNode(name: string, path: string, depth: number): MutableNode {
  return {
    key: normalizePath(path),
    path,
    name,
    isDir: true,
    depth,
    oldSize: 0,
    size: 0,
    diff: 0,
    modified: 0,
    level: 'stable',
    hasOwnStats: false,
    fileCount: 0,
    children: [],
  };
}

/** Fold aggregated figures upward for synthesized folders and count the files below each one */
function finalizeSynthesized(node: MutableNode): { files: number } {
  if (node.children.length === 0) {
    node.fileCount = node.isDir ? 0 : 1;
    return { files: node.isDir ? 0 : 1 };
  }
  let oldSize = 0;
  let size = 0;
  let diff = 0;
  let modified = 0;
  let files = 0;
  for (const child of node.children) {
    const childFiles = finalizeSynthesized(child as MutableNode);
    oldSize += child.oldSize;
    size += child.size;
    diff += child.diff;
    if (child.modified > modified) modified = child.modified;
    files += childFiles.files;
  }
  node.fileCount = files;
  if (!node.hasOwnStats) {
    // Only the changed files are visible here, so these figures are a lower bound,
    // not the folder's real size; the UI marks them with "≈".
    node.oldSize = oldSize;
    node.size = size;
    node.diff = diff;
    node.modified = modified;
    node.level = growthLevelFromDiff(diff, oldSize);
  }
  return { files };
}

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

function sortDetailNodes(nodes: GrowthDetailNode[]): void {
  nodes.sort((left, right) => {
    if (left.isDir !== right.isDir) return left.isDir ? -1 : 1;
    const delta = Math.abs(right.diff) - Math.abs(left.diff);
    if (delta !== 0) return delta;
    return collator.compare(left.name, right.name);
  });
  for (const node of nodes) {
    if (node.children.length > 0) sortDetailNodes(node.children);
  }
}

/**
 * Build one level of the detail tree for `parentPath`.
 *
 * `dirs` comes from the snapshot (changed subdirectories), `files` from the file
 * shard (direct files, or the whole changed subtree when the directory sits at
 * the snapshot depth boundary). Both are merged, and files are nested under the
 * directories their paths imply.
 */
export function buildDetailChildren(
  parentPath: string,
  parentDepth: number,
  dirs: DiskGrowthDetailEntry[],
  files: DiskGrowthFileDetailEntry[],
): GrowthDetailNode[] {
  const index = new Map<string, MutableNode>();
  const roots: MutableNode[] = [];

  const push = (node: MutableNode, parent: MutableNode | null) => {
    index.set(node.key, node);
    if (parent) parent.children.push(node);
    else roots.push(node);
  };

  for (const dir of dirs) {
    // Real snapshot directories are always direct children of the parent being expanded
    push({
      key: normalizePath(dir.path),
      path: dir.path,
      name: dir.name,
      isDir: true,
      depth: parentDepth + 1,
      oldSize: dir.old_size,
      size: dir.new_size,
      diff: dir.diff,
      modified: dir.modified,
      level: dir.level,
      hasOwnStats: true,
      fileCount: 0,
      children: [],
    }, null);
  }

  for (const file of files) {
    const segments = relativeSegments(file.path, parentPath);
    if (!segments || segments.length === 0) continue;

    let cursor: MutableNode | null = null;
    const basePath = parentPath.replace(/\\/g, '/').replace(/\/+$/g, '');
    for (let index2 = 0; index2 < segments.length - 1; index2 += 1) {
      const dirPath = `${basePath}/${segments.slice(0, index2 + 1).join('/')}`;
      const key = normalizePath(dirPath);
      let next = index.get(key);
      if (!next) {
        next = createDirNode(segments[index2], dirPath, parentDepth + 1 + index2);
        push(next, cursor);
      }
      cursor = next;
    }

    const fileNode: MutableNode = {
      key: normalizePath(file.path),
      path: file.path,
      name: file.name,
      isDir: false,
      depth: parentDepth + 1 + Math.max(0, segments.length - 1),
      oldSize: file.old_size,
      size: file.new_size,
      diff: file.diff,
      modified: file.modified,
      level: file.level,
      hasOwnStats: true,
      fileCount: 1,
      children: [],
    };
    // A file with the same path as a reported directory cannot happen, but keep the index consistent
    push(fileNode, cursor);
  }

  for (const root of roots) finalizeSynthesized(root);
  sortDetailNodes(roots);
  return roots;
}

/** Attach children recursively for every directory whose data has been loaded */
export function buildDetailTree(
  rootPath: string,
  dirsByParent: Map<string, DiskGrowthDetailEntry[]>,
  filesByParent: Map<string, DiskGrowthFileDetailEntry[]>,
): GrowthDetailNode[] {
  const visited = new Set<string>();

  const build = (parentPath: string, parentDepth: number): GrowthDetailNode[] => {
    const key = normalizePath(parentPath);
    if (visited.has(key)) return [];
    visited.add(key);
    const children = buildDetailChildren(
      parentPath,
      parentDepth,
      dirsByParent.get(key) ?? [],
      filesByParent.get(key) ?? [],
    );
    for (const child of children) {
      if (!child.isDir) continue;
      if (!dirsByParent.has(child.key) && !filesByParent.has(child.key)) continue;
      child.children = build(child.path, child.depth);
    }
    return children;
  };

  return build(rootPath, -1);
}

/** Flatten the tree into visible rows, honouring the expanded set */
export function flattenDetailNodes(
  nodes: GrowthDetailNode[],
  expandedKeys: Set<string>,
): GrowthDetailNode[] {
  const rows: GrowthDetailNode[] = [];
  const walk = (list: GrowthDetailNode[]) => {
    for (const node of list) {
      rows.push(node);
      if (node.isDir && expandedKeys.has(node.key) && node.children.length > 0) walk(node.children);
    }
  };
  walk(nodes);
  return rows;
}
