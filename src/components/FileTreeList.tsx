// ============================================================================
// 文件目录树列表
// 扫描结果按目录折叠展示，勾选目录即可批量选中整棵子树
//
// 中文说明：原来是「一行一个文件」的平铺列表，想清掉某个缓存目录只能逐个勾选。
// 这里做成文件管理器那样可以展开/折叠的树，目录行带三态复选框，
// 勾一下就把该目录下所有文件一起选中。
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
  type FileTreeNode,
  type TreeSourceFile,
} from '../utils/fileTree';
import { formatSize } from '../utils/format';

export interface FileTreeListProps<T extends TreeSourceFile> {
  /** 当前这一组文件（一个分类，或筛选后的结果） */
  files: T[];
  /** 已勾选的完整路径集合（模块级，可能包含其它分类的文件） */
  selectedPaths: Set<string>;
  /** 整批设置选中状态：目录勾选一次性提交，避免逐条 setState */
  onSetSelection: (paths: string[], selected: boolean) => void;
  /** 单个文件勾选：保留模块自己的副作用（例如反向同步整类选择标记） */
  onToggleFile: (file: T) => void;
  /** 该文件是否可勾选；不可勾选时显示为禁用态（社交清理的聊天记录就是这样） */
  isSelectable?: (file: T) => boolean;
  /** 行内附加信息：风险标签、来源应用等，渲染在文件名与大小之间 */
  renderFileMeta?: (file: T) => ReactNode;
  /** 行尾操作按钮：打开所在文件夹、搜索建议等 */
  renderFileActions?: (file: T) => ReactNode;
  /** 文件行的显示名；默认取树的最后一段 */
  getFileTitle?: (file: T) => string;
  /** 滚动区高度（px）。整块列表始终虚拟化，行数再多也不会卡 */
  height?: number;
  rowHeight?: number;
  /** 树头右侧的额外说明，例如「已加载 500 / 共有 3 万个」 */
  toolbarExtra?: ReactNode;
  /** 整片不可勾选（例如由系统托管的分类）：只剩浏览与展开，没有任何勾选入口 */
  disabled?: boolean;
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
  className,
}: FileTreeListProps<T>) {
  const { t } = useTranslation('common');
  const scrollRef = useRef<HTMLDivElement>(null);

  /*
    展开策略：小列表直接全展开，大列表默认折叠。

    中文说明：全部折叠时，只有三五个文件的分类反而要多点一次才看得到内容；
    全部展开时，几百上千个文件又会把列表撑成一面墙、目录结构反而糊掉。
    所以按文件数分流：≤ 30 个直接平铺开，更多的先只给目录层。
    用户手动展开/折叠过的状态不会被覆盖（只在跨过阈值、换了整批数据时才重置）。
  */
  const [expandedKeys, setExpandedKeys] = useState<Set<string>>(new Set());
  const tree = useMemo(() => buildFileTree(files), [files]);
  const autoExpandMode = tree.fileCount <= 30 ? 'expand' : 'collapse';
  const autoExpandRef = useRef<string | null>(null);
  useEffect(() => {
    if (autoExpandRef.current === autoExpandMode) return;
    autoExpandRef.current = autoExpandMode;
    setExpandedKeys(autoExpandMode === 'expand' ? new Set(collectFolderKeys(tree.nodes)) : new Set());
  }, [autoExpandMode, tree]);

  const rows = useMemo(() => {
    // 没有真正的层级（所有文件都在同一层）时不摆树，避免多出一层无意义的包裹
    if (!tree.hasBranches) return tree.nodes;
    return flattenFileTree(tree.nodes, expandedKeys);
  }, [tree, expandedKeys]);

  const selectedCounts = useMemo(
    () => computeSelectedCounts(tree.nodes, selectedPaths),
    [tree, selectedPaths],
  );

  // 只有可勾选的文件参与「全选 / 整目录勾选」。社交清理的聊天记录数据库
  // 是不可删的，把它塞进选中集合会让删除流程去尝试动它。
  //
  // 中文说明：这里只保留一份「可勾选路径集合」+ 每个目录的可勾选数量，
  // 不再给每个目录各存一份后代路径数组 —— 后者在几万文件时要复制几十万条引用，
  // 而且只要父组件传进来的判断函数换了引用就会整体重算一遍。
  const selectableInfo = useMemo(() => {
    if (disabled) {
      return { set: new Set<string>(), counts: new Map<string, number>(), paths: [] as string[] };
    }
    if (!isSelectable) {
      // 不传判断函数 = 全部可勾选，省掉整轮过滤
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

  /** 某个目录下所有可勾选的路径。只有真正点复选框时才算，不在渲染期展开。 */
  const selectablePathsOf = useCallback(
    (node: FileTreeNode<T>): string[] => {
      if (node.isDir === false) return [];
      if (!selectableInfo.set) return node.paths;
      return node.paths.filter((path) => selectableInfo.set!.has(path));
    },
    [selectableInfo],
  );

  // 整组可勾选的文件：树头复选框与「全选」用这一份，而不是全部文件
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
      // 目录下没有任何可勾选文件时（例如整类都是聊天记录），复选框置灰
      const selectableCount = selectableInfo.counts
        ? (selectableInfo.counts.get(node.key) ?? 0)
        : node.fileCount;
      const blocked = disabled || selectableCount === 0;
      // 与可勾选数量比较（而不是全部文件数）：目录里混着不可删的文件时，
      // 用文件总数比永远凑不满，复选框会一直卡在"部分选中"的半选态。
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
        {/* 目录行有折叠箭头占位，文件行也留同样的宽度，保证同层复选框对齐 */}
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
        <span className={`shrink-0 text-[0.86rem] tabular-nums ${selectable ? 'text-[var(--text-muted)]' : 'text-[var(--text-faint)]'}`}>
          {formatSize(node.file.size)}
        </span>
        {renderFileActions?.(node.file)}
      </div>
    );
  };

  return (
    <div className={className}>
      {/* 树头：公共父目录 + 总览 + 整组勾选 + 展开/折叠 */}
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

/** 三态复选框：全选 / 部分选中（横杠）/ 未选 / 禁用 */
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
