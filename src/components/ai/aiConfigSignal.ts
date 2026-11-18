// ============================================================================
// "Changed" signal for AI config
//
// There's only one copy of the AI config on disk, but **two copies in memory**: one in the assistant panel (`AiAssistant`'s
// `aiConfig`) and one in the settings page (`AiSettings`' `saved`). Saving in the settings page updates only its own copy,
// the assistant's doesn't move — so "just configured a provider then clicked a quick question" still gets blocked (`canChat` is still false).
//
// Here we follow the same idea as `aiAsk.ts` with a minimal module-level broadcast: **the settings page just shouts "changed",
// the assistant re-reads from disk itself**. No state is passed between the two components, and no new state library is introduced.
//
// Why not have the settings page push the new config straight to the assistant: the assistant always needs "the current state on disk",
// there are too many intermediate states (save failure, switching profiles, deleting a profile), so letting the single source of truth (disk) speak is less error-prone.
// ============================================================================

type Listener = () => void;

const listeners = new Set<Listener>();

/** Broadcasts "AI config changed". Called by the settings page after any write to disk. */
export function notifyAiConfigChanged(): void {
  listeners.forEach((listener) => {
    try {
      listener();
    } catch (error) {
      // An error in one subscriber must not affect the others (and must not crash the settings page's save flow)
      console.error('AI 配置变更通知处理失败:', error);
    }
  });
}

/** Subscribes to config changes, returning an unsubscribe function (used directly as a useEffect cleanup). */
export function subscribeAiConfig(listener: Listener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
