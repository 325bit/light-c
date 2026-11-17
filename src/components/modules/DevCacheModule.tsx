/**
 * 开发空间清理模块。
 *
 * 这个模块是 cscan（命令行扫描器）在 light-c 里的对应物，补的是原有模块**都不管**的那一块：
 * 开发工具链缓存（NuGet / Cargo / Gradle / Go / JetBrains…）和同一软件留下的多套旧版本目录。
 *
 * 为什么分两个页签：
 * 「工具缓存」是"一堆散落的缓存目录，按工具归组"，「多版本残留」是"同一路径下并排躺着 4 个
 * 微软 Edge 版本，只有一个是正在用的"。两者的信息结构不同（后者必须标出"当前在用哪一版"），
 * 硬塞进同一棵树会让用户看不出差别在哪。
 *
 * 为什么不做"一键全清"：模拟器镜像、工作区状态、本地模型这些删了就没了，
 * 用户必须先看到"删的是哪个目录、会丢什么"再确认，后端还会再校验一次路径归属。
 */
import { useCallback, useMemo, useState } from 'react';
import { AnimatePresence, motion } from 'framer-motion';
import { useTranslation } from 'react-i18next';
import {
  ChevronRight,
  FolderOpen,
  HardDrive,
  Layers,
  Package,
  RefreshCw,
  Trash2,
} from 'lucide-react';
import { ModuleCard } from '../ModuleCard';
import { EmptyState } from '../EmptyState';
import { ConfirmDialog } from '../ConfirmDialog';
import { ModuleOperationToolbar } from '../ModuleOperationToolbar';
import { LinkBadge } from '../LinkBadge';
import { ModuleDeleteProgress } from '../ModuleDeleteProgress';
import { ModuleScanProgress } from '../ModuleScanProgress';
import { deleteDevCachePaths, scanDevCache } from '../../api/devCache';
import { openInFolder } from '../../api/commands';
import type { DevCacheScanResult, DevRisk } from '../../types/devCache';
import { formatSize } from '../../utils/format';
import { useToast } from '../Toast';
import { useAiModuleBridge } from '../ai/useAiModuleBridge';
import type { ModuleRenderProps } from './moduleProps';

/** 页签：工具缓存 / 多版本残留 */
type ViewTab = 'cache' | 'version';

/**
 * 勾选框。三处复用（分组、缓存目录、版本目录），抽出来免得三份实现慢慢长歪。
 * `partial` 只给分组级用：组内选了一部分时画一条横杠，而不是空框。
 */
function Checkbox({ checked, partial = false }: { checked: boolean; partial?: boolean }) {
  return (
    <div
      className={`flex h-5 w-5 shrink-0 items-center justify-center rounded border-2 transition-colors ${
        checked
          ? 'border-[var(--brand-green)] bg-[var(--brand-green)]'
          : partial
            ? 'border-[var(--brand-green)] bg-[var(--brand-green)]/50'
            : 'border-[var(--text-faint)]'
      }`}
    >
      {(checked || partial) && (
        <svg
          className="h-3 w-3 text-white"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
          strokeWidth={3}
        >
          {checked ? (
            <path strokeLinecap="round" strokeLinejoin="round" d="M5 13l4 4L19 7" />
          ) : (
            <path strokeLinecap="round" d="M5 12h14" />
          )}
        </svg>
      )}
    </div>
  );
}

/** 风险徽标：cache 不标（占多数，标了反而吵），confirm 黄色，readonly 灰色 */
function RiskTag({ risk, label }: { risk: DevRisk; label: string }) {
  if (risk === 'cache') return null;
  const tone =
    risk === 'confirm'
      ? 'bg-[var(--warning)]/15 text-[var(--warning)]'
      : 'bg-[var(--bg-hover)] text-[var(--text-muted)]';
  return (
    <span className={`shrink-0 rounded px-1.5 py-0.5 text-[0.72rem] font-medium ${tone}`}>
      {label}
    </span>
  );
}

export function DevCacheModule({ isPageActive = true }: ModuleRenderProps) {
  const { t: navT } = useTranslation('nav');
  const { t: moduleT } = useTranslation('modules');
  const { showToast } = useToast();

  const [scanning, setScanning] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [result, setResult] = useState<DevCacheScanResult | null>(null);
  /**
   * 刚把可清理项删光了。
   *
   * 和"从没扫到过"是两回事：前者是完成态，后者是"可能没装 / 结构不符"，
   * 刚清完还提示"这些工具可能没装"会让人以为扫描坏了。
   */
  const [justCleared, setJustCleared] = useState(false);
  const [error, setError] = useState('');
  const [tab, setTab] = useState<ViewTab>('cache');
  /** 当前展开的卡片（手风琴：同时只展开一个）。缓存组与版本组共用一个键空间，靠前缀区分 */
  const [expanded, setExpanded] = useState<string | null>(null);
  /** 已勾选的目录路径（两个页签共用一份，删的时候一起删） */
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [confirmOpen, setConfirmOpen] = useState(false);

  /** 路径 → 体积。勾选合计要跨两个页签算，单独查表比每次遍历省事 */
  const sizeByPath = useMemo(() => {
    const map = new Map<string, number>();
    if (!result) return map;
    result.groups.forEach((group) => group.entries.forEach((entry) => map.set(entry.path, entry.size)));
    result.versionGroups.forEach((group) =>
      group.entries.forEach((entry) => map.set(entry.path, entry.size)),
    );
    return map;
  }, [result]);

  const selectedSize = useMemo(() => {
    let total = 0;
    selected.forEach((path) => {
      total += sizeByPath.get(path) ?? 0;
    });
    return total;
  }, [selected, sizeByPath]);

  /** 当前页签里"可以勾"的路径（只读项与当前版本不算），供全选按钮用 */
  const selectablePaths = useMemo(() => {
    if (!result) return [] as string[];
    if (tab === 'cache') {
      return result.groups.flatMap((group) =>
        group.entries.filter((entry) => entry.risk !== 'readonly').map((entry) => entry.path),
      );
    }
    return result.versionGroups.flatMap((group) =>
      group.entries.filter((entry) => !entry.isCurrent).map((entry) => entry.path),
    );
  }, [result, tab]);

  /** 有没有东西可显示：两个页签都空才算空 */
  const hasAnything = useMemo(() => {
    if (!result) return false;
    return result.groups.length > 0 || result.versionGroups.length > 0;
  }, [result]);

  /**
   * 把结果摊平成「一行一项」的文本，给 AI 助手的只读工具用。
   *
   * 顺序沿用界面：先缓存组、再多版本组，组内按体积倒序 ——
   * 助手读到的第一条就是最占空间的那块，和用户屏幕上看到的顺序一致。
   */
  const flattenEntries = useCallback(() => {
    if (!result) return [];
    const rows: { groupName: string; path: string; label: string; size: number; extra: string }[] =
      [];
    result.groups.forEach((group) => {
      group.entries.forEach((entry) => {
        rows.push({
          groupName: group.groupName,
          path: entry.path,
          label: entry.name,
          size: entry.size,
          extra:
            `${group.groupName} · ${entry.fileCount} 个文件 · ${entry.risk}` +
            // 搬迁过的目录要带上实体位置，否则助手会照 C 盘路径解释占用
            (entry.link ? ` · 链接到 ${entry.link.resolved}` : ''),
        });
      });
    });
    result.versionGroups.forEach((group) => {
      group.entries.forEach((entry) => {
        rows.push({
          groupName: group.name,
          path: entry.path,
          label: `v${entry.version}`,
          size: entry.size,
          extra: `${group.name} · ${entry.isCurrent ? '当前在用' : '旧版本'} · ${entry.fileCount} 个文件`,
        });
      });
    });
    return rows;
  }, [result]);

  // 接入 AI 助手：**只读**。不给 select/delete —— 这里牵扯"删了会丢配置"和"版本号判断"，
  // 必须由用户看着列表逐项确认，不适合让模型提议后一键执行。
  useAiModuleBridge('dev-cache', {
    readItems: (targetId, offset, limit) => {
      const all = flattenEntries();
      const wanted = targetId.trim();
      const matched = wanted
        ? all.filter(
            (row) =>
              row.groupName === wanted || row.label === wanted || row.path.startsWith(wanted),
          )
        : all;
      if (matched.length === 0) {
        // 不返回 null：公共层只会回一句"没有读到内容"，模型就只能猜原因，
        // 而"还没扫描"和"没匹配到"是两条完全不同的排查方向，要让它能直接说出来
        return {
          total: 0,
          lines: [
            all.length === 0
              ? '（当前还没有扫描结果 —— 请先点一次「开始扫描」，再问这个问题）'
              : `（没有匹配「${wanted}」的目录；本次扫描共 ${all.length} 项，不带参数即可读全部）`,
          ],
        };
      }
      return {
        total: matched.length,
        lines: matched
          .slice(offset, offset + limit)
          .map((row) => `${row.path}  ${formatSize(row.size)}  （${row.extra}）`),
      };
    },
    readItemDetail: (itemId) => {
      const wanted = itemId.trim();
      if (!wanted) return null;
      const all = flattenEntries();
      const row =
        all.find((item) => item.path === wanted) ??
        all.find((item) => item.path.endsWith(`\\${wanted}`));
      if (!row) return null;
      return [
        `分组：${row.groupName}`,
        `名称：${row.label}`,
        `路径：${row.path}`,
        `占用：${formatSize(row.size)}`,
        `属性：${row.extra}`,
        '删除方式：不进回收站、不可恢复。',
      ].join('\n');
    },
    summary: () => {
      // 没扫过返回 null，助手会如实说"还没有数据"，而不是编一段
      if (!result) return null;
      if (!hasAnything) {
        return '已完成扫描：没有发现可清理的开发工具链缓存或多版本残留。';
      }
      const topCache = result.groups
        .slice(0, 3)
        .map((group) => `${group.groupName} ${formatSize(group.totalSize)}`)
        .join('、');
      const lines = [
        `扫描到 ${result.groups.length} 组工具缓存、${result.versionGroups.length} 组多版本目录，`,
        `合计 ${formatSize(result.totalSize)}（${result.totalFiles} 个文件）。`,
      ];
      if (topCache) lines.push(`占用最多的工具链：${topCache}。`);
      if (result.versionReclaimableSize > 0) {
        lines.push(`旧版本目录里可回收 ${formatSize(result.versionReclaimableSize)}。`);
      }
      lines.push(`当前已勾选 ${selected.size} 项，合计 ${formatSize(selectedSize)}。`);
      lines.push(
        '口径说明：只统计开发工具链缓存、以及同一软件并排存放的旧版本目录；',
        '安装包、已安装程序本体、用户文档不在统计范围内。',
      );
      return lines.join('');
    },
  });

  /**
   * 执行扫描。
   *
   * `preserveCleared` 为真表示这是删除后的自动重扫：不清掉"刚清空"标记，
   * 否则空结果会先闪一帧"没有发现可清理的目录"再变成完成态。
   */
  const runScan = useCallback(async (preserveCleared: boolean) => {
    if (!preserveCleared) setJustCleared(false);
    setScanning(true);
    setError('');
    try {
      const next = await scanDevCache();
      setResult(next);
      // 重新扫描后旧的勾选可能指向已不存在的目录，全部清掉最安全
      setSelected(new Set());
      setExpanded(null);
      // 缓存清干净了就自动跳到还有内容的那个页签，免得用户对着一张空卡片发愣
      if (next.groups.length === 0 && next.versionGroups.length > 0) setTab('version');
    } catch (scanError) {
      console.error('扫描开发空间失败:', scanError);
      setError(String(scanError));
    } finally {
      setScanning(false);
    }
  }, []);

  /** 用户主动扫描：回到常规空状态文案 */
  const handleScan = useCallback(() => runScan(false), [runScan]);

  const toggleEntry = useCallback((path: string) => {
    setSelected((previous) => {
      const next = new Set(previous);
      if (next.has(path)) {
        next.delete(path);
      } else {
        next.add(path);
      }
      return next;
    });
  }, []);

  const toggleGroup = useCallback((paths: string[]) => {
    setSelected((previous) => {
      const next = new Set(previous);
      const allSelected = paths.every((path) => next.has(path));
      paths.forEach((path) => (allSelected ? next.delete(path) : next.add(path)));
      return next;
    });
  }, []);

  const handleDelete = useCallback(async () => {
    setConfirmOpen(false);
    if (selected.size === 0) return;
    setDeleting(true);
    try {
      const deleteResult = await deleteDevCachePaths([...selected]);
      // 三种结果分开报：IDE / 构建进程开着时文件被占用是常态，
      // 只报"已删除 N 个"会让用户以为清干净了，实际上大小一点没降。
      if (deleteResult.failed.length === 0) {
        showToast({
          type: 'success',
          title: moduleT('devCache.deletedTitle', { count: deleteResult.deleted }),
          description: moduleT('devCache.deletedDesc', { size: formatSize(deleteResult.freed) }),
        });
      } else if (deleteResult.deleted === 0) {
        showToast({
          type: 'error',
          title: moduleT('devCache.deleteFailed'),
          description: moduleT('devCache.deleteFailedDesc'),
        });
      } else {
        showToast({
          type: 'warning',
          title: moduleT('devCache.deletePartialTitle', {
            success: deleteResult.deleted,
            failed: deleteResult.failed.length,
          }),
          description: moduleT('devCache.deletePartialDesc'),
        });
      }
      // 标记要在重扫之前落下：删干净后列表为空，若等重扫回来再置位，
      // 会先渲染一帧"没有发现可清理的目录"
      if (deleteResult.deleted > 0) setJustCleared(true);
      await runScan(deleteResult.deleted > 0);
    } catch (deleteError) {
      console.error('删除开发空间目录失败:', deleteError);
      showToast({
        type: 'error',
        title: moduleT('devCache.deleteFailed'),
        description: String(deleteError),
      });
    } finally {
      setDeleting(false);
    }
  }, [moduleT, runScan, selected, showToast]);

  const busy = scanning || deleting;
  /**
   * 底部操作条是否显示。
   *
   * ⚠️ 这一段不能省：`ModuleOperationToolbar` 用 portal 渲染成 `position: fixed` 的悬浮层，
   * **不受模块容器约束** —— 不加这个开关，切到别的模块时它还会浮在人家页面上。
   */
  const shouldShowOperationToolbar = isPageActive;

  /** 分组头里的勾选框是"整组全选"，所以要点在子元素上并阻止冒泡，别把折叠也触发了 */
  const renderGroupHeaderCheckbox = (paths: string[]) => {
    const selectedCount = paths.filter((path) => selected.has(path)).length;
    const allSelected = paths.length > 0 && selectedCount === paths.length;
    const partiallySelected = selectedCount > 0 && !allSelected;
    return (
      <div
        onClick={(event) => {
          event.stopPropagation();
          toggleGroup(paths);
        }}
        className="cursor-pointer"
      >
        <Checkbox checked={allSelected} partial={partiallySelected} />
      </div>
    );
  };

  const cacheEntries = result?.groups ?? [];
  const versionEntries = result?.versionGroups ?? [];
  const tabIsEmpty = tab === 'cache' ? cacheEntries.length === 0 : versionEntries.length === 0;

  return (
    <ModuleCard
      id="dev-cache"
      title={navT('devCache')}
      description={navT('devCacheDesc')}
      icon={<Package className="w-6 h-6 text-[var(--brand-green)]" />}
      status={busy ? 'scanning' : result ? 'done' : 'idle'}
      fileCount={result?.totalFiles ?? 0}
      totalSize={result?.totalSize ?? 0}
      onScan={handleScan}
      error={error}
    >
      <div className="p-4">
        {scanning ? (
          /* 扫描中不渲染上一次的结果：旧数据在重扫期间既可能已经过期，
             也会让用户对着「正在重扫的旧列表」去勾选、删除。 */
          <ModuleScanProgress
            title={moduleT('devCache.scanning')}
            description={moduleT('devCache.scanningDesc')}
          />
        ) : !result ? (
          <EmptyState
            icon={Package}
            title={moduleT('devCache.idleTitle')}
            description={moduleT('devCache.idleDesc')}
            action={
              <button
                type="button"
                onClick={handleScan}
                disabled={scanning}
                className="inline-flex items-center gap-1.5 rounded-lg bg-[var(--brand-green)] px-4 py-2 text-sm font-medium text-white transition-opacity hover:opacity-90 disabled:opacity-50"
              >
                <RefreshCw className={`h-4 w-4 ${scanning ? 'animate-spin' : ''}`} />
                {moduleT('devCache.scan')}
              </button>
            }
          />
        ) : !hasAnything ? (
          <EmptyState
            /* 刚删干净是完成态：换成对勾图标 + 完成文案，不再提示"可能没装" */
            icon={justCleared ? undefined : Package}
            tone={justCleared ? 'success' : 'neutral'}
            title={justCleared ? moduleT('devCache.clearedTitle') : moduleT('devCache.emptyTitle')}
            /* 空字符串而不是 undefined：EmptyState 对 undefined 会回落到通用描述 */
            description={justCleared ? '' : moduleT('devCache.emptyDesc')}
            action={
              <button
                type="button"
                onClick={handleScan}
                className="text-sm text-[var(--brand-green)] hover:underline"
              >
                {moduleT('devCache.rescan')}
              </button>
            }
          />
        ) : (
          <div className="space-y-3 pb-20">
            {/* 三个数：缓存总量 / 可直接清理 / 旧版本可回收。
                分开摆是刻意的 —— 后两个才是"点一下就真能释放多少"，
                总量里含只读项（安装缓存），拿它当清理目标会失望。 */}
            <div className="grid grid-cols-3 gap-2">
              {[
                {
                  label: moduleT('devCache.statTotal'),
                  value: formatSize(result.totalSize),
                  tone: 'text-[var(--text-primary)]',
                },
                {
                  label: moduleT('devCache.statSafe'),
                  value: formatSize(result.safeSize),
                  tone: 'text-[var(--brand-green)]',
                },
                {
                  label: moduleT('devCache.statVersion'),
                  value: formatSize(result.versionReclaimableSize),
                  tone: 'text-[var(--text-primary)]',
                },
              ].map((item) => (
                <div key={item.label} className="rounded-xl bg-[var(--bg-main)] px-3 py-2">
                  <p className="text-[0.76rem] text-[var(--text-muted)]">{item.label}</p>
                  <p className={`mt-0.5 text-[1.05rem] font-bold tabular-nums ${item.tone}`}>
                    {item.value}
                  </p>
                </div>
              ))}
            </div>
            {/* 跳过数只在非零时出现：正常机器上它恒为 0，一直显示只会让人追问"什么是跳过" */}
            {result.skipped > 0 && (
              <p className="px-1 text-[0.76rem] text-[var(--text-faint)]">
                {moduleT('devCache.skipped', { count: result.skipped })}
              </p>
            )}

            {/* 页签：两个页签都没内容时不显示，免得出现"空标签点进去更空" */}
            <div className="flex gap-1 rounded-xl bg-[var(--bg-main)] p-1">
              {(
                [
                  { id: 'cache' as ViewTab, label: moduleT('devCache.tabCache'), count: cacheEntries.length },
                  { id: 'version' as ViewTab, label: moduleT('devCache.tabVersion'), count: versionEntries.length },
                ]
              ).map((item) => (
                <button
                  key={item.id}
                  type="button"
                  onClick={() => setTab(item.id)}
                  className={`flex-1 rounded-lg px-3 py-1.5 text-[0.86rem] font-medium transition-colors ${
                    tab === item.id
                      ? 'bg-[var(--bg-card)] text-[var(--text-primary)] shadow-sm'
                      : 'text-[var(--text-muted)] hover:text-[var(--text-secondary)]'
                  }`}
                >
                  {item.label}
                  <span className="ml-1 text-[var(--text-faint)]">{item.count}</span>
                </button>
              ))}
            </div>

            {tabIsEmpty ? (
              <p className="px-1 py-6 text-center text-[0.86rem] text-[var(--text-muted)]">
                {moduleT('devCache.tabEmpty')}
              </p>
            ) : tab === 'cache' ? (
              /* ── 页签一：工具链缓存 ────────────────────────────── */
              cacheEntries.map((group) => {
                const selectable = group.entries
                  .filter((entry) => entry.risk !== 'readonly')
                  .map((entry) => entry.path);
                const key = `c:${group.groupId}`;
                const isOpen = expanded === key;

                return (
                  <div
                    key={key}
                    className="overflow-hidden rounded-2xl bg-[var(--bg-card)] shadow-sm"
                  >
                    <div
                      className="flex cursor-pointer items-center gap-4 px-5 py-4 transition-colors hover:bg-[var(--bg-hover)]"
                      onClick={() => setExpanded(isOpen ? null : key)}
                    >
                      <ChevronRight
                        className={`h-5 w-5 shrink-0 text-[var(--text-faint)] transition-transform ${isOpen ? 'rotate-90' : ''}`}
                      />
                      {renderGroupHeaderCheckbox(selectable)}
                      <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-[var(--brand-green-10)] text-[var(--brand-green)]">
                        <HardDrive className="h-5 w-5" />
                      </div>
                      <div className="min-w-0 flex-1">
                        <p className="truncate text-[1rem] font-bold text-[var(--text-primary)]">
                          {group.groupName}
                        </p>
                        <p className="mt-1 truncate text-[0.93rem] text-[var(--text-muted)]">
                          {moduleT('devCache.summary', {
                            dirs: group.entries.length,
                            files: group.fileCount.toLocaleString(),
                          })}
                        </p>
                      </div>
                      <p className="shrink-0 text-[1.07rem] font-bold tabular-nums text-[var(--text-primary)]">
                        {formatSize(group.totalSize)}
                      </p>
                    </div>

                    <AnimatePresence initial={false}>
                      {isOpen && (
                        <motion.div
                          className="overflow-hidden border-t border-[var(--border-color)] bg-[var(--bg-main)]"
                          initial={{ height: 0, opacity: 0 }}
                          animate={{ height: 'auto', opacity: 1 }}
                          exit={{ height: 0, opacity: 0 }}
                          transition={{ duration: 0.24, ease: [0.22, 1, 0.36, 1] }}
                        >
                          {/* 限高 + 内部滚动：NuGet 那种一次几十个目录，不限高会把卡片撑得极长 */}
                          <div className="max-h-[15em] overflow-y-auto px-5 py-2">
                            {group.entries.map((entry) => {
                              const isReadonly = entry.risk === 'readonly';
                              return (
                                <div
                                  key={entry.path}
                                  className={`flex items-center gap-3 py-2 text-[0.86rem] ${
                                    isReadonly ? 'cursor-default' : 'cursor-pointer'
                                  }`}
                                  onClick={() => {
                                    if (!isReadonly) toggleEntry(entry.path);
                                  }}
                                >
                                  {isReadonly ? (
                                    /* 只读项占位：保持左边对齐，不然整列会往左缩一截 */
                                    <div className="h-5 w-5 shrink-0" />
                                  ) : (
                                    <div
                                      className={`flex h-5 w-5 shrink-0 items-center justify-center rounded border-2 transition-colors ${
                                        selected.has(entry.path)
                                          ? 'border-[var(--brand-green)] bg-[var(--brand-green)]'
                                          : 'border-[var(--text-faint)]'
                                      }`}
                                    >
                                      {selected.has(entry.path) && (
                                        <svg
                                          className="h-3 w-3 text-white"
                                          fill="none"
                                          viewBox="0 0 24 24"
                                          stroke="currentColor"
                                          strokeWidth={3}
                                        >
                                          <path
                                            strokeLinecap="round"
                                            strokeLinejoin="round"
                                            d="M5 13l4 4L19 7"
                                          />
                                        </svg>
                                      )}
                                    </div>
                                  )}
                                  <span className="w-36 shrink-0 truncate font-medium text-[var(--text-secondary)]">
                                    {entry.name}
                                  </span>
                                  <RiskTag
                                    risk={entry.risk}
                                    label={
                                      isReadonly
                                        ? moduleT('devCache.riskReadonly')
                                        : moduleT('devCache.riskConfirm')
                                    }
                                  />
                                  <LinkBadge link={entry.link} />
                                  {/* note 是"删了会怎样"，常驻显示；路径太长会被截断，靠 title 补全 */}
                                  <span className="min-w-0 flex-1 text-[var(--text-muted)]">
                                    <span
                                      className="block truncate"
                                      title={`${entry.path}\n${entry.note}`}
                                    >
                                      {entry.note || entry.path}
                                    </span>
                                    {/* 搬迁过的目录：实体在别的盘，单独一行写清落点，
                                        只放 tooltip 里等于没显示 */}
                                    {entry.link && (
                                      <span
                                        className="block truncate text-[0.78rem] text-[var(--brand-green)]"
                                        title={`${entry.link.linkPath} → ${entry.link.target}`}
                                      >
                                        {moduleT('link.realPath')}：{entry.link.resolved}
                                      </span>
                                    )}
                                  </span>
                                  <span className="w-20 shrink-0 text-right tabular-nums text-[var(--text-muted)]">
                                    {formatSize(entry.size)}
                                  </span>
                                  {/* 打开所在文件夹：先自己看一眼再决定要不要删 */}
                                  <button
                                    type="button"
                                    onClick={(event) => {
                                      event.stopPropagation();
                                      openInFolder(entry.path);
                                    }}
                                    title={moduleT('devCache.openFolder')}
                                    className="shrink-0 rounded p-0.5 text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--brand-green)]"
                                  >
                                    <FolderOpen className="h-4 w-4" />
                                  </button>
                                </div>
                              );
                            })}
                          </div>
                        </motion.div>
                      )}
                    </AnimatePresence>
                  </div>
                );
              })
            ) : (
              /* ── 页签二：多版本残留 ────────────────────────────── */
              versionEntries.map((group) => {
                const selectable = group.entries
                  .filter((entry) => !entry.isCurrent)
                  .map((entry) => entry.path);
                const key = `v:${group.id}`;
                const isOpen = expanded === key;

                return (
                  <div
                    key={key}
                    className="overflow-hidden rounded-2xl bg-[var(--bg-card)] shadow-sm"
                  >
                    <div
                      className="flex cursor-pointer items-center gap-4 px-5 py-4 transition-colors hover:bg-[var(--bg-hover)]"
                      onClick={() => setExpanded(isOpen ? null : key)}
                    >
                      <ChevronRight
                        className={`h-5 w-5 shrink-0 text-[var(--text-faint)] transition-transform ${isOpen ? 'rotate-90' : ''}`}
                      />
                      {renderGroupHeaderCheckbox(selectable)}
                      <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-[var(--brand-green-10)] text-[var(--brand-green)]">
                        <Layers className="h-5 w-5" />
                      </div>
                      <div className="min-w-0 flex-1">
                        <p className="truncate text-[1rem] font-bold text-[var(--text-primary)]">
                          {group.name}
                        </p>
                        {/* 目录路径要露出来：同名软件可能装了两套（系统盘 + 用户盘），
                            只写名字用户分不清删的是哪一个 */}
                        <p
                          className="mt-1 truncate text-[0.93rem] text-[var(--text-muted)]"
                          title={group.basePath}
                        >
                          {moduleT('devCache.versionSummary', {
                            count: group.entries.length,
                            size: formatSize(group.reclaimableSize),
                          })}
                        </p>
                      </div>
                      <p className="shrink-0 text-[1.07rem] font-bold tabular-nums text-[var(--text-primary)]">
                        {formatSize(group.totalSize)}
                      </p>
                    </div>

                    <AnimatePresence initial={false}>
                      {isOpen && (
                        <motion.div
                          className="overflow-hidden border-t border-[var(--border-color)] bg-[var(--bg-main)]"
                          initial={{ height: 0, opacity: 0 }}
                          animate={{ height: 'auto', opacity: 1 }}
                          exit={{ height: 0, opacity: 0 }}
                          transition={{ duration: 0.24, ease: [0.22, 1, 0.36, 1] }}
                        >
                          <div className="max-h-[15em] overflow-y-auto px-5 py-2">
                            {group.entries.map((entry) => (
                              <div
                                key={entry.path}
                                className={`flex items-center gap-3 py-2 text-[0.86rem] ${
                                  entry.isCurrent ? 'cursor-default' : 'cursor-pointer'
                                }`}
                                onClick={() => {
                                  if (!entry.isCurrent) toggleEntry(entry.path);
                                }}
                              >
                                {entry.isCurrent ? (
                                  /* 当前版本不给勾选框，但保留占位对齐 */
                                  <div className="h-5 w-5 shrink-0" />
                                ) : (
                                  <div
                                    className={`flex h-5 w-5 shrink-0 items-center justify-center rounded border-2 transition-colors ${
                                      selected.has(entry.path)
                                        ? 'border-[var(--brand-green)] bg-[var(--brand-green)]'
                                        : 'border-[var(--text-faint)]'
                                    }`}
                                  >
                                    {selected.has(entry.path) && (
                                      <svg
                                        className="h-3 w-3 text-white"
                                        fill="none"
                                        viewBox="0 0 24 24"
                                        stroke="currentColor"
                                        strokeWidth={3}
                                      >
                                        <path
                                          strokeLinecap="round"
                                          strokeLinejoin="round"
                                          d="M5 13l4 4L19 7"
                                        />
                                      </svg>
                                    )}
                                  </div>
                                )}
                                <span className="w-36 shrink-0 truncate font-medium tabular-nums text-[var(--text-secondary)]">
                                  {entry.version}
                                </span>
                                {entry.isCurrent && (
                                  <span className="shrink-0 rounded bg-[var(--brand-green-10)] px-1.5 py-0.5 text-[0.72rem] font-medium text-[var(--brand-green)]">
                                    {moduleT('devCache.currentTag')}
                                  </span>
                                )}
                                {/* current_reason 说明"凭什么判定这版在用"，
                                    没有它的话用户只能相信版本号大小 —— 而那未必是事实 */}
                                <span
                                  className="min-w-0 flex-1 truncate text-[var(--text-muted)]"
                                  title={`${entry.path}\n${entry.currentReason}`}
                                >
                                  {entry.isCurrent
                                    ? entry.currentReason
                                    : moduleT('devCache.versionFiles', {
                                        count: entry.fileCount.toLocaleString(),
                                      })}
                                </span>
                                <span className="w-20 shrink-0 text-right tabular-nums text-[var(--text-muted)]">
                                  {formatSize(entry.size)}
                                </span>
                                <button
                                  type="button"
                                  onClick={(event) => {
                                    event.stopPropagation();
                                    openInFolder(entry.path);
                                  }}
                                  title={moduleT('devCache.openFolder')}
                                  className="shrink-0 rounded p-0.5 text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--brand-green)]"
                                >
                                  <FolderOpen className="h-4 w-4" />
                                </button>
                              </div>
                            ))}
                          </div>
                        </motion.div>
                      )}
                    </AnimatePresence>
                  </div>
                );
              })
            )}

            {/* 操作条复用公共组件：位置、忙碌态、玻璃外观都由它统一处理 */}
            {shouldShowOperationToolbar && (
              <ModuleOperationToolbar
                moduleId="devCache"
                busy={deleting}
                busyContent={
                  <ModuleDeleteProgress
                    variant="floating"
                    phaseLabel={moduleT('devCache.deleting')}
                    progressLabel={moduleT('devCache.deletingDesc', { count: selected.size })}
                  />
                }
              >
                <button
                  type="button"
                  onClick={() =>
                    setSelected(selected.size === selectablePaths.length && selectablePaths.length > 0
                      ? new Set()
                      : new Set(selectablePaths))
                  }
                  disabled={selectablePaths.length === 0}
                  className="module-operation-toolbar__button module-operation-toolbar__button--muted"
                >
                  {selected.size === selectablePaths.length && selectablePaths.length > 0
                    ? moduleT('devCache.deselectAll')
                    : moduleT('devCache.selectAll')}
                </button>
                <button
                  type="button"
                  onClick={() => setConfirmOpen(true)}
                  disabled={selected.size === 0 || deleting}
                  className="module-operation-toolbar__button module-operation-toolbar__button--danger"
                >
                  <Trash2 className="h-3.5 w-3.5" />
                  {moduleT('devCache.delete')} ({selected.size})
                </button>
              </ModuleOperationToolbar>
            )}
          </div>
        )}
      </div>

      <ConfirmDialog
        isOpen={confirmOpen}
        title={moduleT('devCache.confirmTitle', { count: selected.size })}
        description={moduleT('devCache.confirmDesc', { size: formatSize(selectedSize) })}
        warning={moduleT('devCache.confirmWarning')}
        confirmText={moduleT('devCache.delete')}
        /* 删目录不可逆：图标和确认按钮都要走危险色，别用默认的绿色 */
        isDanger
        onConfirm={handleDelete}
        onCancel={() => setConfirmOpen(false)}
      />
    </ModuleCard>
  );
}
