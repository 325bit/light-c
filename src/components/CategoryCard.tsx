// ============================================================================
// 分类卡片组件 - 支持主题切换
// 使用虚拟列表优化大量文件的渲染性能
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

// 微信风格风险等级样式配置
const getRiskBadgeStyle = (level: number) => {
  switch (level) {
    case 1:
      return 'bg-[var(--brand-green-10)] text-[var(--brand-green)] border-[var(--brand-green-20)]';  // 微信绿 - 安全
    case 2:
      return 'bg-[var(--brand-green-10)] text-[var(--brand-green)] border-[var(--brand-green-20)]';  // 微信绿 - 低风险
    case 3:
      return 'bg-[var(--color-warning)]/10 text-[var(--color-warning)] border-[var(--color-warning)]/20';  // 柔和橙 - 中等
    case 4:
      return 'bg-[var(--color-warning)]/10 text-[var(--color-warning)] border-[var(--color-warning)]/20';  // 柔和橙 - 较高
    default:
      return 'bg-[var(--color-danger)]/10 text-[var(--color-danger)] border-[var(--color-danger)]/20';  // 柔和红 - 高风险
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
  /** 目录级批量勾选：一次提交一批路径（勾文件夹就靠它，不必逐条 toggle） */
  onSetSelectionForPaths: (paths: string[], selected: boolean) => void;
  onSearchFile: (file: FileInfo) => void;
  /** 深扫分类只取了第一页：给出「查看全部文件」入口，点开在弹窗里看完整目录树 */
  hasMore?: boolean;
  onViewAllFiles?: () => void;
  isLoadingAll?: boolean;
}

/**
 * 分类卡片组件 - 桌面应用风格
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

  // 使用useMemo缓存计算结果，避免重复计算
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

  // 由系统托管的分类（旧版 Windows 安装）：应用内删不掉，也不该由应用删 ——
  // 删了用户就失去回退旧版本的能力。这里统一拦掉勾选，只保留
  // 「看占用大小 + 引导到系统磁盘清理」两条路径。
  const isSystemManaged = category.system_managed === true;

  // 稳定引用：FileTreeList 内部按它判断"哪些文件可勾选"，
  // 每次渲染换一个新函数会让整棵树的可勾选集合重算一遍。
  const isSelectable = useCallback(() => !isSystemManaged, [isSystemManaged]);

  const handleCategoryToggle = useCallback(() => {
    if (isSystemManaged) return;
    onToggleCategory(category.display_name, category.files, !isAllSelected);
  }, [category.display_name, category.files, isAllSelected, onToggleCategory, isSystemManaged]);

  const handleExpand = useCallback(() => {
    setExpanded(prev => !prev);
  }, []);

  /**
   * 目录树勾选。
   *
   * 中文说明：勾中的是「整类全部文件」时必须走原来的分类选择逻辑 —— 深扫模式下
   * 它还会记住"整类已选"，删除时连未加载的分页一起删；勾子目录只动对应那批路径。
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
      {/* 分类头部 - 增加内边距 */}
      <div
        className="px-5 py-4 cursor-pointer hover:bg-[var(--bg-hover)] transition-colors select-none"
        onClick={handleExpand}
      >
        {/* 多语言分类名称长度不同，允许窄窗口换行，避免风险标签和统计信息互相挤压。 */}
        <div className="flex flex-wrap items-center gap-3 sm:flex-nowrap sm:gap-4">
          {/* 展开图标 */}
          <div className="text-[var(--text-faint)] transition-transform duration-200" style={{ transform: expanded ? 'rotate(0deg)' : 'rotate(-90deg)' }}>
            <ChevronDown className="w-5 h-5" />
          </div>

          {/* 复选框 - 微信绿；系统托管的分类不提供勾选，显示为禁用态 */}
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

          {/* 分类图标 - 微信绿 10% 透明度圆角容器 */}
          <div className="w-10 h-10 rounded-xl bg-[var(--brand-green-10)] flex items-center justify-center text-[var(--brand-green)]">
            <Folder className="w-5 h-5" />
          </div>

          {/* 分类信息 */}
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

          {/* 统计信息 - tabular-nums 确保数字稳定 */}
          <div className="ml-auto shrink-0 text-right">
            <p className="text-[1.07rem] font-bold text-[var(--text-primary)] tabular-nums">
              {formatSize(category.total_size)}
            </p>
            <p className="text-[0.93rem] text-[var(--text-muted)] tabular-nums">
              {commonT('fileCount', { count: category.file_count.toLocaleString() })}
              {selectedCount > 0 && (
                <span className="text-[var(--brand-green)] ml-1">
                  {/* 深度分类分页时提示完整删除口径，避免把当前页大小误认为分类总量。 */}
                  {hasMore && selectedCount === category.files.length
                    ? commonT('selectedPage', { count: selectedCount.toLocaleString() })
                    : commonT('selectedFiles', { count: selectedCount.toLocaleString(), size: formatSize(selectedSize) })}
                </span>
              )}
            </p>
          </div>
        </div>
      </div>

      {/* 文件列表 - 虚拟滚动。用高度动画包住列表，展开/折叠时不会再硬切。 */}
      <AnimatePresence initial={false}>
        {expanded && (
          <motion.div
            className="overflow-hidden border-t border-[var(--border-color)]"
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: 'auto', opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.26, ease: [0.22, 1, 0.36, 1] }}
          >
            {/* 系统托管的分类：不给删除入口，改为引导系统磁盘清理。
                这类目录应用内删不掉（权限/占用），而且删掉会让用户失去回退旧版本的能力，
                所以规范做法是交给 Windows 自带的磁盘清理。 */}
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

            {/* 目录树：按路径折叠成文件管理器那样的层级，
                勾目录 = 批量选中该目录下所有文件（逐个勾太麻烦）。
                高度沿用原来的 256px，内部自带虚拟滚动。 */}
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
            {/* 不再让用户一页页点「加载更多」：统一成一个「查看全部文件」入口，
                完整目录树放到弹窗里看 —— 卡片内这块 256px 的窗口本来就装不下
                几千上万个文件，深扫的首屏 500 条也一样翻不动。 */}
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
