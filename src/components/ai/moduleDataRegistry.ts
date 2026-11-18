// ============================================================================
// Module data summary registry
//
// Lets the AI assistant see "what the current module is showing", without **requiring modules to change their own structure**:
// a module registers a "get summary" function on mount, and the assistant calls it before each send to get the latest data.
//
// Three hard constraints:
// 1. Register only **summaries**. Details never go into the prompt - junk cleanup can find thousands of files in one scan,
//    and injecting them all would blow up the context, slow, expensive, and still inaccurate. Details are left for later tool calls on demand.
// 2. Use a **function**, not a snapshot: module data changes continuously with scans and selections, so every send must fetch the latest.
// 3. Unmount must unregister. Otherwise the assistant would answer with stale data from the previous module - worse than "not knowing".
// ============================================================================

import { useEffect, useRef } from 'react';

/** Returns a plain-text summary; returning null means "no data currently" (e.g. not scanned yet). */
export type ModuleDataSummaryProvider = () => string | null;

const providers = new Map<string, ModuleDataSummaryProvider>();

/** Register a data summary provider for a module, returning an unregister function. */
export function registerModuleDataProvider(
  moduleId: string,
  provider: ModuleDataSummaryProvider,
): () => void {
  providers.set(moduleId, provider);
  return () => {
    // Only delete if "the currently registered provider is still ours": prevents an old instance's unmount from removing a new instance's registration
    // (in page mode a module instance may mount and then unmount, order not guaranteed).
    if (providers.get(moduleId) === provider) {
      providers.delete(moduleId);
    }
  };
}

/** Get a module's summary. Returns null if nothing is registered or summary generation throws (the assistant honestly says "no data"). */
export function getModuleDataSummary(moduleId: string | null): string | null {
  if (!moduleId) return null;
  const provider = providers.get(moduleId);
  if (!provider) return null;
  try {
    return provider();
  } catch (error) {
    // A summary generation failure must not affect the conversation itself; fall back to "no data"
    console.error('生成模块数据摘要失败:', error);
    return null;
  }
}

/**
 * Registration hook for use on the module side.
 * The builder is kept in a ref: otherwise every render would re-register/unregister, needlessly creating side effects.
 */
export function useModuleDataSummary(moduleId: string, builder: () => string | null): void {
  const builderRef = useRef(builder);
  builderRef.current = builder;

  useEffect(() => registerModuleDataProvider(moduleId, () => builderRef.current()), [moduleId]);
}
