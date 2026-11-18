// ============================================================================
// Quick-ask channel
//
// Each module's data rows will have an "Ask AI assistant" button; clicking it must: **open the assistant and send the question immediately**.
// But module components are deeply nested while the assistant is mounted at the App top level - passing context down layer by layer would pollute the whole chain.
//
// So use a tiny module-level subscription here: modules just "emit a question request",
// the assistant subscribes and opens the panel and sends it itself. Same idea as moduleDataRegistry:
// **The module does not need to know where the assistant is, and the assistant does not need to know what the button looks like.**
//
// The question text is decided by the module (usually reusing its existing "one-click search" keyword); no rewriting is done here.
// ============================================================================

import { useSyncExternalStore } from 'react';

export interface AiAskRequest {
  /** Auto-increment sequence: clicking twice with the same text must trigger the second time too */
  id: number;
  text: string;
  /**
   * This question **does not require a data lookup**.
   *
   * Used for "a row the user expanded to see themselves" (e.g., the disk change analysis "change details" modal):
   * the question carries that row itself, and these **file-level** details are outside the module's read capability -
   * having the model look it up would only waste a try and would first blurt out "no content read" before answering.
   * When set: declare no tools this round and state in the system prompt that "the data is in the question, answer with your own knowledge".
   */
  skipLookup: boolean;
}

let sequence = 0;
let pending: AiAskRequest | null = null;
const listeners = new Set<() => void>();

/** For useSyncExternalStore: return the subscribe function. */
function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Called by a module: request the assistant to open and send this question. */
export function requestAiAsk(text: string, options?: { skipLookup?: boolean }): void {
  const trimmed = text.trim();
  if (!trimmed) return;
  sequence += 1;
  pending = { id: sequence, text: trimmed, skipLookup: options?.skipLookup === true };
  listeners.forEach((listener) => listener());
}

/** Clear after the assistant handles it, to avoid resending on remount. */
export function clearAiAsk(): void {
  if (pending === null) return;
  pending = null;
  listeners.forEach((listener) => listener());
}

/** Subscribed by the assistant: get the pending question (null if none). */
export function useAiAskRequest(): AiAskRequest | null {
  return useSyncExternalStore(
    subscribe,
    () => pending,
    () => null,
  );
}
