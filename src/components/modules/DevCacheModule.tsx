/**
 * Dev space cleanup module.
 *
 * This module is the light-c counterpart of cscan (the command-line scanner), covering the area that **no existing module handles**:
 * dev toolchain caches (NuGet / Cargo / Gradle / Go / JetBrains…) and sets of old version directories left by the same software.
 *
 * Why two tabs:
 * "Tool Caches" is "a bunch of scattered cache directories grouped by tool", while "Multi-Version Leftovers" is "four side-by-side
 * Microsoft Edge versions under one path, only one of which is in use". The two have different information structures (the latter must mark "which version is current"),
 * forcing them into one tree would hide the difference from users.
 *
 * Why no "clean all in one click": emulator images, workspace state, and local models are gone once deleted,
 * so the user must first see "which directory is being deleted and what would be lost" before confirming; the backend re-validates path ownership too.
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

/** Tabs: tool caches / multi-version leftovers */
type ViewTab = 'cache' | 'version';

/**
 * Checkbox. Reused in three places (group, cache directory, version directory); extracted to keep three implementations from drifting apart.
 * `partial` is only for the group level: draw a dash when part of the group is selected, rather than an empty box.
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

/** Risk badge: no badge for cache (the majority; badging it would be noisy), yellow for confirm, gray for readonly */
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
   * Cleanable items were just all deleted.
   *
   * Different from "never found anything": the former is a completed state, the latter means "possibly not installed / structure mismatch",
   * showing "these tools may not be installed" right after a cleanup would make users think the scan broke.
   */
  const [justCleared, setJustCleared] = useState(false);
  const [error, setError] = useState('');
  const [tab, setTab] = useState<ViewTab>('cache');
  /** Currently expanded card (accordion: only one open at a time). Cache groups and version groups share one key space, distinguished by prefix */
  const [expanded, setExpanded] = useState<string | null>(null);
  /** Checked directory paths (one set shared by both tabs; deleted together) */
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [confirmOpen, setConfirmOpen] = useState(false);

  /** Path → size. Checked totals span both tabs; a separate map is easier than traversing each time */
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

  /** Paths that can be checked in the current tab (read-only items and the current version excluded), for the select-all button */
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

  /** Whether there is anything to show: empty only when both tabs are empty */
  const hasAnything = useMemo(() => {
    if (!result) return false;
    return result.groups.length > 0 || result.versionGroups.length > 0;
  }, [result]);

  /**
   * Flattens the result into "one item per line" text for the AI assistant's read-only tools.
   *
   * Order follows the UI: cache groups first, then version groups, each sorted by size descending —
   * so the first line the assistant reads is the biggest space consumer, matching what the user sees on screen.
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
            // Relocated directories must include their real location, otherwise the assistant interprets usage by the C-drive path
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

  // Hook up the AI assistant: **read-only**. No select/delete — this involves "deletion loses config" and "version number judgment",
  // so the user must review the list and confirm item by item; not suitable for one-click execution on a model's proposal.
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
        // Do not return null: the common layer would just say "no content read", leaving the model to guess why,
        // while "not scanned yet" and "no match" are completely different troubleshooting directions; let it say so directly
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
      // If never scanned, return null so the assistant truthfully says "no data yet" instead of making something up
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
   * Performs the scan.
   *
   * `preserveCleared` true means this is an automatic rescan after deletion: keep the "just cleared" flag,
   * otherwise an empty result would flash a frame of "no cleanable directories found" before reaching the completed state.
   */
  const runScan = useCallback(async (preserveCleared: boolean) => {
    if (!preserveCleared) setJustCleared(false);
    setScanning(true);
    setError('');
    try {
      const next = await scanDevCache();
      setResult(next);
      // After rescanning, old selections may point to directories that no longer exist; clearing them all is safest
      setSelected(new Set());
      setExpanded(null);
      // Once caches are cleared, auto-switch to the tab that still has content so users do not stare at an empty card
      if (next.groups.length === 0 && next.versionGroups.length > 0) setTab('version');
    } catch (scanError) {
      console.error('扫描开发空间失败:', scanError);
      setError(String(scanError));
    } finally {
      setScanning(false);
    }
  }, []);

  /** User-initiated scan: return to the normal empty-state text */
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
      // Report three outcomes separately: files being in use while IDE/build processes run is normal,
      // reporting only "deleted N items" makes users think it is clean when the size has not dropped at all.
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
      // Set the flag before rescanning: after a full cleanup the list is empty, and waiting until after rescan to set it
      // would render a frame of "no cleanable directories found" first
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
   * Whether to show the bottom toolbar.
   *
   * ⚠️ This cannot be omitted: `ModuleOperationToolbar` renders via portal into a `position: fixed` floating layer,
   * **not constrained by the module container** — without this switch, it would keep floating over other modules' pages.
   */
  const shouldShowOperationToolbar = isPageActive;

  /** The checkbox in the group header means "select whole group", so click on child elements and stop propagation to avoid also triggering collapse */
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
          /* Do not render the previous results while scanning: during a rescan old data may already be stale,
             and would let users check or delete against "an old list being rescanned". */
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
            /* Just-cleared is a completed state: switch to a check icon + completion text, no "possibly not installed" hint */
            icon={justCleared ? undefined : Package}
            tone={justCleared ? 'success' : 'neutral'}
            title={justCleared ? moduleT('devCache.clearedTitle') : moduleT('devCache.emptyTitle')}
            /* Empty string rather than undefined: EmptyState falls back to a generic description for undefined */
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
            {/* Three numbers: total caches / directly cleanable / reclaimable from old versions.
                Shown separately on purpose — the latter two are "how much you actually free with one click",
                the total includes read-only items (install caches), so treating it as the cleanup target would disappoint. */}
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
            {/* Skip count only appears when non-zero: on a normal machine it is always 0, showing it constantly would only make users ask "what is skipped" */}
            {result.skipped > 0 && (
              <p className="px-1 text-[0.76rem] text-[var(--text-faint)]">
                {moduleT('devCache.skipped', { count: result.skipped })}
              </p>
            )}

            {/* Tabs: hidden when both tabs are empty, to avoid "empty tab that is even emptier inside" */}
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
              /* ── Tab 1: toolchain caches ────────────────────────────── */
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
                          {/* Height cap + internal scrolling: NuGet can produce dozens of directories at once; without a cap the card would become extremely long */}
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
                                    /* Placeholder for read-only items: keeps left alignment, otherwise the whole column shifts left */
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
                                  {/* note is "what happens if deleted", always shown; long paths are truncated, completed via the title attribute */}
                                  <span className="min-w-0 flex-1 text-[var(--text-muted)]">
                                    <span
                                      className="block truncate"
                                      title={`${entry.path}\n${entry.note}`}
                                    >
                                      {entry.note || entry.path}
                                    </span>
                                    {/* Relocated directories: the real data is on another drive, so spell out the destination on its own line,
                                        putting it only in a tooltip would be as good as not showing it */}
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
                                  {/* Open containing folder: look for yourself first before deciding whether to delete */}
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
              /* ── Tab 2: multi-version leftovers ────────────────────────────── */
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
                        {/* Show the directory path: the same software may be installed twice (system drive + user drive),
                            a name alone would leave users unable to tell which one is being deleted */}
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
                                  /* No checkbox for the current version, but keep a placeholder for alignment */
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
                                {/* current_reason explains "why this version is judged as in use",
                                    without it users can only trust the version number — which may not be the truth */}
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

            {/* Toolbar reuses the shared component: position, busy state, and glass styling are all handled by it */}
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
        /* Deleting directories is irreversible: use danger colors for the icon and confirm button, not the default green */
        isDanger
        onConfirm={handleDelete}
        onCancel={() => setConfirmOpen(false)}
      />
    </ModuleCard>
  );
}
