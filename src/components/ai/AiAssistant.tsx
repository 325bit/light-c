// ============================================================================
// AI assistant: floating ball + chat panel
//
// A standalone feature added horizontally, mounted in the App's global layer, and **does not participate in any module's logic**.
// The panel is a "non-modal floating layer + collapsible" to ensure it does not block the module's main page (explicitly requested by Evan).
//
// Session state (messages, sending, errors) all lives in this layer; the panel only handles display and input.
// ============================================================================

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AnimatePresence, motion } from 'framer-motion';
import {
  aiChatStream,
  appendAiSessionMessage,
  clearAiModuleSession,
  getAiConfig,
  listenAiStream,
  loadAiSessionRange,
  localDateKey,
  readAiAttachment,
  saveAiAttachment,
  saveAiProfile,
  shiftDateKey,
} from '../../api/ai';
import type { AiSessionMessage } from '../../api/ai';
import type { AiChatMessage, AiChatReply, AiPublicConfig, AiToolCall } from '../../types/ai';
import { AiChatPanel } from './AiChatPanel';
import { AiMarkIcon } from './AiMarkIcon';
import { buildRequestMessages, getModuleDisplayName } from './aiContext';
import { getModuleDataSummary } from './moduleDataRegistry';
import { getModuleActions } from './moduleActionRegistry';
import type { AiDeleteOutcome, AiDeleteTargetSummary, ModuleActions } from './moduleActionRegistry';
import { formatReadPage } from './useAiModuleBridge';
import { clearAiAsk, useAiAskRequest } from './aiAsk';
import { notifyAiConfigChanged, subscribeAiConfig } from './aiConfigSignal';
import { useToast } from '../Toast';
import type { AttachedImage } from './imageAttach';

/** Only keep the most recent few rounds of conversation: older content is handled by the rolling summary. */
const MAX_HISTORY_MESSAGES = 16;

/** How many history entries to load from disk at once (when opening the panel); "Load earlier" adds the same amount each time. */
const HISTORY_PAGE_SIZE = 40;

/**
 * Maximum number of history entries to keep in memory.
 *
 * How much is loaded only affects the **UI** and the input to the rolling summary — only the most recent 16 actually enter the context.
 * But stuffing them in endlessly is pointless and long lists get sluggish, so stop at the cap.
 */
const MAX_LOADED_HISTORY = 200;

/** Maximum number of dropped messages to include in the summary (loading earlier can drop hundreds, can't include them all). */
const SUMMARY_MAX_MESSAGES = 40;

/** Hard character cap for the summary, as a fallback to prevent the summary from filling the context. */
const SUMMARY_MAX_CHARS = 1200;

/**
 * Maximum number of request rounds per question.
 *
 * One round = one request, and **only the first round declares tools** — see the detailed explanation in `handleSend`
 *(subsequent requests with `tools` would be required by reasoning mode to return that round's chain of thought, which we can't produce).
 * So in practice it's two rounds: "look up once → answer based on it"; keeping 3 leaves room for fallback retries.
 */
const MAX_TOOL_ROUNDS = 3;

/**
 * Frontend watchdog: if no delta arrives within this long, consider it stuck.
 *
 * **Slightly longer** than the backend's idle timeout (30 seconds), letting the backend error first —
 * the backend can give a specific reason, while the frontend can only give a generic hint.
 * This frontend layer exists as a fallback "when the backend also fails": events not delivered, process suspended,
 * or the backend logic gets broken someday. **Without it, the button would stay on "thinking" forever and the user would have to restart.**
 */
const STREAM_IDLE_MS = 45_000;

interface AiAssistantProps {
  /** Current module id (visibleModuleId in App); used to let the assistant know "where the user is now" */
  activeModuleId: string | null;
}

export function AiAssistant({ activeModuleId }: AiAssistantProps) {
  const { t } = useTranslation('ai');
  /** For quick questions, when no provider is configured the panel won't open, so it can only be conveyed via toast */
  const { showToast } = useToast();
  const [isOpen, setIsOpen] = useState(false);
  /**
   * **Module-isolated** conversation history: one per module, switching modules swaps sets, and missing ones are empty.
   * Using "module id → message array" instead of a single array keeps "things said in junk cleanup"
   * from showing up in the old driver's panel (Evan reported this cross-talk).
   */
  const [historyByModule, setHistoryByModule] = useState<Record<string, AiChatMessage[]>>({});
  /** Whether a module still has earlier history not yet loaded (the UI shows "Load earlier" accordingly) */
  const [hasEarlierByModule, setHasEarlierByModule] = useState<Record<string, boolean>>({});
  const [isLoadingEarlier, setIsLoadingEarlier] = useState(false);
  /** Second confirmation for "Clear conversation" (clearing is **all dates**, can't be done with a single click) */
  const [showClearConfirm, setShowClearConfirm] = useState(false);
  const [isSending, setIsSending] = useState(false);
  const [errorMessage, setErrorMessage] = useState('');
  // The "answer being generated" assembled from streaming deltas. It doesn't enter history — only the complete text returned by the command is stored,
  // so even if a few events are missed, the final content won't be incomplete.
  const [streamingText, setStreamingText] = useState('');

  // Use a ref to store the "request in progress" flag to avoid concurrent requests from rapid clicks
  const sendingRef = useRef(false);
  // Modules whose history has already been requested from disk, to avoid repeatedly reading files when switching back and forth
  const loadedModulesRef = useRef<Set<string>>(new Set());

  /**
   * One session id per module: multiple rounds of the same conversation share one value, replaced after "Clear conversation".
   * Lives only in memory — it's not a secret and shouldn't persist across restarts (a restart is a new session anyway).
   */
  const sessionIdByModuleRef = useRef<Record<string, string>>({});
  const getSessionId = useCallback((moduleId: string) => {
    const existing = sessionIdByModuleRef.current[moduleId];
    if (existing) return existing;
    // Official examples use the ses_ prefix; uuid uses native Webview capability, no new dependency
    const generated = `ses_${crypto.randomUUID()}`;
    sessionIdByModuleRef.current[moduleId] = generated;
    return generated;
  }, []);

  const moduleName = useMemo(() => getModuleDisplayName(activeModuleId), [activeModuleId]);

  /**
   * The currently used model and provider, for **display** only.
   *
   * Must be visible to the user: when something goes wrong (poor answer, error), the first question is always "what model is this".
   * Read once each time the panel opens — the user may have changed config in settings while the panel was open.
   */
  /**
   * Current config (the whole thing). Store the whole config rather than a display string because the panel also needs to determine:
   * whether an API key is configured, which models are available, and which is selected — when not configured, disable input and send.
   */
  const [aiConfig, setAiConfig] = useState<AiPublicConfig | null>(null);

  const reloadAiConfig = useCallback(async () => {
    try {
      setAiConfig(await getAiConfig());
    } catch (error) {
      console.error('读取当前 AI 配置失败:', error);
    }
  }, []);

  /**
   * The panel is portaled to document.body (see AiChatPanel), fully decoupled from module cards —
   * The old workaround of "disabling module blur while the panel is open" is no longer needed, and the corresponding CSS rule has been removed.
   */

  // Read once on mount: otherwise, before the panel has ever been opened, aiConfig stays null,
  // canChat is always false — quick questions would be misjudged as "no provider configured".
  // Closing the entry and reopening remounts this component, clearing state; this is the direct fix for that bug.
  useEffect(() => {
    void reloadAiConfig();
  }, [reloadAiConfig]);

  // Subscribe to "AI config changed": broadcast after settings save / profile switch / profile delete / data dir change.
  // Refreshing only when the panel opens isn't enough: clicking a quick question right after configuring a provider doesn't go through the panel,
  // so the check still uses the old value and gets wrongly blocked (Evan reported this).
  useEffect(() => subscribeAiConfig(() => void reloadAiConfig()), [reloadAiConfig]);

  // Refresh when opening the panel: the user may have just changed config in settings
  useEffect(() => {
    if (!isOpen) return;
    void reloadAiConfig();
  }, [isOpen, reloadAiConfig]);

  /** Current profile: both display and checks are based on it. */
  const activeProfile = useMemo(
    () =>
      aiConfig
        ? aiConfig.profiles.find((item) => item.id === aiConfig.activeProfileId) ?? aiConfig.profiles[0]
        : undefined,
    [aiConfig],
  );

  /**
   * Whether chatting is possible: **just configure a provider**, no API key required anymore.
   * Local models (Ollama / LM Studio / vLLM) don't need authentication anyway; using hasApiKey would wrongly
   * block them as "no provider configured"; a missing key for cloud models will get a clear 401 on the first request.
   */
  const canChat = Boolean(activeProfile?.providerId);

  /** Switch model in place: write back to the current profile, effective on the next message. */
  const handleSwitchModel = useCallback(
    async (model: string) => {
      if (!activeProfile || !aiConfig) return;
      try {
        const next = await saveAiProfile({
          id: activeProfile.id,
          name: activeProfile.name,
          providerId: activeProfile.providerId,
          baseUrl: activeProfile.baseUrl,
          model,
          apiKey: '', // empty = don't change the saved API key
          // Carry it back as-is: the user configured this in settings, switching models shouldn't clear it
          reasoningEffort: activeProfile.reasoningEffort,
        });
        setAiConfig(next);
        // The settings page may be open, holding a snapshot from mount time. Without broadcasting, when the user later clicks
        // "Save" in settings, it would write back the old model over the just-switched value — appearing as "model switch failed".
        notifyAiConfigChanged();
      } catch (error) {
        // Write-back failure must be visible: the dropdown looks switched but it didn't take effect
        setErrorMessage(t('switchModelFailed', { message: String(error) }));
      }
    },
    [activeProfile, aiConfig, t],
  );
  const currentHistory = historyByModule[activeModuleId ?? ''] ?? [];

  /** Write a message to the module's history (in memory). Also add `at`: used to insert date separators after cross-day loading. */
  const pushMessage = useCallback((moduleId: string, message: AiChatMessage) => {
    const stamped = message.at === undefined ? { ...message, at: Date.now() } : message;
    setHistoryByModule((previous) => ({
      ...previous,
      [moduleId]: [...(previous[moduleId] ?? []), stamped],
    }));
  }, []);

  /**
   * When first entering a module, read that day's history back from disk.
   * Write/read failures are only logged to console — conversation history is an auxiliary feature and shouldn't block chatting itself.
   */
  useEffect(() => {
    const moduleId = activeModuleId ?? '';
    if (!moduleId || loadedModulesRef.current.has(moduleId)) return;
    loadedModulesRef.current.add(moduleId);

    void (async () => {
      try {
        const page = await loadAiSessionRange(moduleId, localDateKey(), HISTORY_PAGE_SIZE);
        setHasEarlierByModule((previous) => ({ ...previous, [moduleId]: page.hasMore }));
        if (page.messages.length === 0) return;
        const restored = await restoreMessages(page.messages);
        setHistoryByModule((previous) => ({ ...previous, [moduleId]: restored }));
      } catch (error) {
        console.error('读取 AI 会话历史失败:', error);
      }
    })();
  }, [activeModuleId]);

  /**
   * Pending delete proposal. **It's not a history message**: the card carries callbacks and isn't serializable,
   * so it's only shown as temporary state at the end of the message list, disappearing after confirm or cancel.
   */
  const [pendingDelete, setPendingDelete] = useState<{
    moduleId: string;
    ids: string[];
    items: AiDeleteTargetSummary[];
  } | null>(null);
  /** Most recent delete result, likewise only shown in this session */
  const [deleteResult, setDeleteResult] = useState<AiDeleteOutcome | null>(null);
  const [isDeleting, setIsDeleting] = useState(false);

  /**
   * Ref for the current module id.
   *
   * When async callbacks (streaming deltas, delete results, request failures) land, the activeModuleId in the closure is the value **at initiation time**,
   * so determining "whether the user is still in that module" can only rely on the ref. When not in that module, don't write to the UI —
   * otherwise answers, errors, and delete results from module A would show up in module B's panel.
   */
  const activeModuleIdRef = useRef(activeModuleId);
  activeModuleIdRef.current = activeModuleId;

  // On module switch, clear leftover proposals, result cards, error hints, and streaming text from the previous module.
  // Otherwise: a delete proposal generated in junk cleanup would **still sit on the panel** after switching to the old driver,
  // and the user clicks confirm only to find "current module has no delete capability" — looks like the old driver can trigger deletion, but it's cross-talk.
  // Same for streaming text: it's temporary content for "this round" and shouldn't appear in B if switching from A mid-generation.
  useEffect(() => {
    setPendingDelete(null);
    setDeleteResult(null);
    // Error hints are likewise "from the previous module": not clearing them would leak into other modules' dialogs
    setErrorMessage('');
    setStreamingText('');
  }, [activeModuleId]);

  /**
   * Push a message and **persist it at the same time**: tool execution results must also enter history, otherwise they'd be gone after switching away and back.
   *
   * `extra` carries three things, the first two being hard requirements of the server:
   *   - `reasoningContent`: reasoning mode's chain of thought, must be returned verbatim in subsequent requests or you get a 400;
   *   - `localToolOutput`: this one is a locally generated tool result, treated as user-side content when sent;
   *   - `producedByProfile`: which profile produced it — after switching providers, this chain of thought is no longer returned.
   */
  const pushAndPersist = useCallback(
    (
      moduleId: string,
      content: string,
      extra?: {
        reasoningContent?: string;
        localToolOutput?: boolean;
        producedByProfile?: string;
        /** Actual model name returned by the backend, for display only */
        model?: string;
      },
    ) => {
      const message = {
        role: 'assistant' as const,
        content,
        reasoningContent: extra?.reasoningContent,
        localToolOutput: extra?.localToolOutput,
        producedByProfile: extra?.producedByProfile,
        model: extra?.model,
      };
      pushMessage(moduleId, message);
      void appendAiSessionMessage(moduleId, localDateKey(), {
        ...message,
        at: Date.now(),
      }).catch((error) => console.error('写入 AI 会话历史失败:', error));
    },
    [pushMessage],
  );

  /** When the user clicks "Confirm delete": only then actually call the module's delete endpoint. */
  const handleConfirmDelete = useCallback(async () => {
    if (!pendingDelete) return;
    const { moduleId, ids } = pendingDelete;
    const actions = getModuleActions(moduleId);
    if (!actions?.deleteItems) {
      // Reaching here means the module the proposal belongs to is no longer available (likely switched modules or module unloaded).
      // Tell the user clearly the reason and what to do, don't just let the card disappear silently.
      setDeleteResult({
        deletedCount: 0,
        failures: [{ name: '', reason: t('delete.wrongModule') }],
      });
      setPendingDelete(null);
      return;
    }
    setIsDeleting(true);
    try {
      const outcome = await actions.deleteItems(ids);
      // The user may have switched away during deletion: show the result card only to the module at initiation time,
      // while the text record still goes into that module's history (switching back shows the deletion really happened).
      if (activeModuleIdRef.current === moduleId) setDeleteResult(outcome);
      // The result card is temporary (carries callbacks, not serializable), so additionally persist a text record:
      // otherwise the user would switch away and back and think "this deletion never happened".
      pushAndPersist(
        moduleId,
        outcome.failures.length > 0
          ? t('delete.persistedWithFailures', {
              count: outcome.deletedCount,
              failed: outcome.failures.length,
            })
          : t('delete.persisted', { count: outcome.deletedCount }),
      );
    } catch (error) {
      // Failure must also be clearly explained to the user, rather than the card vanishing as if nothing happened
      if (activeModuleIdRef.current === moduleId) {
        setDeleteResult({ deletedCount: 0, failures: [{ name: '', reason: String(error) }] });
      } else {
        console.error('执行删除失败（用户已切走模块）:', error);
      }
    } finally {
      setIsDeleting(false);
      setPendingDelete(null);
    }
  }, [pendingDelete, pushAndPersist, t]);

  /**
   * Execute the model's requested tool calls, writing results into the conversation as a **local message**.
   *
   * Result text is generated here, not narrated by the model — what was done and how many items were affected
   * are all based on local facts, avoiding "AI says it deleted but actually didn't".
   *
   * `allowedTools` is **the tools declared to the model this round**: must be checked again before execution.
   */
  const runToolCalls = useCallback(
    (
      moduleId: string,
      calls: AiToolCall[],
      allowedTools: string[],
    ): { texts: string[]; changedData: boolean } => {
      const actions = getModuleActions(moduleId);
      // Results both go into the conversation and are collected to return to the caller —
      // after read-only tools run, we must **ask the model another round with these results** so it can answer.
      // If we only show tool output without giving the model another chance, the user sees raw data with no conclusion.
      const texts: string[] = [];
      /** Whether data was actually changed (selection / delete proposal). Read-only tools won't set this to true. */
      let changedData = false;
      const report = (text: string) => {
        texts.push(text);
        // Mark as "local tool result": converted to user-side content when sending requests —
        // otherwise it's an assistant message without reasoning_content, and requests with tools would be rejected with 400.
        pushAndPersist(moduleId, text, { localToolOutput: true });
      };

      for (const call of calls) {
        // ⚠️ The second gate only at the "declaration" layer isn't enough: the model can write a call directly in its text,
        // and the backend's inline fallback will parse it out — that path never goes through tool declaration.
        // If it lands on select_items, that's **directly changing selection with no confirmation step**, so it must be blocked again before execution.
        if (!allowedTools.includes(call.name)) continue;
        const rawIds = (call.arguments as { ids?: unknown }).ids;
        const ids = Array.isArray(rawIds)
          ? rawIds.filter((id): id is string => typeof id === 'string')
          : [];

        // Read-only reads: **put results into the conversation first**, so the model can reference them next round.
        // These two tools change nothing, so they're not subject to the "must be explicitly requested by user" restriction.
        if (call.name === 'read_items') {
          // ⚠️ Report failure reasons **separately**. These three cases have completely different debugging directions:
          // not registered / not implemented / empty data. Merging into one "no content read",
          // would make the problem look like "feature not implemented", wasting a lot of debugging effort.
          if (!actions) {
            report(t('tool.moduleNotReady', { module: moduleId }));
            continue;
          }
          if (!actions.readItems) {
            report(t('tool.readNotProvided', { module: moduleId }));
            continue;
          }
          const args = call.arguments as { target_id?: unknown; offset?: unknown; limit?: unknown };
          const targetId = typeof args.target_id === 'string' ? args.target_id : '';
          const offset = Math.max(0, Number(args.offset) || 0);
          // Cap at 100: any larger would eat up the context, and "seeing more" doesn't mean "answering better"
          const limit = Math.min(100, Math.max(1, Number(args.limit) || 30));
          const page = actions.readItems(targetId, offset, limit);
          if (!page || page.lines.length === 0) {
            report(t(emptyReadMessageKey(moduleId), { module: moduleId }));
            continue;
          }
          report(formatReadPage(page, offset));
          continue;
        }

        if (call.name === 'read_item_detail') {
          if (!actions) {
            report(t('tool.moduleNotReady', { module: moduleId }));
            continue;
          }
          if (!actions.readItemDetail) {
            report(t('tool.detailNotProvided', { module: moduleId }));
            continue;
          }
          const args = call.arguments as { item_id?: unknown };
          const itemId = typeof args.item_id === 'string' ? args.item_id : '';
          const detail = actions.readItemDetail(itemId);
          report(detail || t(emptyReadMessageKey(moduleId), { module: moduleId }));
          continue;
        }

        // Always resolve ids against current module data: between proposal and execution the user may have changed selection or the module may have rescanned,
        // drop unresolved items directly, never blindly execute against stale ids.
        const resolved = actions?.resolveTargets?.(ids) ?? [];

        if (call.name === 'select_items') {
          // ⚠️ When the module hasn't registered a "select" action, it **must be blocked here**: `?.()` is a silent no-op,
          // while the code below would still report "selected N items as requested" — that would be **falsely claiming execution** (the project's biggest taboo).
          // Normal paths can't reach here (unregistered means undeclared, and the execution whitelist would block too); this is the last fallback,
          // to guarantee "if we say selected, it really was selected".
          if (!actions?.selectItems) {
            report(t('tool.selectNotProvided', { module: moduleId }));
            continue;
          }
          if (resolved.length === 0) {
            report(t('tool.selectNotFound'));
            continue;
          }
          actions.selectItems(resolved.map((target) => target.id));
          changedData = true;
          report(
            t('tool.selected', {
              count: resolved.length,
              names: resolved.map((target) => target.label).join('、'),
            }),
          );
          continue;
        }

        if (call.name === 'propose_delete') {
          // Same here: when the module has no "delete" capability, **don't show the card** — it can't be executed anyway,
          // clicking confirm would just get "current module doesn't provide delete capability" (that error should be stated before confirming).
          if (!actions?.deleteItems) {
            report(t('tool.deleteNotProvided', { module: moduleId }));
            continue;
          }
          if (resolved.length === 0) {
            report(t('tool.selectNotFound'));
            continue;
          }
          // **Do not execute**, only show the confirmation card — the AI can only propose, execution power lies with the user
          setDeleteResult(null);
          setPendingDelete({
            moduleId,
            ids: resolved.map((target) => target.id),
            items: resolved,
          });
          changedData = true;
        }
      }

      return { texts, changedData };
    },
    [pushAndPersist, t],
  );

  const handleSend = useCallback(
    async (
      text: string,
      images: AttachedImage[] = [],
      historyOverride?: AiChatMessage[],
      // A question started from "the row the user expanded themselves": data is in the question, no lookup this round (see aiAsk.ts)
      skipLookup = false,
    ) => {
      if (sendingRef.current) return;
      sendingRef.current = true;
      // Remember whether to "look up data" this round: must be reused when retrying the same question (see handleRetry)
      lastSkipLookupRef.current = skipLookup;

      // Capture the module and history "at send time": the user may switch modules during generation,
      // this round's results must go back to the module at initiation, not the "current" module (otherwise cross-talk again).
      const moduleId = activeModuleId ?? '';
      // The whole tool loop shares one session id: changing it midway would split one conversation into two sessions
      const sessionId = getSessionId(moduleId);
      // Retry explicitly passes the truncated history: setState hasn't taken effect, so synchronously reading the closure still gets the **pre-truncation** old value,
      // so the question that was "retried away" would be sent again (UI truncated, request not).
      const history = historyOverride ?? historyByModule[moduleId] ?? [];
      setIsSending(true);
      setErrorMessage('');
      setStreamingText('');

      // Persisted and session history records store **only text and placeholders**: image base64 is often hundreds of KB,
      // writing it to jsonl would bloat the file quickly and be useless on reload.
      const persistedText =
        images.length > 0 ? `${text}\n[已附加 ${images.length} 张图片]`.trim() : text;
      // Keep thumbnails in the UI (using the freshly compressed data URL), so this entry carries images
      pushMessage(moduleId, {
        role: 'user',
        content: persistedText,
        images: images.map((image) => ({ mediaType: image.mediaType, dataBase64: image.base64 })),
      });
      // The division between persistence and UI is handled by persistUserTurn: UI keeps thumbnails, disk keeps only filenames
      void persistUserTurn(moduleId, persistedText, images);

      // A separate stream id per request: events are globally broadcast, so it's used to filter out others' deltas.
      const streamId = `s${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
      const request = buildTurnMessages(
        history,
        text,
        images,
        moduleId,
        activeProfile?.id ?? '',
        skipLookup,
      );
      // Tool declaration, request, watchdog, and cleanup each have their own function (see "Steps in the send flow" at the end of the file)
      const tools = buildAllowedTools(getModuleActions(moduleId), request, skipLookup);

      try {
        // One round = one request. When the model asks for a read-only tool, include the results and **ask another round** so it can answer.
        // Without this round, the user sees raw data with no conclusion ("looked it up but said nothing").
        let turnMessages = request;
        // Fingerprint of the last round's tool results: the model may repeatedly ask for **the same data** (tested: kept asking for the same top-level list),
        // from round two on we can recognize it, don't burn rounds on this kind of spinning in place.
        let previousToolFingerprint = '';

        for (let round = 0; round < MAX_TOOL_ROUNDS; round += 1) {
          const isLastRound = round === MAX_TOOL_ROUNDS - 1;

          // ⚠️ **Only the first round declares tools.**
          // DeepSeek specifies: when a request has `tools`, the `reasoning_content` of the assistant round that initiated the tool call
          // must be returned in full in all subsequent requests — but we don't use the standard tool_calls protocol (tool results are self-assembled
          // text), **we can't produce that round's chain of thought at all**, so any subsequent request still carrying `tools` will inevitably get a 400.
          // That rule only applies to "requests with tools"; without them it doesn't apply at all.
          // Besides, round two's job is precisely "answer using the just-fetched data", no more tool calls needed.
          let roundTools = round === 0 ? tools : [];

          // Final round: no more tools, and **explicitly require "answer now"** —
          // if no data was read, answer with your own knowledge, don't come empty-handed (the prompt already allows this; here we point out the timing).
          if (isLastRound) {
            turnMessages = [
              ...turnMessages,
              { role: 'user' as const, content: t('tool.answerFromKnowledge') },
            ];
          }

          const onDelta = (delta: string) => {
            // If the user has switched away, don't keep appending text to the panel: this round's full text will still end up in
            // the history of the module at initiation, visible when switching back.
            if (activeModuleIdRef.current !== moduleId) return;
            setStreamingText((previous) => previous + delta);
          };
          // A separate stream id per round: last round's late deltas shouldn't be counted in this one
          const callModel = (attemptTools: string[]) =>
            requestStreamReply({
              streamId: `${streamId}-r${round}-${attemptTools.length}`,
              messages: turnMessages,
              tools: attemptTools,
              sessionId,
              timeoutMessage: t('streamTimeout'),
              onDelta,
            });

          let reply: AiChatReply;
          try {
            reply = await callModel(roundTools);
          } catch (error) {
            // Fallback: if we still hit the 400 requiring reasoning_content return, drop tools and retry once
            //(requests without tools aren't subject to that rule). Retry only once, no nesting.
            if (roundTools.length === 0 || !/reasoning_content/i.test(String(error))) throw error;
            console.warn('服务端要求回传思维链，本轮改为不带工具重试:', error);
            roundTools = [];
            reply = await callModel(roundTools);
          }

          const calls = reply.toolCalls ?? [];
          if (calls.length === 0) {
            // No tool calls = this round is the answer. Record the profile that produced it:
            // after switching providers, this chain of thought can no longer be returned (see toRequestMessage).
            pushAndPersist(moduleId, reply.content, {
              reasoningContent: reply.reasoningContent,
              producedByProfile: activeProfile?.id,
              model: reply.model,
            });
            return;
          }

          // Filter again before execution using the "executable whitelist for this round":
          // read-only tools always count, data-changing tools must be declared this round (see executableTools)
          const { texts, changedData } = runToolCalls(moduleId, calls, executableTools(roundTools));

          // Data-changing actions stop here: selection already changed, delete proposal awaiting user confirmation,
          // sending another round automatically would only make the model repeat "selected" again, pointless.
          if (changedData) return;
          // Round limit reached: no more rounds to feed, if there's text use it to wrap up
          if (isLastRound) {
            // Empty text = the model kept looking and never gave a conclusion by the end. **Must say something**,
            // otherwise the user stares at raw data, unsure whether it broke or didn't finish.
            if (reply.content.trim()) {
              pushAndPersist(moduleId, reply.content, {
                reasoningContent: reply.reasoningContent,
                producedByProfile: activeProfile?.id,
                model: reply.model,
              });
            } else {
              pushAndPersist(moduleId, t('tool.noConclusion'));
            }
            return;
          }
          // No progress (tools gave nothing / same result twice in a row):
          // **Don't feed more data, and don't discard it either** — explicitly tell it to answer with its own knowledge.
          // The prompt already allows this; here we just tell it the timing that "it's time to wrap up".
          const fingerprint = texts.join('\n');
          const madeNoProgress =
            texts.length === 0 || (fingerprint.length > 0 && fingerprint === previousToolFingerprint);
          if (madeNoProgress) {
            turnMessages = [
              ...turnMessages,
              { role: 'user' as const, content: t('tool.answerFromKnowledge') },
            ];
            continue;
          }
          previousToolFingerprint = fingerprint;

          // Read-only tool results are carried into context as assistant messages — this is exactly the step the old "next round" was missing.
          // ⚠️ Must **combine into one**, not one per result: Anthropic requires user/assistant alternation,
          // two consecutive assistant messages would get the whole request rejected.
          turnMessages = [
            ...turnMessages,
            { role: 'assistant' as const, content: texts.join('\n\n') },
          ];
          // Next round's text displays from scratch: tool output is already in the conversation, no need to concatenate
          setStreamingText('');
        }
      } catch (error) {
        // On failure, keep the user input in history for easy retry; show the error separately, don't mix into conversation text.
        // If the user has switched away, don't surface the error onto other modules (this round's record stays in the module at initiation).
        if (activeModuleIdRef.current === moduleId) {
          setErrorMessage(String(error));
        } else {
          console.error('AI 请求失败（用户已切走模块）:', error);
        }
      } finally {
        // Listener and watchdog cleanup is done inside requestStreamReply; here we only reset this round's state
        setStreamingText('');
        sendingRef.current = false;
        setIsSending(false);
      }
    },
    [activeModuleId, activeProfile, historyByModule, pushMessage, pushAndPersist, runToolCalls, getSessionId, t],
  );

  /**
   * Retry a specific user message: truncate the history to just before it, then resend.
   * Used when network/API errors occur, or when you're unsatisfied with the answer.
   *
   * Session files on disk are append-only, never rewritten — retry content gets appended again,
   * the old attempt stays in the file but is no longer shown in the UI (avoid touching file write logic just for "retry").
   */
  const handleRetry = useCallback(
    (index: number) => {
      const moduleId = activeModuleId ?? '';
      const history = historyByModule[moduleId] ?? [];
      const target = history[index];
      if (!target || target.role !== 'user') return;
      const truncated = history.slice(0, index);
      setHistoryByModule((previous) => ({ ...previous, [moduleId]: truncated }));
      // Explicitly pass the truncated array: otherwise handleSend still reads the old full history,
      // and the retried question would be sent again along with its following old reply (UI truncated, request not).
      // When retrying the **last** message, reuse the "no data lookup" rule (see lastSkipLookupRef).
      const skipLookup = index === history.length - 1 && lastSkipLookupRef.current;
      void handleSend(target.content, [], truncated, skipLookup);
    },
    [activeModuleId, historyByModule, handleSend],
  );

  /** Retry the most recent question: find the last user message from the back, using the same path as single-message retry. */
  const handleRetryLast = useCallback(() => {
    const moduleId = activeModuleId ?? '';
    const history = historyByModule[moduleId] ?? [];
    for (let index = history.length - 1; index >= 0; index -= 1) {
      if (history[index].role === 'user') {
        handleRetry(index);
        return;
      }
    }
  }, [activeModuleId, historyByModule, handleRetry]);

  /**
   * Load earlier messages.
   *
   * Only affects the **UI** and the input to the rolling summary — only the most recent 16 (plus the capped summary) actually enter context,
   * so however much is loaded won't blow up the context. Stop at `MAX_LOADED_HISTORY` and the button disappears accordingly.
   */
  const handleLoadEarlier = useCallback(async () => {
    const moduleId = activeModuleId ?? '';
    if (!moduleId || isLoadingEarlier) return;
    const current = historyByModule[moduleId] ?? [];
    if (current.length >= MAX_LOADED_HISTORY) {
      setHasEarlierByModule((previous) => ({ ...previous, [moduleId]: false }));
      return;
    }

    setIsLoadingEarlier(true);
    try {
      // Take the boundary as the **day before** the date of the current earliest message: earlier ones on the same day won't be read twice
      const oldestAt = current.find((message) => typeof message.at === 'number')?.at;
      const boundary = oldestAt ? localDateKey(new Date(oldestAt)) : localDateKey();
      const page = await loadAiSessionRange(moduleId, shiftDateKey(boundary, -1), HISTORY_PAGE_SIZE);
      const restored = await restoreMessages(page.messages);
      setHistoryByModule((previous) => {
        const merged = [...restored, ...(previous[moduleId] ?? [])];
        // If over the in-memory cap, drop the oldest (the rest will enter the prompt via the rolling summary)
        return { ...previous, [moduleId]: merged.slice(-MAX_LOADED_HISTORY) };
      });
      setHasEarlierByModule((previous) => ({ ...previous, [moduleId]: page.hasMore }));
    } catch (error) {
      console.error('加载更早的对话失败:', error);
    } finally {
      setIsLoadingEarlier(false);
    }
  }, [activeModuleId, historyByModule, isLoadingEarlier]);

  /** Clicking "Clear" only shows confirmation — clearing is **all dates**, can't be executed with a single click. */
  const handleClearRequest = useCallback(() => {
    setShowClearConfirm(true);
  }, []);

  /**
   * After confirmation, actually clear: **both memory and disk, and all dates for that module**.
   *
   * Deleting only that day's file isn't enough (history now loads across days) —
   * otherwise you'd get "cleared, but reopening the panel brings back yesterday's".
   */
  const handleClearConfirmed = useCallback(() => {
    const moduleId = activeModuleId ?? '';
    setShowClearConfirm(false);
    // Clearing conversation = starting a new session: the session id must change, otherwise the new conversation still sticks to the previous routing/cache
    delete sessionIdByModuleRef.current[moduleId];
    setHistoryByModule((previous) => ({ ...previous, [moduleId]: [] }));
    setHasEarlierByModule((previous) => ({ ...previous, [moduleId]: false }));
    setErrorMessage('');
    void clearAiModuleSession(moduleId).catch((error) =>
      console.error('清空 AI 会话历史失败:', error),
    );
  }, [activeModuleId]);

  const handleToggle = useCallback(() => {
    setIsOpen((previous) => !previous);
  }, []);

  // The module's "Ask AI assistant" button → open the panel and send directly, no need for the user to type again.
  // Deduplicate by id: clicking the same question twice must trigger twice, but not be triggered repeatedly by a recreated handleSend.
  const askRequest = useAiAskRequest();
  const lastAskIdRef = useRef(0);
  /**
   * Whether the last send was "no need to query data" (a question initiated from the detail row in the popup).
   *
   * Retry must reuse the same rule, otherwise retrying the same question becomes different behavior (look up first, hit a wall, then answer).
   * Only remember the most recent one: retrying older history messages doesn't carry this flag — that's rare, and worst case is one extra lookup.
   */
  const lastSkipLookupRef = useRef(false);
  useEffect(() => {
    if (!askRequest || askRequest.id === lastAskIdRef.current) return;

    // aiConfig === null means **not yet known** (not read back), not "no provider configured".
    // Don't consume this request here: trigger a load, and once config comes back this effect will rerun
    // (aiConfig is in dependencies), and only then can we judge.
    if (!aiConfig) {
      void reloadAiConfig();
      return;
    }

    lastAskIdRef.current = askRequest.id;

    // ⚠️ When no provider is configured, **do nothing**: no panel, no persistence, no request.
    // Quick questions are clicked from a module; popping an empty panel then erroring makes the user think the question was sent;
    // worse, that sentence would be written into session history (it's persisted as soon as the panel opens).
    // Here we only show a hint (panel doesn't open, so only toast can convey it) and clear this request —
    // otherwise it would "auto-resend" after the user configures a provider, another kind of surprise.
    if (!canChat) {
      showToast({
        type: 'warning',
        title: t('needConfigPlaceholder'),
        description: t('needConfigHint'),
      });
      clearAiAsk();
      return;
    }

    setIsOpen(true);
    void handleSend(askRequest.text, [], undefined, askRequest.skipLookup);
    clearAiAsk();
  }, [askRequest, aiConfig, reloadAiConfig, handleSend, canChat, showToast, t]);

  return (
    /* AnimatePresence handles the "exit animation": when React unmounts a node it plays exit fully before removing.
       Both child nodes need stable keys, otherwise switching would treat them as the same node and the animation wouldn't trigger. */
    <AnimatePresence mode="wait">
      {isOpen ? (
        <AiChatPanel
          key="panel"
          messages={currentHistory}
          streamingText={streamingText}
          pendingDelete={pendingDelete?.items ?? null}
          deleteResult={deleteResult}
          isDeleting={isDeleting}
          onConfirmDelete={() => void handleConfirmDelete()}
          onCancelDelete={() => setPendingDelete(null)}
          onRetry={handleRetry}
          isSending={isSending}
          errorMessage={errorMessage}
          onRetryLast={handleRetryLast}
          moduleName={moduleName}
          modelOptions={activeProfile?.models ?? []}
          activeModel={activeProfile?.model ?? ''}
          canChat={canChat}
          onSwitchModel={handleSwitchModel}
          onSend={(text, attached) => void handleSend(text, attached)}
          onCollapse={handleToggle}
          onClear={handleClearRequest}
          hasEarlier={(hasEarlierByModule[activeModuleId ?? ''] ?? false) && currentHistory.length > 0}
          isLoadingEarlier={isLoadingEarlier}
          onLoadEarlier={() => void handleLoadEarlier()}
          isClearConfirmOpen={showClearConfirm}
          onConfirmClear={handleClearConfirmed}
          onCancelClear={() => setShowClearConfirm(false)}
        />
      ) : (
        <motion.button
          key="ball"
          type="button"
          initial={{ opacity: 0, scale: 0.6 }}
          animate={{ opacity: 1, scale: 1 }}
          exit={{ opacity: 0, scale: 0.6 }}
          transition={{ duration: 0.2, ease: [0.22, 1, 0.36, 1] }}
          /* Hover/press scaling is left to framer-motion (no CSS hover:scale-*, otherwise the two would conflict) */
          whileHover={{ scale: 1.06 }}
          whileTap={{ scale: 0.94 }}
          onClick={handleToggle}
          title={t('ballTitle')}
          aria-label={t('ballTitle')}
          /* Stick to the right edge, **vertically centered**.
             It used to sit in the bottom-right corner (above the back-to-top button), which overlapped each row's right-side action icons:
             blocking the view and hard to click — the whole column of icons sits there.
             After vertical centering, this area on the right is always empty.

             ⚠️ Centering deliberately **doesn't use transform** (neither Tailwind's -translate-y-1/2
             nor framer-motion's style={{ y }}):
             this element is also being scaled by framer-motion, which writes an inline transform to the same element —
             two places writing transform will inevitably conflict, and which wins depends on execution order.
             Switching to pure box-model centering ("top and bottom both 0 + margin auto") is completely unrelated to transform,
             so no matter how the outside animates, it won't be misaligned. */
          className="glass-item fixed right-5 top-0 bottom-0 my-auto z-40 flex h-11 w-11 items-center justify-center rounded-full border border-[var(--border-color)] bg-[var(--bg-elevated)] text-[var(--brand-green)] shadow-lg"
        >
          {/* The little bubble above: an icon alone doesn't tell new users what this is.
              The animation is split into two layers — the outer handles "popping out" (runs once), the inner handles slow floating (looping).
              Combining into one layer has a pitfall: when keyframes loop they jump back to the starting offset, looking like a twitch.

              Centering also **doesn't use transform**: the outer is a motion element (does scale/y animation),
              same reason as the ball, leaving it to "left and right both 0 + margin auto + w-fit" is safer. */}
          <motion.span
            aria-hidden="true"
            initial={{ opacity: 0, scale: 0.7, y: 6 }}
            animate={{ opacity: 1, scale: 1, y: 0 }}
            transition={{ delay: 0.25, type: 'spring', stiffness: 320, damping: 20 }}
            className="pointer-events-none absolute bottom-full left-0 right-0 mx-auto mb-2 w-fit"
          >
            <motion.span
              animate={{ y: [0, -3, 0] }}
              transition={{ duration: 2.4, repeat: Infinity, ease: 'easeInOut' }}
              className="relative block rounded-lg border border-[var(--brand-green-20)] bg-[var(--bg-elevated)] px-2 py-0.5 text-[0.72rem] font-medium leading-4 text-[var(--brand-green)] shadow-sm"
            >
              {t('ballBadge')}
              {/* The little pointer toward the ball: a square rotated 45°, keeping only bottom and right borders,
                  forming a downward-pointing triangle, with border color matching the bubble (the two-layer triangle approach is too verbose).
                  This layer isn't a motion element, so using translate for centering here won't be overridden. */}
              <span className="absolute left-1/2 top-full h-1.5 w-1.5 -translate-x-1/2 -translate-y-[0.21rem] rotate-45 border-b border-r border-[var(--brand-green-20)] bg-[var(--bg-elevated)]" />
            </motion.span>
          </motion.span>
          {/* The icon needs to be one size larger than elsewhere: this SVG's viewBox is 24 units, but the shape itself only occupies
              the middle band (4 units trimmed from each side, 6 from the bottom), so at h-5 the rendered
              visible robot is under 14px, looking empty in a 44px circle.
              Bumping to h-7 makes its actual size comparable to neighboring icons. */}
          <AiMarkIcon className="h-7 w-7" />
        </motion.button>
      )}
    </AnimatePresence>
  );
}

// ============================================================================
// Steps in the send flow
//
// Extracted not because it "looks long", but because these steps each have independent constraints (who can declare tools, what must
// come before what, what to do on failure), and mixing them in one function makes changing one part easily break another.
// ============================================================================

/**
 * Assemble this round's messages sent to the model: history + current question (**images live only in this round's request**, not in history state).
 * Fetch the data summary fresh — the user may have just scanned or just selected; even a one-second cache would give wrong answers.
 */
function buildTurnMessages(
  history: AiChatMessage[],
  text: string,
  images: AttachedImage[],
  moduleId: string,
  activeProfileId: string,
  skipLookup = false,
): AiChatMessage[] {
  return buildRequestMessages(
    [
      ...history,
      {
        role: 'user',
        content: text,
        images:
          images.length > 0
            ? images.map((image) => ({ mediaType: image.mediaType, dataBase64: image.base64 }))
            : undefined,
      },
    ],
    moduleId,
    getModuleDataSummary(moduleId),
    activeProfileId,
    skipLookup,
  );
}

/** Read-only tool names. They change nothing, so they're **allowed at any time** (see executableTools). */
const READ_ONLY_TOOLS = ['read_items', 'read_item_detail'];

/** Whether the module currently has scanned data (empty summary = not yet). Used to explain the reason for an "empty result". */
function moduleHasData(moduleId: string): boolean {
  return Boolean(getModuleDataSummary(moduleId)?.trim());
}

/**
 * When a read-only tool gets an empty result, what exactly should be said.
 *
 * ⚠️ These two cases have **completely different** next steps: module not scanned → tell user to scan; no match → ask about a different item.
 * Merging into one sentence "no content read: this item may no longer exist, or there's no scanned data yet",
 * leaves the user not knowing what to do, and the debugger has to guess backwards — this trap was actually hit twice
 *(model files in the AI model space, driver packages in the old driver; neither time was it "this item doesn't exist").
 */
function emptyReadMessageKey(moduleId: string): 'tool.readEmpty' | 'tool.moduleNoData' {
  return moduleHasData(moduleId) ? 'tool.readEmpty' : 'tool.moduleNoData';
}

/**
 * The "executable whitelist" for this round.
 *
 * - **Read-only tools always count**: later rounds declare no tools, but the model may still use its own markers (DSML)
 *   to request a read — that's what we observed. Not recognizing it makes the round a silent no-op where "nothing happened".
 * - **Data-changing tools must be declared this round**, otherwise never execute — this bottom line doesn't move.
 */
function executableTools(declared: string[]): string[] {
  return [...new Set([...declared, ...READ_ONLY_TOOLS])];
}

/**
 * Tools allowed to be declared to the model in this round.
 *
 * Read-only tools: give them if the module registered them — they change nothing, and when the user asks "what's here" a direct read is exactly right.
 * Data-changing tools: **give them only when this looks like an action request**. This is a hard constraint at the request layer,
 * far more reliable than repeated instructions in the prompt: prompts can only "urge", withholding tools means "cannot".
 */
function buildAllowedTools(
  actions: ModuleActions | null,
  messages: AiChatMessage[],
  skipLookup = false,
): string[] {
  // The user clicked a row **after expanding a level** (data is in the question and not within the module's read capability):
  // declare no tools at all — otherwise the model would look up first, hit a wall, then blurt out "no content read" before answering.
  if (skipLookup) return [];

  const tools: string[] = [];
  if (actions?.readItems) tools.push('read_items');
  if (actions?.readItemDetail) tools.push('read_item_detail');
  if (looksLikeActionRequest(messages)) {
    if (actions?.selectItems) tools.push('select_items');
    if (actions?.deleteItems) tools.push('propose_delete');
  }
  return tools;
}

/**
 * Write this round's user message to disk: images are first saved as files, and the session record keeps only filenames.
 * Failures are only logged to console — session history is an auxiliary feature and must not block this round's conversation due to a write failure.
 */
async function persistUserTurn(
  moduleId: string,
  content: string,
  images: AttachedImage[],
): Promise<void> {  try {
    const names = await Promise.all(
      images.map((image) => saveAiAttachment(image.base64, image.mediaType)),
    );
    await appendAiSessionMessage(moduleId, localDateKey(), {
      role: 'user',
      content,
      images: names,
      at: Date.now(),
    });
  } catch (error) {
    console.error('写入 AI 会话历史失败:', error);
  }
}

/**
 * Send a streaming request, hand deltas to `onDelta`, and finally return the model's reply.
 *
 * Three error-prone things are concentrated here, hence a separate function:
 *   1. **Attach the listener before sending the request** — otherwise the first deltas may arrive before the listener is attached;
 *   2. **Watchdog** — if no deltas for 45 seconds, consider it stuck and end this round. It's a fallback "when the backend also fails"
 *      (events not delivered, process suspended); without it the button would stay on "thinking" forever;
 *   3. **Always unbind the listener and clear the timer, success or failure** — one conversation leaving a permanent listener
 *      would cause the next deltas to be accumulated twice.
 */
async function requestStreamReply(options: {
  streamId: string;
  /** Already a complete request message (including system) */
  messages: AiChatMessage[];
  tools: string[];
  /** Session id for this conversation, reused throughout the whole tool loop */
  sessionId: string;
  /** Hint shown when the watchdog triggers; passed in by the caller so it follows the UI language */
  timeoutMessage: string;
  onDelta: (text: string) => void;
}): Promise<AiChatReply> {
  const { streamId, messages, tools, sessionId, timeoutMessage, onDelta } = options;
  let unlisten: (() => void) | null = null;
  let watchdogHandle: number | undefined;
  let resetWatchdog: () => void = () => {};

  try {
    // Attach the listener before sending, otherwise the first deltas may arrive before it's attached
    unlisten = await listenAiStream((delta) => {
      if (delta.streamId !== streamId) return;
      resetWatchdog(); // a delta means it's still alive, reset the timer
      onDelta(delta.text);
    });

    // Race against the watchdog: if stuck, the latter rejects first, ending this round and releasing the button
    return await Promise.race([
      aiChatStream({ messages: trimHistory(messages), tools, sessionId }, streamId),
      new Promise<never>((_, reject) => {
        const arm = () => {
          watchdogHandle = window.setTimeout(
            () => reject(new Error(timeoutMessage)),
            STREAM_IDLE_MS,
          );
        };
        resetWatchdog = () => {
          window.clearTimeout(watchdogHandle);
          arm();
        };
        arm();
      }),
    ]);
  } finally {
    // Clear the watchdog: not clearing it is harmless (rejecting an already-settled Promise is a no-op),
    // but it would leave a meaningless timer behind
    window.clearTimeout(watchdogHandle);
    // Must cancel the listener: otherwise a conversation leaves a permanent listener and the next deltas get accumulated twice
    unlisten?.();
  }
}

/**
 * Expressions that look like "asking to take action".
 *
 * Only on a match do we declare **data-changing** tools to the model — this is a hard constraint at the request layer,
 * far more reliable than repeated instructions in the prompt: prompts can only "urge", withholding tools means "cannot".
 *
 * ⚠️ Deliberately made **conservative**: prefer false negatives (should act but didn't, assistant asks a follow-up),
 * over false positives (acted when it shouldn't, popping up a delete card).
 * ⚠️ Only include "action" words, **do not include response words like "yes", "ok", "confirm"** —
 * they can appear in any casual chat, which would leave this gate permanently open.
 */
const ACTION_HINTS = [
  // Simplified Chinese ("clean up" and "clear" share forms, listed once)
  '删', '清理', '清掉', '清空', '清除', '勾选', '勾上', '选中', '移除',
  // Traditional Chinese
  '刪', '勾選', '選取',
  // Japanese
  '削除', '消去', '選択', '選んで',
  // English
  'delete', 'remove', 'clean up', 'clear out', 'erase', 'wipe', 'uninstall',
];

/**
 * If action-type words appear in the user's last two messages, treat it as "requesting an operation".
 *
 * ⚠️ Only scan **user** messages: earlier we also scanned the assistant's own replies, so text like "these files aren't recommended for deletion"
 * would re-declare the delete tool — the model's own wording would open the gate in reverse.
 * Looking at two messages is to accommodate "assistant asks, user replies".
 * Known trade-off: pure replies (just "yes"/"ok", no verb) won't match; in that case
 * the assistant will ask again, and the feature won't fail silently.
 */
function looksLikeActionRequest(messages: AiChatMessage[]): boolean {
  const recent = messages
    .filter((message) => message.role === 'user')
    .slice(-2)
    .map((message) => message.content.toLowerCase());
  return recent.some((text) => ACTION_HINTS.some((hint) => text.includes(hint)));
}

/**
 * Compress the dropped portion of history into a summary.
 *
 * **Done locally, no extra model call** — an extra request is slow and costly, and this content isn't complex anyway:
 * What matters is only "what was asked and what was executed". Keeping these two, early context won't disappear completely.
 */
function summarizeDropped(dropped: AiChatMessage[]): string {
  // ⚠️ Only take the **most recent** N: after loading earlier, hundreds may have been dropped,
  // putting them all in the summary would refill the context — the summary's job is "don't forget entirely", not "remember everything".
  const recent = dropped.length > SUMMARY_MAX_MESSAGES ? dropped.slice(-SUMMARY_MAX_MESSAGES) : dropped;
  const omitted = dropped.length - recent.length;

  const asked = recent
    .filter((message) => message.role === 'user')
    .map((message) => message.content.split('\n')[0].slice(0, 40))
    .filter(Boolean);
  // Tool execution results are also assistant messages, but their content starts with a conclusion sentence, so putting them on a separate line is more useful
  const acted = recent
    .filter((message) => message.role === 'assistant' && message.content.startsWith('已'))
    .map((message) => message.content.slice(0, 60));

  const parts: string[] = [];
  if (asked.length > 0) parts.push(`用户此前问过：${asked.join('；')}`);
  if (acted.length > 0) parts.push(`期间执行过：${acted.join('；')}`);
  if (parts.length === 0) return '';
  const head =
    omitted > 0
      ? `【更早的对话摘要（另有 ${omitted} 条更早的未纳入）】`
      : '【更早的对话摘要（原始消息已从上下文移除）】';
  const text = `${head}${parts.join('。')}。需要细节时请让用户重新说明，不要凭这段摘要编造具体数字。`;
  // Hard cap fallback: no matter how the trimming strategy changes later, the summary entering the prompt won't exceed this length
  return text.length > SUMMARY_MAX_CHARS ? `${text.slice(0, SUMMARY_MAX_CHARS)}…（摘要已截断）` : text;
}

/**
 * Trim history: always keep system messages, truncate the rest to "the most recent N".
 * The goal is to keep context cost down first — the dropped part is compressed into a summary by summarizeDropped,
 * so early context won't disappear completely.
 */
function trimHistory(messages: AiChatMessage[]): AiChatMessage[] {
  const systems = messages.filter((message) => message.role === 'system');
  const rest = messages.filter((message) => message.role !== 'system');
  const cutIndex = Math.max(0, rest.length - MAX_HISTORY_MESSAGES);
  const kept = rest.slice(cutIndex);
  // Don't just discard the dropped part: compress it into a summary and carry it as extra system content.
  // (Both protocol builders extract system messages, so this is the right place.)
  const droppedSummary = cutIndex > 0 ? summarizeDropped(rest.slice(0, cutIndex)) : '';

  // **Keep images only in the most recent message**.
  // Images enter requests as base64, and the conversation is stateless with the whole history resent each round —
  // a pile of historical screenshots would make each request several MB. Old images help little with "answering the current question",
  // keeping the most recent one is enough (users usually ask about the image they just sent).
  let seenLatestImage = false;
  const trimmed = [...kept].reverse().map((message) => {
    if (!message.images || message.images.length === 0) return message;
    if (!seenLatestImage) {
      seenLatestImage = true;
      return message;
    }
    return { ...message, images: undefined };
  });

  const summaryMessage: AiChatMessage[] = droppedSummary
    ? [{ role: 'system', content: droppedSummary }]
    : [];
  return [...systems, ...summaryMessage, ...trimmed.reverse()];
}

/**
 * Restore session records from disk into UI messages (used both when opening the panel and for "Load earlier").
 *
 * Attachments must be read back as data URLs: this way reopening the panel still shows the images sent at the time.
 * The three fields — chain of thought, "local tool result" flag, and "produced by which profile" — must also be restored —
 * not returning the first gets a 400 from the server, the second determines whether this counts as assistant or user-side content,
 * and the third determines whether it should still be sent after switching providers.
 * `at` is for the UI only (inserting date separators), request construction won't include it.
 */
async function restoreMessages(stored: AiSessionMessage[]): Promise<AiChatMessage[]> {
  return Promise.all(
    stored.map(async (item) => {
      const carried = {
        at: item.at,
        reasoningContent: item.reasoningContent,
        localToolOutput: item.localToolOutput,
        producedByProfile: item.producedByProfile,
      };
      const names = item.images ?? [];
      if (names.length === 0) {
        return { role: item.role, content: item.content, ...carried };
      }
      const dataUrls = await Promise.all(names.map((name) => readAiAttachment(name)));
      return {
        role: item.role,
        content: item.content,
        images: dataUrls
          .filter((url) => url.length > 0)
          .map((url) => ({ mediaType: 'image/jpeg', dataBase64: url.split(',')[1] ?? '' })),
        ...carried,
      };
    }),
  );
}
