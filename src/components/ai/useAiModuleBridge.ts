// ============================================================================
// The single entry point for modules to hook into the AI assistant
//
// Design goal: **modules write only this one hook, and different modules write it in exactly the same shape.**
//
// Why it's worth a separate layer: differences between modules come from the **business itself** and can't be eliminated —
// junk cleanup has category lists, the old driver has driver packages, big files has a directory tree, so summaries inevitably differ.
// But the **pipeline** that "feeds the differences to AI" must be completely identical; otherwise 11 modules mean 11 different styles,
// and that's what's truly hard to maintain.
//
// So the division of labor is:
//   - Module provides: **what the data looks like** (summary) + **what can be operated on** (targets) + **how to operate** (onSelect / onDelete)
//   - Common layer provides: id format in summaries, injection timing, action registration, confirmation cards, permissions and toggles
//
// ⚠️ About deletion: onDelete **must call the module's original delete function**. This layer won't and shouldn't implement deletion for the module —
//    the module's own validation, backup and double-check logic must keep taking effect, otherwise you'd get a split where "manual deletion is safe
//    but AI deletion takes another path".
// ============================================================================

import type { AiDeleteOutcome, AiReadPage } from './moduleActionRegistry';
import { useModuleActions } from './moduleActionRegistry';
import { useModuleDataSummary } from './moduleDataRegistry';

/** An object the AI can operate on. The id must be stable (the module can resolve it back), label is for humans. */
export interface AiModuleTarget {
  id: string;
  label: string;
  /** Description such as size or item count, formatted by the module */
  detail?: string;
  /** Not recoverable after deletion (like the Recycle Bin); the UI marks it separately */
  irreversible?: boolean;
}

/**
 * Render a page of details as text for the model, **and force-append "which items these are, out of how many"**.
 *
 * This sentence is the most critical constraint in the whole progressive reading flow: only when the model clearly knows it hasn't seen everything
 * will it say "need to look further"; otherwise it assumes what it saw is all there is and jumps to a wrong conclusion.
 */
export function formatReadPage(page: AiReadPage, offset: number): string {
  const shown = page.lines.length;
  const more = page.total > offset + shown;
  const footer = more
    ? `\n\n（以上是第 ${offset + 1} 到 ${offset + shown} 条，共 ${page.total} 条，**还有更多未显示**；需要时再调用并加大 offset）`
    : `\n\n（以上是全部 ${page.total} 条）`;
  return page.lines.join('\n') + footer;
}

export interface AiModuleBridge {
  /** A natural-language summary: what was scanned, how big, what the user is doing. Returning null means no data yet. */
  summary?: () => string | null;
  /** List of objects the AI can operate on. An empty array means there's nothing operable right now. */
  targets?: () => AiModuleTarget[];
  /** Called when the AI requests "select these items" (P3a, no risk). */
  onSelect?: (ids: string[]) => void;
  /** Called on AI delete requests **only after user confirmation** (P3b). Must go through the module's original delete entry. */
  onDelete?: (ids: string[]) => Promise<AiDeleteOutcome>;

  /**
   * Read-only: read one page of details under a given operable item.
   * Register this and the model gets the read_items tool — and knows it "can look deeper".
   */
  readItems?: (targetId: string, offset: number, limit: number) => AiReadPage | null;
  /** Read-only: read the detail of a single entry. */
  readItemDetail?: (itemId: string) => string | null;
}

/**
 * Render module data and actions into a summary for the model.
 *
 * **The id presentation format is decided uniformly here**, modules don't need to care —
 * that's the key to "different modules write the same way": modules hand over structured data, formatting happens in the common layer.
 */
export function composeModuleSummary(bridge: AiModuleBridge): string | null {
  const summaryText = bridge.summary?.() ?? null;
  const targets = bridge.targets?.() ?? [];

  if (!summaryText && targets.length === 0) return null;

  const lines: string[] = [];
  if (summaryText) lines.push(summaryText.trim());

  if (targets.length > 0) {
    lines.push(
      '',
      '可操作项（要操作某一项时请使用方括号里的 id，不要用名称）：',
      ...targets.map((target) => {
        const parts = [`[id: ${target.id}]`, target.label];
        if (target.detail) parts.push(target.detail);
        if (target.irreversible) parts.push('（删除后不可恢复）');
        return `- ${parts.join(' · ')}`;
      }),
    );
  }

  return lines.join('\n');
}

/**
 * Modules only need to call this one hook to hook into the AI assistant.
 *
 * ```tsx
 * useAiModuleBridge('junk-clean', {
 *   summary: () => `${result.total_file_count} files in total…`,
 *   targets: () => result.categories.map((c) => ({
 *     id: c.display_name, label: c.display_name, detail: formatSize(c.total_size),
 *   })),
 *   onSelect: (ids) => selectCategories(ids),
 *   onDelete: (ids) => deleteCategories(ids),   // go through the module's original delete entry
 * });
 * ```
 */
export function useAiModuleBridge(moduleId: string, bridge: AiModuleBridge): void {
  // Summary: fetched fresh on every send (internally guarantees the latest closure)
  useModuleDataSummary(moduleId, () => composeModuleSummary(bridge));

  // Actions: register only the ones the module actually provides, leave the rest undefined —
  // callers use this to judge "whether this module can be operated by AI", instead of finding out only after a click that it isn't implemented
  useModuleActions(moduleId, {
    resolveTargets: bridge.targets
      ? (ids) => {
          const available = bridge.targets?.() ?? [];
          // Resolve by id: **do not accept names given directly by the model**, and don't trust stale ids either
          return ids
            .map((id) => available.find((target) => target.id === id))
            .filter((target): target is AiModuleTarget => Boolean(target))
            .map((target) => ({
              id: target.id,
              label: target.label,
              detail: target.detail,
              irreversible: target.irreversible,
            }));
        }
      : undefined,
    selectItems: bridge.onSelect,
    deleteItems: bridge.onDelete,
    // Read-only capabilities pass straight through: unregistered means undefined, the frontend uses that to decide whether to declare tools
    readItems: bridge.readItems,
    readItemDetail: bridge.readItemDetail,
  });
}
