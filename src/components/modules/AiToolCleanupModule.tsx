/**
 * AI tool cleanup module.
 *
 * Interaction matches the dedicated social cleanup: collapse by tool (accordion), expanding shows a list of cache directories,
 * and after selecting, a second confirmation is required before deleting.
 *
 * Why there is no "one-click clean all": next to these cache directories are login credentials, chat histories, and runtimes,
 * the user must see "which tool's which directory" is being deleted before confirming. The backend validates path ownership once more.
 */
import { useCallback, useMemo, useState } from 'react';
import { AnimatePresence, motion } from 'framer-motion';
import { useTranslation } from 'react-i18next';
import { Bot, ChevronRight, FolderOpen, HardDrive, RefreshCw, Trash2 } from 'lucide-react';
import { ModuleCard } from '../ModuleCard';
import { EmptyState } from '../EmptyState';
import { ConfirmDialog } from '../ConfirmDialog';
import { AiCacheFileModal } from './AiCacheFileModal';
import { ModuleOperationToolbar } from '../ModuleOperationToolbar';
import { LinkBadge, displayPathOf } from '../LinkBadge';
import { ModuleDeleteProgress } from '../ModuleDeleteProgress';
import { ModuleScanProgress } from '../ModuleScanProgress';
import { deleteAiToolCaches, scanAiToolCaches } from '../../api/aiToolCleanup';
import { openInFolder } from '../../api/commands';
import type { AiToolScanResult } from '../../types/aiToolCleanup';
import { formatSize } from '../../utils/format';
import { useToast } from '../Toast';
import { useAiModuleBridge } from '../ai/useAiModuleBridge';
import type { ModuleRenderProps } from './moduleProps';

export function AiToolCleanupModule({ isPageActive = true }: ModuleRenderProps) {
  const { t: navT } = useTranslation('nav');
  const { t: moduleT } = useTranslation('modules');
  const { showToast } = useToast();
  // Expansion state lives in the global context, sharing the same "currently expanded module" as the dedicated social cleanup,
  // otherwise maintaining separate copies would show two operation bars at once.

  const [scanning, setScanning] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [result, setResult] = useState<AiToolScanResult | null>(null);
  /**
   * The cleanable items have just been fully deleted.
   *
   * This is different from "never scanned": the former is a completed state, the latter means "possibly not installed / structure mismatch",
   * hinting "these tools may not be installed" right after cleaning would make people think the scan is broken.
   */
  const [justCleared, setJustCleared] = useState(false);
  const [error, setError] = useState('');
  /** Currently expanded tool (accordion: only one expanded at a time) */
  const [expandedTool, setExpandedTool] = useState<string | null>(null);
  /** Selected cache directory paths */
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [confirmOpen, setConfirmOpen] = useState(false);
  /** Cache directory whose details are being viewed; null means the modal is closed */
  const [viewDir, setViewDir] = useState<{ path: string; name: string } | null>(null);

  const allPaths = useMemo(
    () => (result ? result.groups.flatMap((group) => group.entries.map((entry) => entry.path)) : []),
    [result],
  );
  const selectedSize = useMemo(() => {
    if (!result) return 0;
    return result.groups
      .flatMap((group) => group.entries)
      .filter((entry) => selected.has(entry.path))
      .reduce((total, entry) => total + entry.size, 0);
  }, [result, selected]);

  /**
   * Flatten the groups into "cache directory + owning tool".
   *
   * Order follows the UI: tools by descending total usage, entries within a group by descending size - the first row the assistant reads is the biggest space consumer,
   * consistent with what the user sees on screen, so citations in answers will not mismatch.
   */
  const flattenEntries = useCallback(() => {
    if (!result) return [];
    return result.groups.flatMap((group) =>
      group.entries.map((entry) => ({
        toolId: group.toolId,
        toolName: group.toolName,
        entry,
      })),
    );
  }, [result]);

  // Integrate with the AI assistant: **read-only**. No select/delete - deleting caches must be confirmed item by item by the user watching the list,
  // this path also involves "cannot delete while the tool is running", so it is not suitable for one-click execution after a model suggestion.
  useAiModuleBridge('ai-tool-cleanup', {
    // Read the cache directory list (one page per batch; the model decides whether to continue).
    readItems: (targetId, offset, limit) => {
      const all = flattenEntries();
      const wanted = targetId.trim();
      // targetId may be a tool name, tool id, or a parent directory path - all three are accepted
      const matched = wanted
        ? all.filter(
            (row) =>
              row.toolName === wanted ||
              row.toolId === wanted ||
              row.entry.path.startsWith(wanted),
          )
        : all;
      if (matched.length === 0) {
        // Do not return null: the common layer would only say "no content read", leaving the model to guess the reason,
        // while "not scanned yet" and "no match" are completely different troubleshooting directions; let it state them directly
        return {
          total: 0,
          lines: [
            all.length === 0
              ? '（当前还没有扫描结果 —— 请先点一次「开始扫描」，再问这个问题）'
              : `（没有匹配「${wanted}」的缓存目录；本次扫描共 ${all.length} 个，不带参数即可读全部）`,
          ],
        };
      }
      return {
        total: matched.length,
        lines: matched
          .slice(offset, offset + limit)
          .map(
            (row) =>
              `${row.entry.path}  ${formatSize(row.entry.size)}  （${row.toolName} · ${row.entry.name} · ${row.entry.fileCount} 个文件）` +
              // For relocated directories, the real location must be spelled out, otherwise the assistant would misinterpret the C: path
              (row.entry.link ? `  [${row.entry.link.kind} → 实际位置 ${row.entry.link.resolved}]` : ''),
          ),
      };
    },
    // Single-item detail. itemId is the full path at the start of each row above; the model may also return just a directory name ("Cache"),
    // which is accepted only when unique across the whole list - otherwise Devin's Cache would be mistaken for Kimi's.
    readItemDetail: (itemId) => {
      const wanted = itemId.trim();
      if (!wanted) return null;
      const all = flattenEntries();
      const sameName = all.filter((row) => row.entry.name === wanted);
      const row =
        all.find((item) => item.entry.path === wanted) ??
        all.find((item) => item.entry.path.endsWith(`\\${wanted}`)) ??
        (sameName.length === 1 ? sameName[0] : undefined);
      if (!row) return null;

      const siblings = all.filter(
        (item) => item.toolId === row.toolId && item.entry.path !== row.entry.path,
      );
      const siblingSize = siblings.reduce((total, item) => total + item.entry.size, 0);
      return [
        `工具：${row.toolName}`,
        `目录：${row.entry.name}`,
        `路径：${row.entry.path}`,
        row.entry.link
          ? `链接：${row.entry.link.linkPath} 是${row.entry.link.kind === 'junction' ? '目录联接' : '符号链接'}，指向 ${row.entry.link.target}；实体在 ${row.entry.link.resolved}（占用记在那边，不是原盘符）`
          : '',
        `占用：${formatSize(row.entry.size)}（${row.entry.fileCount} 个文件）`,
        '性质：客户端自己生成的缓存，删掉后工具下次运行会重新生成，不影响已装扩展、设置和登录状态。',
        siblings.length > 0
          ? `同一工具下还有 ${siblings.length} 个可清理目录，合计 ${formatSize(siblingSize)}。`
          : '同一工具下没有别的可清理目录。',
        '删除方式：不进回收站、不可恢复；工具正在运行时可能因文件被占用而删不掉。',
      ]
        .filter(Boolean)
        .join('\n');
    },
    summary: () => {
      // If never scanned, return null so the assistant truthfully says "no data yet" instead of making something up
      if (!result) return null;
      if (result.groups.length === 0) {
        return '已完成扫描：当前没有发现可清理的 AI 工具缓存（这些工具可能都没安装，或者缓存目录结构与预期不同）。';
      }
      const all = flattenEntries();
      const top = result.groups
        .slice(0, 3)
        .map((group) => `${group.toolName} ${formatSize(group.totalSize)}`)
        .join('、');
      return [
        `扫描到 ${result.groups.length} 个工具留下的 ${all.length} 个缓存目录，合计 ${formatSize(result.totalSize)}（${result.totalFiles} 个文件）。`,
        `占用最多的：${top}。`,
        `当前已勾选 ${selected.size} 个目录。`,
        '口径说明：只统计缓存类目录（Cache / GPUCache / logs / CachedData 这类）。',
        '会话历史、登录凭据、插件本体、站点数据都不在统计范围内，',
        '所以这些工具的实际总占用会明显大于这里的数字，这是设计如此，不是漏扫。',
      ].join('');
    },
  });

  /**
   * Perform the scan.
   *
   * `preserveCleared` being true means this is an automatic rescan after deletion: do not clear the "just cleared" flag,
   * otherwise the empty result would flash "no cleanable caches found" for a frame before becoming the completed state.
   */
  const runScan = useCallback(async (preserveCleared: boolean) => {
    if (!preserveCleared) setJustCleared(false);
    setScanning(true);
    setError('');
    try {
      const next = await scanAiToolCaches();
      setResult(next);
      // After rescanning, old selections may point to directories that no longer exist; clearing all is safest
      setSelected(new Set());
      setExpandedTool(null);
    } catch (scanError) {
      console.error('扫描 AI 工具缓存失败:', scanError);
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
      const result = await deleteAiToolCaches([...selected]);
      // Report the three outcomes separately: when a tool is open, locked cache files are common,
      // reporting only "deleted N" would make the user think everything was cleaned when the size did not drop at all.
      if (result.failed.length === 0) {
        showToast({
          type: 'success',
          title: moduleT('aiTools.deletedTitle', { count: result.deleted }),
          description: moduleT('aiTools.deletedDesc'),
        });
      } else if (result.deleted === 0) {
        showToast({
          type: 'error',
          title: moduleT('aiTools.deleteFailed'),
          description: moduleT('aiTools.deleteFailedDesc'),
        });
      } else {
        showToast({
          type: 'warning',
          title: moduleT('aiTools.deletePartialTitle', {
            success: result.deleted,
            failed: result.failed.length,
          }),
          description: moduleT('aiTools.deletePartialDesc'),
        });
      }
      // Set the flag before rescanning: after a complete cleanup the list is empty; if we set it after the rescan returns,
      // a frame of "no cleanable caches found" would render first
      if (result.deleted > 0) setJustCleared(true);
      await runScan(result.deleted > 0);
    } catch (deleteError) {
      console.error('删除 AI 工具缓存失败:', deleteError);
      showToast({
        type: 'error',
        title: moduleT('aiTools.deleteFailed'),
        description: String(deleteError),
      });
    } finally {
      setDeleting(false);
    }
  }, [moduleT, runScan, selected, showToast]);

  const busy = scanning || deleting;
  /**
   * Whether to show the bottom operation bar.
   *
   * ⚠️ This part cannot be omitted: `ModuleOperationToolbar` renders via portal into a `position: fixed` floating layer,
   * **not constrained by the module container** - without this switch, it would float over other modules' pages.
   * Show it only in the current module, avoiding a shared display condition across modules.
   */
  const shouldShowOperationToolbar = isPageActive;

  return (
    <ModuleCard
      id="ai-tools"
      title={navT('aiToolCleanup')}
      description={navT('aiToolCleanupDesc')}
      icon={<Bot className="w-6 h-6 text-[var(--brand-green)]" />}
      status={busy ? 'scanning' : result ? 'done' : 'idle'}
      fileCount={result?.totalFiles ?? 0}
      totalSize={result?.totalSize ?? 0}
      onScan={handleScan}
      error={error}
    >
      <div className="p-4">
        {scanning ? (
          /* Do not render the previous result while scanning: old data may already be stale during the rescan,
             and it would let the user select and delete against an "old list being rescanned".
             The first scan also goes here, so there is no need to distinguish "whether there is an old result". */
          <ModuleScanProgress
            title={moduleT('aiTools.scanning')}
            description={moduleT('aiTools.scanningDesc')}
          />
        ) : !result ? (
          <EmptyState
            icon={Bot}
            title={moduleT('aiTools.idleTitle')}
            description={moduleT('aiTools.idleDesc')}
            action={
              <button
                type="button"
                onClick={handleScan}
                disabled={scanning}
                className="inline-flex items-center gap-1.5 rounded-lg bg-[var(--brand-green)] px-4 py-2 text-sm font-medium text-white transition-opacity hover:opacity-90 disabled:opacity-50"
              >
                <RefreshCw className={`h-4 w-4 ${scanning ? 'animate-spin' : ''}`} />
                {moduleT('aiTools.scan')}
              </button>
            }
          />
        ) : result.groups.length === 0 ? (
          <EmptyState
            /* Just completely cleaned is a completed state: switch to a check icon + completion text, no longer hint "possibly not installed" */
            icon={justCleared ? undefined : Bot}
            tone={justCleared ? 'success' : 'neutral'}
            title={justCleared ? moduleT('aiTools.clearedTitle') : moduleT('aiTools.emptyTitle')}
            /* Empty string instead of undefined: EmptyState falls back to a generic description for undefined */
            description={justCleared ? '' : moduleT('aiTools.emptyDesc')}
            action={
              <button
                type="button"
                onClick={handleScan}
                className="text-sm text-[var(--brand-green)] hover:underline"
              >
                {moduleT('aiTools.rescan')}
              </button>
            }
          />
        ) : (
          // pb-20: the bottom operation bar is a fixed floating layer; add bottom padding so the last tool card is not covered
          <div className="space-y-2 pb-20">
            {result.groups.map((group) => {
              const paths = group.entries.map((entry) => entry.path);
              const selectedInGroup = paths.filter((path) => selected.has(path)).length;
              const allSelected = paths.length > 0 && selectedInGroup === paths.length;
              const partiallySelected = selectedInGroup > 0 && !allSelected;
              const isOpen = expandedTool === group.toolId;

              return (
                <div
                  key={group.toolId}
                  className="overflow-hidden rounded-2xl bg-[var(--bg-card)] shadow-sm"
                >
                  <div
                    className="flex cursor-pointer items-center gap-4 px-5 py-4 transition-colors hover:bg-[var(--bg-hover)]"
                    onClick={() => setExpandedTool(isOpen ? null : group.toolId)}
                  >
                    <ChevronRight
                      className={`h-5 w-5 shrink-0 text-[var(--text-faint)] transition-transform ${isOpen ? 'rotate-90' : ''}`}
                    />
                    {/* Tool-level checkbox: one-click select all cache directories under the current tool */}
                    <div
                      onClick={(event) => {
                        event.stopPropagation();
                        toggleGroup(paths);
                      }}
                      className={`flex h-5 w-5 shrink-0 cursor-pointer items-center justify-center rounded border-2 transition-colors ${
                        allSelected
                          ? 'border-[var(--brand-green)] bg-[var(--brand-green)]'
                          : partiallySelected
                            ? 'border-[var(--brand-green)] bg-[var(--brand-green)]/50'
                            : 'border-[var(--text-faint)] hover:border-[var(--text-muted)]'
                      }`}
                    >
                      {(allSelected || partiallySelected) && (
                        <svg
                          className="h-3 w-3 text-white"
                          fill="none"
                          viewBox="0 0 24 24"
                          stroke="currentColor"
                          strokeWidth={3}
                        >
                          {allSelected ? (
                            <path strokeLinecap="round" strokeLinejoin="round" d="M5 13l4 4L19 7" />
                          ) : (
                            <path strokeLinecap="round" d="M5 12h14" />
                          )}
                        </svg>
                      )}
                    </div>
                    <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-[var(--brand-green-10)] text-[var(--brand-green)]">
                      <HardDrive className="h-5 w-5" />
                    </div>
                    <div className="min-w-0 flex-1">
                      <p className="truncate text-[1rem] font-bold text-[var(--text-primary)]">
                        {group.toolName}
                      </p>
                      <p className="mt-1 truncate text-[0.93rem] text-[var(--text-muted)]">
                        {moduleT('aiTools.summary', {
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
                        {/* Height limit + internal scrolling: a single tool may have thousands of directories/files,
                            without a limit the card would stretch extremely long and other tools would not be visible */}
                        <div className="max-h-[15em] overflow-y-auto px-5 py-2">
                      {group.entries.map((entry) => (
                        <div
                          key={entry.path}
                          className="flex cursor-pointer items-center gap-3 py-2 text-[0.86rem]"
                          onClick={() => toggleEntry(entry.path)}
                        >
                          <div
                            className={`flex h-4 w-4 shrink-0 items-center justify-center rounded border-2 transition-colors ${
                              selected.has(entry.path)
                                ? 'border-[var(--brand-green)] bg-[var(--brand-green)]'
                                : 'border-[var(--text-faint)]'
                            }`}
                          >
                            {selected.has(entry.path) && (
                              <svg
                                className="h-2.5 w-2.5 text-white"
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
                          <span className="w-40 shrink-0 truncate font-medium text-[var(--text-secondary)]">
                            {entry.name}
                          </span>
                          {/* Relocated directory: the path still reads C:\..., but the actual data is on another drive.
                              Without marking it, the user would think the space is counted on the C: drive. */}
                          <span
                            className="min-w-0 flex-1 truncate text-[var(--text-muted)]"
                            title={
                              entry.link
                                ? `${entry.path}\n${entry.link.linkPath} → ${entry.link.target}`
                                : entry.path
                            }
                          >
                            {entry.link && <LinkBadge link={entry.link} />}
                            <span
                              className={
                                entry.link ? 'text-[var(--text-secondary)]' : undefined
                              }
                            >
                              {displayPathOf(entry.path, entry.link)}
                            </span>
                          </span>
                          <span className="w-20 shrink-0 text-right tabular-nums text-[var(--text-muted)]">
                            {formatSize(entry.size)}
                          </span>
                          {/* Open containing folder: look in Explorer first before deciding whether to delete */}
                          <button
                            type="button"
                            onClick={(event) => {
                              event.stopPropagation();
                              openInFolder(entry.path);
                            }}
                            title={moduleT('aiTools.openFolder')}
                            className="shrink-0 rounded p-0.5 text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--brand-green)]"
                          >
                            <FolderOpen className="h-4 w-4" />
                          </button>
                          {/* Details via modal: a directory may contain tens of thousands of files, so do not expand inline */}
                          <button
                            type="button"
                            onClick={(event) => {
                              event.stopPropagation();
                              setViewDir({ path: entry.path, name: entry.name });
                            }}
                            className="shrink-0 text-[var(--brand-green)] hover:underline"
                          >
                            {moduleT('aiTools.viewAll')}
                          </button>
                        </div>
                      ))}
                        </div>
                      </motion.div>
                    )}
                  </AnimatePresence>
                </div>
              );
            })}

            {/* The operation bar reuses the shared component: same floating layer as social cleanup, big files, and context menu,
                position, busy state, and glass appearance are all handled by it; do not build a separate one here. */}
            {shouldShowOperationToolbar && (
            <ModuleOperationToolbar
              moduleId="aiTools"
              busy={deleting}
              busyContent={
                <ModuleDeleteProgress
                  variant="floating"
                  phaseLabel={moduleT('aiTools.deleting')}
                  progressLabel={moduleT('aiTools.deletingDesc', { count: selected.size })}
                />
              }
            >
              {/* All styles use the shared operation bar's semantic classes; do not write colors and font sizes here */}
              <button
                type="button"
                onClick={() =>
                  setSelected(selected.size === allPaths.length ? new Set() : new Set(allPaths))
                }
                disabled={allPaths.length === 0}
                className="module-operation-toolbar__button module-operation-toolbar__button--muted"
              >
                {selected.size === allPaths.length && allPaths.length > 0
                  ? moduleT('aiTools.deselectAll')
                  : moduleT('aiTools.selectAll')}
              </button>
              <button
                type="button"
                onClick={() => setConfirmOpen(true)}
                disabled={selected.size === 0 || deleting}
                className="module-operation-toolbar__button module-operation-toolbar__button--danger"
              >
                <Trash2 className="h-3.5 w-3.5" />
                {moduleT('aiTools.delete')} ({selected.size})
              </button>
            </ModuleOperationToolbar>
            )}
          </div>
        )}
      </div>

      <AiCacheFileModal
        isOpen={viewDir !== null}
        dirPath={viewDir?.path ?? null}
        dirName={viewDir?.name ?? ''}
        onClose={() => setViewDir(null)}
      />

      <ConfirmDialog
        isOpen={confirmOpen}
        title={moduleT('aiTools.confirmTitle', { count: selected.size })}
        description={moduleT('aiTools.confirmDesc', { size: formatSize(selectedSize) })}
        warning={moduleT('aiTools.confirmWarning')}
        confirmText={moduleT('aiTools.delete')}
        /* Deleting directories is irreversible: use the danger color for the icon and confirm button, not the default green */
        isDanger
        onConfirm={handleDelete}
        onCancel={() => setConfirmOpen(false)}
      />
    </ModuleCard>
  );
}
