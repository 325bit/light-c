// ============================================================================
// 文件目录树
// 把扫描结果里的扁平文件列表按路径折叠成目录树
//
// 中文说明：扫描结果原本是一行一个文件，用户想"把这个文件夹里的都删掉"只能一个个点。
// 这里按路径的公共前缀建树，勾选目录就能一次性选中整棵子树。
// ============================================================================

import { stripWindowsDevicePrefix } from './searchEngine';

/** 建树只需要「路径 + 大小」，其余字段原样带在叶子上 */
export interface TreeSourceFile {
  path: string;
  size: number;
}

export interface FileTreeFolder<T extends TreeSourceFile> {
  /** 稳定 key：目录用 `d|完整路径`，文件用 `f|完整路径` */
  key: string;
  /** 展示名（路径最后一段） */
  name: string;
  /** 完整路径（目录不带结尾反斜杠） */
  path: string;
  /** 相对树根的层级，0 = 第一层 */
  depth: number;
  isDir: true;
  /** 后代文件数 */
  fileCount: number;
  /** 后代文件总大小 */
  totalSize: number;
  /**
   * 全部后代文件的路径。
   *
   * 中文说明：勾选目录要靠它一次性提交整批路径 —— 逐条 toggle 会触发几十次
   * setState，几百个文件时肉眼可见地卡。建树时一次算好，勾选时直接批量提交。
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
  file: T;
}

export type FileTreeNode<T extends TreeSourceFile> =
  | FileTreeFolder<T>
  | FileTreeFile<T>;

export interface FileTree<T extends TreeSourceFile> {
  /** 所有文件的公共祖先目录（展示成树头，例如 C:\Users\x\AppData\Local\Temp） */
  rootPath: string;
  /** 根下的第一层节点 */
  nodes: FileTreeNode<T>[];
  /** 是否有真正的层级结构；全在同一层时没必要画树 */
  hasBranches: boolean;
  fileCount: number;
  totalSize: number;
}

/** 统一分隔符与设备前缀，保证比较时同一路径只有一种写法 */
function normalizePath(value: string): string {
  // stripWindowsDevicePrefix 内部是两次 split+join，几万个路径时开销明显；
  // 设备前缀只出现在少数扫描结果里，先判开头，绝大多数路径直接跳过。
  let path = value.startsWith('\\\\') || value.startsWith('\\??\\')
    ? stripWindowsDevicePrefix(value)
    : value;
  if (path.indexOf('/') !== -1) path = path.replace(/\//g, '\\');
  if (path.endsWith('\\')) path = path.replace(/\\+$/, '');
  return path;
}

/** 段比较：大小写不敏感（Windows 路径不区分大小写），先走等值快路径避免无谓的 toLowerCase */
function sameSegment(left: string, right: string): boolean {
  return left === right || left.toLowerCase() === right.toLowerCase();
}

function joinPath(parent: string, name: string): string {
  return parent ? `${parent}\\${name}` : name;
}

/** 对比器：目录在前、同类按名称自然序（2 排在 10 前面） */
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

/** 自底向上聚合目录的文件数与体积，同时收集后代文件路径 */
function aggregate<T extends TreeSourceFile>(node: FileTreeFolder<T>): void {
  if (node.children.length === 0) {
    // 空目录不会出现在扫描结果里，但保留兜底避免除零或空数组异常
    node.paths = [];
    return;
  }
  const paths: string[] = [];
  let size = 0;
  for (const child of node.children) {
    if (child.isDir) {
      aggregate(child);
      // 逐个 push 而不是展开传参：几万个后代路径用 spread 会顶到参数个数上限
      for (const path of child.paths) paths.push(path);
      size += child.totalSize;
    } else {
      paths.push(child.path);
      size += child.file.size;
    }
  }
  node.paths = paths;
  node.fileCount = paths.length;
  node.totalSize = size;
}

/**
 * 由扁平文件列表建树。
 *
 * `getPath` 默认取 `file.path`；社交清理等模块的文件对象结构不同，可自行指定。
 */
export function buildFileTree<T extends TreeSourceFile>(
  files: T[],
  getPath: (file: T) => string = (file) => file.path,
): FileTree<T> {
  const total = files.length;
  const paths = new Array<string>(total);
  const segmentLists = new Array<string[]>(total);
  for (let index = 0; index < total; index += 1) {
    const path = normalizePath(getPath(files[index]));
    paths[index] = path;
    segmentLists[index] = path.split('\\').filter(Boolean);
  }

  // 公共祖先目录：只保留「目录段」，逐个文件收窄前缀长度。
  // 这里用下标而不是每次 slice 数组，34k 个文件时能省掉几万次临时数组分配。
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
    paths: [],
    children: [],
  };

  /*
    子目录索引：父节点 → 小写目录名 → 子节点。

    中文说明：原来每个层级都用 children.find 线性扫，一个父目录下有上千个子目录时
    退化成平方级（34k 个文件要 200ms 以上）。改成 Map 之后每层都是 O(1)。
  */
  const childIndex = new Map<FileTreeFolder<T>, Map<string, FileTreeFolder<T>>>();

  for (let index = 0; index < total; index += 1) {
    const segments = segmentLists[index];
    // 文件名之后的段就是目录层级；路径本身正好等于根目录时没有文件名，跳过
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
      file: files[index],
    });
  }

  aggregate(root);
  sortNodes(root.children);

  return {
    rootPath,
    nodes: root.children,
    // 全都是「根目录下的文件」时树没有意义，调用方直接平铺渲染
    hasBranches: root.children.some((child) => child.isDir),
    fileCount: root.fileCount,
    totalSize: root.totalSize,
  };
}

/**
 * 把树摊平成可见行（已折叠目录的后代不出现），供虚拟列表消费。
 *
 * 展开状态由调用方持有：目录 key 稳定（就是完整路径），重新扫描后同一批目录
 * 仍然保持展开，用户不用每次重新点开。
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

/** 收集所有目录节点的 key，供「全部展开」使用 */
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
 * 一次性算出每个目录下已选中的文件数，用于三态复选框。
 *
 * 中文说明：不能对每个目录都遍历一遍后代路径（行数 × 文件数会退化成平方级），
 * 这里只走一遍树，把叶子的选中状态向上汇总。
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
