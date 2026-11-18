// ============================================================================
// 垃圾清理 - 「查看全部文件」弹窗
//
// 中文说明：深度扫描每个分类首屏只给 500 条，卡片里那块 256px 的窗口
// 既翻不动几万个文件，也要用户一页页点「加载更多」。改成点一次
// 「查看全部文件」，把整类结果一次性取回来，在这个弹窗里用完整目录树展示，
// 勾选状态与模块共用一套（关掉弹窗后卡片里的勾选、底部「清理」口径都一致）。
// ============================================================================

import { useCallback, useEffect, useMemo } from 'react';
import { createPortal } from 'react-dom';
import { AnimatePresence, motion } from 'framer-motion';
import { ExternalLink, FolderOpen, Search, Trash2, X } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { FileTreeList } from '../FileTreeList';
import { formatSize } from '../../utils/format';
import { openFile, openInFolder, openRecycleBin } from '../../api/commands';
import type { CategoryScanResult, FileInfo } from '../../types';

interface JunkFileTreeModalProps {
  /** 要展示的分类；null 表示关闭 */
  category: CategoryScanResult | null;
  selectedPaths: Set<string>;
  onToggleFile: (path: string) => void;
  onSetSelectionForPaths: (paths: string[], selected: boolean) => void;
  onSearchFile: (file: FileInfo) => void;
  /** 请求删除：由模块弹统一的确认框，作用范围与底部操作条一致 */
  onRequestDelete: () => void;
  onClose: () => void;
}

export function JunkFileTreeModal({
  category,
  selectedPaths,
  onToggleFile,
  onSetSelectionForPaths,
  onSearchFile,
  onRequestDelete,
  onClose,
}: JunkFileTreeModalProps) {
  const { t } = useTranslation('junkClean');
  const { t: commonT } = useTranslation('common');

  // 系统托管的分类（Windows.old 之类）应用内删不掉，弹窗里也不给勾选
  const isSystemManaged = category?.system_managed === true;
  const isSelectable = useCallback(() => !isSystemManaged, [isSystemManaged]);

  // Esc 关闭：弹窗盖住整页，只留右上角一个叉在几万行的列表里不好找
  useEffect(() => {
    if (!category) return;
    const handleKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [category, onClose]);

  const files = category?.files;
  const { selectedCount, selectedSize } = useMemo(() => {
    if (!files) return { selectedCount: 0, selectedSize: 0 };
    let count = 0;
    let size = 0;
    for (const file of files) {
      if (selectedPaths.has(file.path)) {
        count += 1;
        size += file.size;
      }
    }
    return { selectedCount: count, selectedSize: size };
  }, [files, selectedPaths]);

  const categoryName = category
    ? t(`categories.${category.category}.name`, { defaultValue: category.display_name })
    : '';

  // 打开回收站条目时「打开所在位置」要点回收站而不是文件所在目录
  const handleOpenLocation = useCallback((file: FileInfo) => {
    if (file.category === 'RecycleBin') openRecycleBin();
    else openInFolder(file.path);
  }, []);

  return createPortal(
    <AnimatePresence>
      {category && (
        <motion.div
          className="fixed inset-0 z-[9999] flex items-center justify-center p-4"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.18, ease: 'easeOut' }}
        >
          <div className="absolute inset-0 bg-black/50 backdrop-blur-sm glass-backdrop" onClick={onClose} />
          <motion.div
            className="glass-dialog relative flex max-h-[86vh] w-full max-w-5xl flex-col overflow-hidden rounded-2xl border border-[var(--border-default)] bg-[var(--bg-card)] shadow-2xl"
            initial={{ opacity: 0, scale: 0.96, y: 12 }}
            animate={{ opacity: 1, scale: 1, y: 0 }}
            exit={{ opacity: 0, scale: 0.96, y: 12 }}
            transition={{ duration: 0.2, ease: [0.22, 1, 0.36, 1] }}
            onClick={(event) => event.stopPropagation()}
          >
            <div className="flex shrink-0 items-center justify-between border-b border-[var(--border-default)] px-6 py-4">
              <div className="min-w-0">
                <h3 className="truncate text-lg font-semibold text-[var(--fg-primary)]">
                  {t('viewAllTitle', { name: categoryName })}
                </h3>
                <p className="mt-0.5 text-xs text-[var(--fg-muted)]">
                  {t('viewAllSummary', {
                    count: category.files.length.toLocaleString(),
                    size: formatSize(category.total_size),
                  })}
                </p>
              </div>
              <button onClick={onClose} className="rounded-lg p-2 transition hover:bg-[var(--bg-hover)]">
                <X className="h-5 w-5 text-[var(--fg-muted)]" />
              </button>
            </div>

            <div className="min-h-0 flex-1 px-4 py-3">
              <FileTreeList<FileInfo>
                files={category.files}
                selectedPaths={selectedPaths}
                onSetSelection={onSetSelectionForPaths}
                onToggleFile={(file) => onToggleFile(file.path)}
                isSelectable={isSelectable}
                disabled={isSystemManaged}
                getFileTitle={(file) => file.name}
                // 弹窗是这个列表唯一"用来看"的地方，排序放这里；
                // 卡片里那块 256px 的窄列表塞排序控件反而挤掉路径
                sortable
                height={Math.round(Math.min(560, Math.max(280, window.innerHeight * 0.5)))}
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
                      onClick={(event) => { event.stopPropagation(); handleOpenLocation(file); }}
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
            </div>

            <div className="flex shrink-0 flex-wrap items-center gap-3 border-t border-[var(--border-default)] px-6 py-3">
              <span className="text-xs text-[var(--fg-muted)]">
                {selectedCount > 0
                  ? t('viewAllSelected', { count: selectedCount.toLocaleString(), size: formatSize(selectedSize) })
                  : t('viewAllNone')}
              </span>
              {isSystemManaged && (
                <span className="text-xs text-[var(--color-warning)]">{commonT('systemManagedHint')}</span>
              )}
              <div className="ml-auto flex items-center gap-2">
                <button
                  onClick={onClose}
                  className="rounded-lg px-3 py-1.5 text-xs text-[var(--fg-muted)] transition hover:bg-[var(--bg-hover)]"
                >
                  {commonT('cancel')}
                </button>
                <button
                  onClick={onRequestDelete}
                  disabled={isSystemManaged || selectedCount === 0}
                  className="flex items-center gap-1.5 rounded-lg bg-[var(--color-danger)] px-3 py-1.5 text-xs font-medium text-white transition hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-40"
                >
                  <Trash2 className="h-3.5 w-3.5" />
                  {commonT('deleteSelected')}
                </button>
              </div>
            </div>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>,
    document.body,
  );
}
