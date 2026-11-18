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
  /** 修改时间（Unix 秒）。没有这个字段的数据源按 0 处理，排序时统一排在最后 */
  modified_time?: number;
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
  /** 后代文件里最新的修改时间（Unix 秒）；"按时间排序"用它给目录排位 */
  latestModified: number;
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
  /** 修改时间（Unix 秒）；数据源没给就是 0 */
  modifiedTime: number;
  file: T;
}

export type FileTreeNode<T extends TreeSourceFile> =
  | FileTreeFolder<T>
  | FileTreeFile<T>;

/** 排序依据：名称（自然序）/ 大小 / 修改时间 */
export type TreeSortKey = 'name' | 'size' | 'time';

export interface TreeSort {
  key: TreeSortKey;
  /** true = 由大到小、由新到旧；名称排序时 true = 逆序 */
  desc: boolean;
}

/** 各排序键的默认方向：大小和时间默认"最大的/最新的先看" */
export const DEFAULT_SORT_DESC: Record<TreeSortKey, boolean> = {
  name: false,
  size: true,
  time: true,
};

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
  let latest = 0;
  for (const child of node.children) {
    if (child.isDir) {
      aggregate(child);
      // 逐个 push 而不是展开传参：几万个后代路径用 spread 会顶到参数个数上限
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
 * 由扁平文件列表建树。
 *
 * `getPath` 默认取 `file.path`；社交清理等模块的文件对象结构不同，可自行指定。
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
    latestModified: 0,
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
    // 全都是「根目录下的文件」时树没有意义，调用方直接平铺渲染
    hasBranches: root.children.some((child) => child.isDir),
    fileCount: root.fileCount,
    totalSize: root.totalSize,
  };
}

/**
 * 按指定依据重排整棵树。
 *
 * 中文说明：不重新建树 —— 路径拆分、目录归并、体积聚合都与顺序无关，
 * 重排只需把每一层的子节点数组按新依据排序（节点对象复用，只重建目录壳）。
 *
 * 两条口径：
 * - 目录始终排在文件前面，和资源管理器一致；排序方向只作用于同类之间。
 * - 目录的「大小」取后代总和、「时间」取后代里最新的一个 ——
 *   否则装了八万文件的目录永远排不到前面，按大小排序就失去意义。
 */
export function sortTree<T extends TreeSourceFile>(
  tree: FileTree<T>,
  sort: TreeSort,
): FileTree<T> {
  const { key, desc } = sort;
  // 名称升序就是建树时的默认顺序，不必再排一遍
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
 * 扁平列表排序（不建树的「全部文件」弹窗用）。
 *
 * 中文说明：AI 缓存明细这类地方拿到的就是一堆文件，没有目录层级可折，
 * 排序口径要和目录树弹窗完全一致，否则同一个「按大小」在两个窗口里含义不同。
 * 三条口径同样照搬：名称自然序、大小按字节、时间按 Unix 秒；
 * 时间缺失（0）的一律排在最后，不随升降序翻到前面去。
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
    // 没有时间戳的数据无论升序降序都垫底：把它们当成"最旧"会在降序时冒到最前面
    if (leftTime === 0) return 1;
    if (rightTime === 0) return -1;
    return desc ? rightTime - leftTime : leftTime - rightTime;
  });
  return next;
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
