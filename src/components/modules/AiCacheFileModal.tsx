/**
 * 缓存目录的明细弹窗。
 *
 * 为什么单独做一个而不是复用社交专清那个：两者的数据形状不同（那边是带风险等级的
 * 社交文件，这边只是一个路径 + 大小），硬塞进去要给它加一堆可选字段。
 *
 * 为什么用虚拟滚动：单个缓存目录可能有上万个文件（WorkBuddy 的 logs 就上千），
 * 一次性渲染会让弹窗卡死。这里按行虚拟化，只渲染视口内的十几行。
 */
import { useEffect, useMemo, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';
import { AnimatePresence, motion } from 'framer-motion';
import { useVirtualizer } from '@tanstack/react-virtual';
import { FolderOpen, X } from 'lucide-react';
import { listAiCacheFiles } from '../../api/aiToolCleanup';
import { openInFolder } from '../../api/commands';
import type { AiCacheFileList } from '../../types/aiToolCleanup';
import { FileSortControl } from '../FileSortControl';
import { sortFileList, type TreeSort } from '../../utils/fileTree';
import { formatDate, formatSize } from '../../utils/format';

interface AiCacheFileModalProps {
  isOpen: boolean;
  /** 要展开的缓存目录，null 表示关闭 */
  dirPath: string | null;
  dirName: string;
  onClose: () => void;
}

export function AiCacheFileModal({ isOpen, dirPath, dirName, onClose }: AiCacheFileModalProps) {
  const { t } = useTranslation('modules');
  // 后端可能因为目录过大而截断（`truncated`），截断时必须告诉用户，
  // 否则"文件数 × 大小"和目录实际占用对不上，会被当成统计错误。
  const [listing, setListing] = useState<AiCacheFileList>({ files: [], truncated: false });
  const files = listing.files;
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');
  const [sort, setSort] = useState<TreeSort>({ key: 'name', desc: false });
  const scrollRef = useRef<HTMLDivElement>(null);

  /*
    排序口径与「查看全部文件」目录树弹窗完全一致：名称自然序、大小按字节、
    时间按 Unix 秒；没有时间戳的条目无论升降序都垫底，不会冒充"最新"。
    这里没有目录层级，所以直接排数组，不用建树。
  */
  const sortedFiles = useMemo(
    () => sortFileList(files, sort, {
      // 按名称排的是文件名，不是整条路径：同一目录下文件名的前缀都一样，
      // 拿全路径去比等于没排。取最后一段。
      getName: (file) => {
        const separator = Math.max(file.path.lastIndexOf('\\'), file.path.lastIndexOf('/'));
        return separator >= 0 ? file.path.slice(separator + 1) : file.path;
      },
      getSize: (file) => file.size,
      getTime: (file) => file.modifiedTime ?? 0,
    }),
    [files, sort],
  );

  useEffect(() => {
    if (!isOpen || !dirPath) {
      setListing({ files: [], truncated: false });
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError('');
    listAiCacheFiles(dirPath)
      .then((next) => {
        if (!cancelled) setListing(next);
      })
      .catch((loadError) => {
        if (!cancelled) setError(String(loadError));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    // 弹窗关闭或换目录时丢弃在途结果，避免旧数据覆盖新目录
    return () => {
      cancelled = true;
    };
  }, [isOpen, dirPath]);

  const virtualizer = useVirtualizer({
    count: sortedFiles.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 34,
    overscan: 8,
  });

  /**
   * 数据到位后强制重测一次。
   *
   * 弹窗刚挂载时面板还在做入场缩放，滚动容器测出来的高度是 0，
   * virtualizer 按 0 高度算可见区间 → 第一帧一条都不渲染，表现为"打开是空的、
   * 滚一下才出来"。等文件列表变了重新测量即可。
   */
  useEffect(() => {
    virtualizer.measure();
    // 只依赖排序后的列表：virtualizer 每次渲染都是新对象，放进依赖会无限重渲染
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sortedFiles]);

  const totalSize = files.reduce((sum, file) => sum + file.size, 0);

  /**
   * ⚠️ 必须 portal 到 body。
   * 模块容器带 `transform`（入场动画），`position: fixed` 会退化成 `absolute` ——
   * 后果是遮罩只盖住模块区域、弹窗也不在视口中心。社交专清的明细弹窗同样这么做。
   */
  return createPortal(
    <AnimatePresence>
      {isOpen && (
    <motion.div
      className="fixed inset-0 z-[9999] flex items-center justify-center p-4"
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      transition={{ duration: 0.18, ease: 'easeOut' }}
    >
      <motion.div className="absolute inset-0 bg-black/50 backdrop-blur-sm glass-backdrop" onClick={onClose} />
      <motion.div
        className="ai-cache-modal glass-dialog relative flex max-h-[80vh] w-full max-w-5xl flex-col overflow-hidden rounded-2xl border border-[var(--border-color)] bg-[var(--bg-elevated)] shadow-2xl"
        initial={{ opacity: 0, scale: 0.96, y: 12 }}
        animate={{ opacity: 1, scale: 1, y: 0 }}
        exit={{ opacity: 0, scale: 0.96, y: 12 }}
        transition={{ duration: 0.2, ease: [0.22, 1, 0.36, 1] }}
      >
        <div className="flex shrink-0 items-start justify-between border-b border-[var(--border-color)] px-6 py-4">
          <div className="min-w-0">
            <h3 className="truncate text-lg font-semibold text-[var(--text-primary)]">{dirName}</h3>
            <p className="mt-0.5 text-xs text-[var(--text-muted)]">
              {t('aiTools.fileSummary', {
                count: files.length.toLocaleString(),
                size: formatSize(totalSize),
              })}
              {listing.truncated && (
                <span className="ml-2 text-[var(--text-faint)]">
                  {t('aiTools.filesTruncated', { count: files.length.toLocaleString() })}
                </span>
              )}
            </p>
          </div>
          <div className="flex shrink-0 items-center gap-2">
            {/* 排序控件与其他「查看全部文件」弹窗同一套：名称 / 大小 / 修改时间，
                再点同一个键切换升降序。列头不单独做可点击排序，避免两处口径打架 */}
            <FileSortControl sort={sort} onSortChange={setSort} />
            <button
              type="button"
              onClick={onClose}
              className="p-2 rounded-lg hover:bg-[var(--bg-hover)] transition-colors"
              aria-label={t('common:close')}
            >
              <X className="h-5 w-5 text-[var(--text-muted)]" />
            </button>
          </div>
        </div>

        {/* 表头行：列宽与数据行对齐，序号列居中 */}
        <div className="flex shrink-0 items-center gap-4 border-b border-[var(--border-color)] bg-[var(--bg-hover)] px-6 py-2 text-xs font-medium text-[var(--text-muted)]">
          <span className="w-10 shrink-0 text-center">#</span>
          <span className="flex-1">{t('aiTools.columnPath')}</span>
          {/* 按时间排序时才出现时间列：平时不占宽度，排序时才有依据可看 */}
          {sort.key === 'time' && (
            <span className="w-24 shrink-0 text-right">{t('aiTools.columnTime')}</span>
          )}
          <span className="w-24 shrink-0 text-right">{t('aiTools.columnSize')}</span>
          {/* 占位：与数据行的「打开文件夹」按钮对齐 */}
          <span className="w-5 shrink-0" />
        </div>

        <div ref={scrollRef} className="min-h-0 flex-1 overflow-auto">
          {loading ? (
            <div className="flex h-32 items-center justify-center text-sm text-[var(--text-muted)]">
              {t('aiTools.loadingFiles')}
            </div>
          ) : error ? (
            <div className="flex h-32 items-center justify-center px-6 text-center text-sm text-[var(--color-danger)]">
              {error}
            </div>
          ) : sortedFiles.length === 0 ? (
            <div className="flex h-32 items-center justify-center text-sm text-[var(--text-muted)]">
              {t('aiTools.noFiles')}
            </div>
          ) : (
            <div style={{ height: `${virtualizer.getTotalSize()}px`, position: 'relative' }}>
              {virtualizer.getVirtualItems().map((virtualRow) => {
                const file = sortedFiles[virtualRow.index];
                return (
                  <div
                    key={virtualRow.key}
                    className="absolute left-0 top-0 flex w-full items-center gap-4 px-6 py-2 text-xs transition-colors hover:bg-[var(--bg-hover)]"
                    style={{
                      height: `${virtualRow.size}px`,
                      transform: `translateY(${virtualRow.start}px)`,
                    }}
                  >
                    <span className="w-10 shrink-0 text-center tabular-nums text-[var(--text-faint)]">
                      {virtualRow.index + 1}
                    </span>
                    <span
                      className="min-w-0 flex-1 truncate text-[var(--text-secondary)]"
                      title={file.path}
                    >
                      {file.path}
                    </span>
                    {sort.key === 'time' && (
                      <span className="w-24 shrink-0 text-right tabular-nums text-[var(--text-faint)]">
                        {file.modifiedTime > 0 ? formatDate(file.modifiedTime) : '—'}
                      </span>
                    )}
                    <span className="w-24 shrink-0 text-right tabular-nums text-[var(--text-muted)]">
                      {formatSize(file.size)}
                    </span>
                    <button
                      type="button"
                      onClick={() => openInFolder(file.path)}
                      title={t('aiTools.openFolder')}
                      className="shrink-0 rounded p-0.5 text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--brand-green)]"
                    >
                      <FolderOpen className="h-4 w-4" />
                    </button>
                  </div>
                );
              })}
            </div>
          )}
        </div>

        {/* 底部信息条：左侧总数，右侧收口按钮 */}
        <div className="flex shrink-0 items-center justify-between border-t border-[var(--border-color)] px-6 py-3 text-xs text-[var(--text-muted)]">
          <span>
            {t('aiTools.fileSummary', {
              count: files.length.toLocaleString(),
              size: formatSize(totalSize),
            })}
          </span>
          <button
            type="button"
            onClick={onClose}
            className="rounded-lg bg-[var(--brand-green)] px-4 py-1.5 text-xs font-medium text-white transition-opacity hover:opacity-90"
          >
            {t('common:close')}
          </button>
        </div>
      </motion.div>
    </motion.div>
      )}
    </AnimatePresence>,
    document.body,
  );
}
