// ============================================================================
// AI chat panel
//
// Interaction requirements (Evan explicitly mentioned): collapsible, and **must not block any module's main page**.
// So this is deliberately: non-modal floating layer + collapsible into a thin strip + doesn't intercept underlying interaction.
// Only handles display and input; session state and requests all live in AiAssistant.
// ============================================================================

import { useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';
import { motion } from 'framer-motion';
import { AlertCircle, Check, Copy, Eraser, History, ImagePlus, Loader2, Minus, RefreshCw, Send, User, X } from 'lucide-react';
import { Select } from '../ui/Select';
import { ConfirmDialog } from '../ConfirmDialog';
import i18n from '../../i18n';
import type { AiChatMessage } from '../../types/ai';
import { AiDeleteRequestCard, type AiDeleteRequestItem } from './AiDeleteRequestCard';
import { AiDeleteResultCard } from './AiDeleteResultCard';
import { AiMarkIcon } from './AiMarkIcon';
import type { AiDeleteOutcome } from './moduleActionRegistry';
import { collectImageFiles, compressImage, type AttachedImage } from './imageAttach';
import { renderMarkdown } from './renderMarkdown';
import { usePanelSize } from './usePanelSize';
import { BetaBadge } from '../BetaBadge';
import { formatSize } from '../../utils/format';
import { errorText } from '../../api/errors';

/**
 * Display text for a message's date (used for the date separator after cross-day loading).
 *
 * Use `Intl.DateTimeFormat` to follow the UI language, instead of hand-assembling "month/day" (each language writes it differently).
 * Old data without a timestamp returns an empty string — then just skip the separator; better than inserting a wrong one.
 */
function formatDayLabel(at: number | undefined): string {
  if (typeof at !== 'number' || !Number.isFinite(at) || at <= 0) return '';
  const date = new Date(at);
  if (Number.isNaN(date.getTime())) return '';
  const sameYear = date.getFullYear() === new Date().getFullYear();
  return new Intl.DateTimeFormat(i18n.language, {
    month: 'short',
    day: 'numeric',
    ...(sameYear ? {} : { year: 'numeric' }),
  }).format(date);
}

interface AiChatPanelProps {
  messages: AiChatMessage[];
  /** Content currently streaming in; empty means this round hasn't started producing text yet */
  streamingText: string;
  /** Pending delete proposal; when non-empty, show the confirmation card below the message list */
  pendingDelete: AiDeleteRequestItem[] | null;
  /** Most recent delete result */
  deleteResult: AiDeleteOutcome | null;
  /** Delete in progress: both buttons are disabled and spin */
  isDeleting: boolean;
  onConfirmDelete: () => void;
  onCancelDelete: () => void;
  /** Retry a user message (on network/API errors): the component only reports the index; truncation and resend are decided by the upper layer */
  onRetry: (index: number) => void;
  /** Retry the most recent question — used by the button next to the error hint, so users don't have to scroll up */
  onRetryLast: () => void;
  isSending: boolean;
  errorMessage: string;
  /** Current module display name, used only in the title so users know "which module's assistant" they're talking to */
  moduleName: string;
  /** Models available for the current profile, for in-place switching next to the input */
  modelOptions: string[];
  activeModel: string;
  onSwitchModel: (model: string) => void;
  /** Whether an API key is configured. When not configured, no input or send allowed — otherwise users type a whole message only to get an error */
  canChat: boolean;
  /** When images is empty, it's a text-only send */
  onSend: (text: string, images: AttachedImage[]) => void;
  onCollapse: () => void;
  /** Click "Clear": only **shows the second confirmation**, doesn't clear directly */
  onClear: () => void;
  /** Whether there is earlier history not yet loaded (top "Load earlier" button) */
  hasEarlier: boolean;
  isLoadingEarlier: boolean;
  onLoadEarlier: () => void;
  /** Toggle and callbacks for the "Clear conversation" second confirmation (clears **all dates**, must be confirmed) */
  isClearConfirmOpen: boolean;
  onConfirmClear: () => void;
  onCancelClear: () => void;
}

export function AiChatPanel({
  messages,
  streamingText,
  pendingDelete,
  deleteResult,
  isDeleting,
  onConfirmDelete,
  onCancelDelete,
  onRetry,
  isSending,
  errorMessage,
  onRetryLast,
  moduleName,
  modelOptions,
  activeModel,
  onSwitchModel,
  canChat,
  onSend,
  onCollapse,
  onClear,
  hasEarlier,
  isLoadingEarlier,
  onLoadEarlier,
  isClearConfirmOpen,
  onConfirmClear,
  onCancelClear,
}: AiChatPanelProps) {
  const { t } = useTranslation('ai');
  const [draft, setDraft] = useState('');
  const listRef = useRef<HTMLDivElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  /** Textarea node: its height must grow with content, so we need to measure its scrollHeight */
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  /**
   * Textarea grows with its content.
   *
   * Order matters: **reset to zero first, then set height from scrollHeight** — setting scrollHeight directly makes the height only grow, never shrink
   * when deleting text (scrollHeight doesn't shrink on its own).
   *
   * Lower/upper bounds are left to CSS min-h / max-h; here we only provide the "natural height":
   * future tweaks to bounds don't need to touch this logic. ⚠️ max-h additionally stacks 30vh —
   * in a very short window it must not squeeze the message list above out of existence (same approach as the dialog sizing).
   */
  useEffect(() => {
    const element = textareaRef.current;
    if (!element) return;
    element.style.height = 'auto';
    element.style.height = `${element.scrollHeight}px`;
  }, [draft]);
  /** Images pending send (compressed). Cleared once sent — images don't enter session history. */
  const [attachments, setAttachments] = useState<AttachedImage[]>([]);
  /** Attachment-related hints (compression failure, too many images, etc.), shown at the bottom-left of the input area */
  const [attachError, setAttachError] = useState('');
  /** Image currently being previewed enlarged (data URL); empty means no preview */
  const [previewUrl, setPreviewUrl] = useState('');

  /** Max images per message: more doesn't help the model, only makes each request heavier. */
  const MAX_ATTACHMENTS = 4;

  const handleAddImages = async (files: File[]) => {
    if (files.length === 0) return;
    const room = MAX_ATTACHMENTS - attachments.length;
    if (room <= 0) {
      setAttachError(t('attachTooMany', { count: MAX_ATTACHMENTS }));
      return;
    }
    const accepted = files.slice(0, room);
    if (files.length > room) setAttachError(t('attachTooMany', { count: MAX_ATTACHMENTS }));

    const compressed: AttachedImage[] = [];
    for (const file of accepted) {
      try {
        compressed.push(await compressImage(file));
      } catch (error) {
        // Handle one by one: one failure shouldn't discard the rest
        setAttachError(errorText(error));
      }
    }
    if (compressed.length > 0) {
      setAttachments((previous) => [...previous, ...compressed]);
      if (files.length <= room) setAttachError('');
    }
  };
  /** Index of the message just copied: icon briefly becomes a check mark, giving feedback that "it really copied" */
  const [copiedIndex, setCopiedIndex] = useState<number | null>(null);
  /** Timer handle for copy feedback: must be cleared on unmount, otherwise the callback still runs after the panel collapses, and rapid clicks stack several */
  const copyResetHandleRef = useRef<number | undefined>(undefined);

  useEffect(() => () => window.clearTimeout(copyResetHandleRef.current), []);

  const handleCopy = async (content: string, index: number) => {
    try {
      await navigator.clipboard.writeText(content);
      setCopiedIndex(index);
      // Clear the previous one first: two rapid clicks shouldn't leave two callbacks
      window.clearTimeout(copyResetHandleRef.current);
      copyResetHandleRef.current = window.setTimeout(
        () => setCopiedIndex((previous) => (previous === index ? null : previous)),
        1500,
      );
    } catch (error) {
      console.error('复制对话内容失败:', error);
    }
  };

  /**
   * Whether to stick to the bottom.
   *
   * Why this switch is needed: during streaming **every delta triggers following** — unconditional scrolling to bottom means
   * when the user wants to read back earlier text, every frame yanks them back down, making review impossible. So only follow "when they were already at the bottom";
   * scrolling up stops following, and scrolling back down themselves (or sending a new message) resumes it.
   */
  const stickToBottomRef = useRef(true);

  /** Within this many pixels of the bottom counts as "still at bottom". Font scaling and float accumulation can differ by a few pixels. */
  const FOLLOW_BOTTOM_THRESHOLD_PX = 24;

  const handleListScroll = () => {
    const node = listRef.current;
    if (!node) return;
    // scrollHeight - clientHeight - scrollTop is "how far from the bottom"
    const overflow = node.scrollHeight - node.clientHeight;
    // When content is shorter than the screen there's no "stuck to bottom"; restore following directly —
    // otherwise after clearing the conversation it would stay stuck in "not following" and new answers wouldn't auto-scroll.
    stickToBottomRef.current =
      overflow <= FOLLOW_BOTTOM_THRESHOLD_PX ||
      overflow - node.scrollTop <= FOLLOW_BOTTOM_THRESHOLD_PX;
  };

  // Follow to bottom when messages, streaming deltas, or send state change; use scrollTop instead of scrollIntoView,
  // to avoid scrolling the whole page along with it.
  //
  // ⚠️ streamingText must be in the deps: the body is appended piece by piece, while messages doesn't change until the whole round ends —
  // without it, there'd be no scrolling at all during streaming, only one jump to bottom when the answer settles.
  useEffect(() => {
    const node = listRef.current;
    if (!node || !stickToBottomRef.current) return;
    node.scrollTop = node.scrollHeight;
  }, [messages, streamingText, isSending]);

  const handleSubmit = () => {
    const text = draft.trim();
    // Allow sending images without text: users often take a screenshot and ask "can these be deleted"
    if ((!text && attachments.length === 0) || isSending || !canChat) return;
    // User actively sent a message: resume following no matter where they had scrolled, otherwise their own message lands off-screen
    stickToBottomRef.current = true;
    onSend(text, attachments);
    setDraft('');
    setAttachments([]);
    setAttachError('');
  };

  const visibleMessages = messages.filter((message) => message.role !== 'system');

  // Size is draggable and persisted; when untouched, use the em + viewport calculation from CSS
  const { sizeStyle, hasCustomSize, startResize, resetSize } = usePanelSize();

  /**
   * Portal the whole block to `document.body`.
   *
   * Why: the panel is `position: fixed`, so DOM position doesn't affect layout; but if left in the App root node,
   * it shares the same compositing context as module cards — WebView2 paints the card's `backdrop-filter` layer
   * on top of the panel ("ghost artifact"). Moving under body fully decouples the two.
   *
   * ⚠️ No styles/context lost: the theme class is on `<html>`, so CSS variables still inherit;
   * React portals preserve context too. The preview layer moves along (it belongs at the very top anyway).
   */
  return createPortal(
    <>
    {/* Enlarged preview. Deliberately placed **outside** the panel: the panel uses framer-motion (with transform),
        and once there's a transform, inner position:fixed elements get clipped by its overflow-hidden. */}
    {previewUrl ? (
      <div
        className="fixed inset-0 z-[60] flex items-center justify-center bg-black/70 glass-backdrop p-6"
        onClick={() => setPreviewUrl('')}
        role="button"
        tabIndex={-1}
      >
        <img src={previewUrl} alt="" className="max-h-full max-w-full rounded-xl shadow-2xl" />
      </div>
    ) : null}
    {/* All sizes use em + vw/vh, no hard-coded pixels: em follows the global font-size setting,
        and the cap is clamped by the viewport, so a smaller window narrows it automatically.
        ⚠️ The minus sign in calc must have spaces — in Tailwind write it as underscores,
        otherwise the whole property is dropped by the browser (hit before).
        ⚠️ The two lines above were originally JS comments; ever since return was wrapped in a Fragment,
        they became JSX sibling nodes; a bare JS comment in JSX gets **rendered as text onto the UI**.
        So it must be in brace-wrapped form here. (The comment also can't contain its own closing token,
        or this comment would end early.) */}
    <motion.div
      /* The App root sets select-none (to stop the UI being text-selected like a web page),
         but conversation content **must be selectable and copyable** — explicitly override that here. */
      initial={{ opacity: 0, y: 12, scale: 0.96 }}
      animate={{ opacity: 1, y: 0, scale: 1 }}
      exit={{ opacity: 0, y: 8, scale: 0.97 }}
      transition={{ duration: 0.24, ease: [0.22, 1, 0.36, 1] }}
      style={sizeStyle}
      className="ai-panel fixed right-5 bottom-5 z-40 flex w-[min(28em,calc(100vw_-_2.5em))] max-h-[min(38em,calc(100vh_-_8em))] min-h-[22em] flex-col overflow-hidden rounded-2xl border border-[var(--border-color)] bg-[var(--bg-elevated)] shadow-lg select-text"
    >
      {/* Top-left drag handle: the panel is fixed at bottom-right, so the "top-left" is the intuitive resize corner.
          Double-click restores the default size (back to following font size and window auto-fit). */}
      <div
        onPointerDown={startResize}
        onDoubleClick={resetSize}
        title={hasCustomSize ? t('resizeHintCustom') : t('resizeHint')}
        className="group absolute left-0 top-0 z-10 h-5 w-5 cursor-nwse-resize"
      >
        {/* Minimal resize marker: a corner bracket, not attention-grabbing but signals it's draggable.
            Color uses brand green rather than --text-muted — the latter is dark gray and looks like a black smudge on a light panel,
            and clashes with the panel's green tone. Darkens on hover to hint "you can drag here".
            (The sidebar's drag handle uses brand green too, for consistency.) */}
        <div className="pointer-events-none absolute left-1 top-1 h-2.5 w-2.5 border-l border-t border-[var(--brand-green)] opacity-40 transition-opacity group-hover:opacity-90" />
      </div>

      {/* ⚠️ Must **not** attach glass-block here: "remove border and background" is for "child blocks inside a card",
          applying it to a floating layer's header makes the header fully transparent, with module content clearly showing through (looks like a ghost).
          The header now shares the panel's background layer, and the separator is kept. */}
      <header className="flex shrink-0 items-center gap-2 border-b border-[var(--border-color)] px-3 py-2">
        <AiMarkIcon className="h-3.5 w-3.5 shrink-0 text-[var(--brand-green)]" />
        <span className="min-w-0 flex-1 truncate text-xs font-medium text-[var(--text-primary)]">
          {t('panelTitle')}
          {moduleName ? <span className="text-[var(--text-muted)]"> · {moduleName}</span> : null}
        </span>
        <BetaBadge />
        <button
          type="button"
          onClick={onClear}
          title={t('clear')}
          aria-label={t('clear')}
          className="shrink-0 rounded-lg p-1 text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
        >
          <Eraser className="h-3.5 w-3.5" />
        </button>
        <button
          type="button"
          onClick={onCollapse}
          title={t('collapse')}
          aria-label={t('collapse')}
          className="shrink-0 rounded-lg p-1 text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
        >
          <Minus className="h-3.5 w-3.5" />
        </button>
      </header>

      <div ref={listRef} onScroll={handleListScroll} className="min-h-0 flex-1 space-y-2.5 overflow-y-auto px-3 py-3">
        {/* Load earlier: history is read across days; the button disappears at the end or at the in-memory cap */}
        {hasEarlier && visibleMessages.length > 0 ? (
          <button
            type="button"
            onClick={onLoadEarlier}
            disabled={isLoadingEarlier}
            className="mx-auto flex items-center gap-1 rounded-full border border-[var(--border-color)] px-2.5 py-1 text-[0.79rem] text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] disabled:opacity-60"
          >
            {isLoadingEarlier ? (
              <Loader2 className="h-3 w-3 animate-spin" />
            ) : (
              <History className="h-3 w-3" />
            )}
            {isLoadingEarlier ? t('history.loadingEarlier') : t('history.loadEarlier')}
          </button>
        ) : null}
        {visibleMessages.length === 0 ? (
          <p className="text-sm leading-relaxed text-[var(--text-muted)]">{t('emptyHint')}</p>
        ) : (
          visibleMessages.map((message, index) => {
            const isUser = message.role === 'user';
            // Insert a date separator after cross-day loading: otherwise you can't tell if this chunk is yesterday or today
            const dayLabel = formatDayLabel(message.at);
            const previousDayLabel = index > 0 ? formatDayLabel(visibleMessages[index - 1]?.at) : '';
            return (
              <div
                key={index}
                className={`group flex flex-col gap-0.5 ${isUser ? 'items-end' : 'items-start'}`}
              >
                {dayLabel && dayLabel !== previousDayLabel ? (
                  /* The first message's date has no horizontal rule: it sits right against the panel header's bottom border,
                     two lines so close look like a duplicated border. Keep only the text and center it. */
                  <div className="flex w-full items-center justify-center gap-2 py-1 text-[0.72rem] text-[var(--text-faint)]">
                    {index > 0 && <span className="h-px flex-1 bg-[var(--border-color)]" />}
                    <span className="shrink-0">{dayLabel}</span>
                    {index > 0 && <span className="h-px flex-1 bg-[var(--border-color)]" />}
                  </div>
                ) : null}
                <div style={{ userSelect: 'all' }} className={`flex items-start gap-2 ${isUser ? 'flex-row-reverse' : ''}`}>
                  <div
                    className={`mt-0.5 flex h-6 w-6 shrink-0 items-center justify-center rounded-full ${
                      isUser
                        ? 'bg-[var(--bg-hover)] text-[var(--text-muted)]'
                        : 'bg-[var(--brand-green-10)] text-[var(--brand-green)]'
                    }`}
                  >
                    {isUser ? <User className="h-3.5 w-3.5" /> : <AiMarkIcon className="h-3.5 w-3.5" />}
                  </div>
                  <div
                    className={`max-w-[80%] select-text whitespace-pre-wrap break-words rounded-xl px-2.5 py-1.5 text-sm leading-relaxed ${
                      isUser ? 'bg-[var(--brand-green)] text-white' : 'bg-[var(--ai-bubble-bg)] text-[var(--text-primary)]'
                    }`}
                  >
                    {/* User-sent images: thumbnail on top, text below, click to enlarge.
                        These are the images saved with the message, so they persist after reopening the panel. */}
                    {isUser && message.images && message.images.length > 0 ? (
                      <div className="mb-1.5 flex flex-wrap gap-1.5">
                        {message.images.map((image, imageIndex) => (
                          <button
                            key={imageIndex}
                            type="button"
                            onClick={() => setPreviewUrl(`data:${image.mediaType};base64,${image.dataBase64}`)}
                            title={t('previewImage')}
                            className="overflow-hidden rounded-lg ring-1 ring-inset ring-white/30"
                          >
                            <img
                              src={`data:${image.mediaType};base64,${image.dataBase64}`}
                              alt=""
                              className="h-16 w-16 object-cover transition-transform hover:scale-105"
                            />
                          </button>
                        ))}
                      </div>
                    ) : null}
                    {/* Assistant replies are Markdown and must be rendered (via React nodes, not inline HTML);
                        the user's own input is placed as plain text, unparsed — so hand-typed markers aren't eaten. */}
                    {isUser ? message.content : renderMarkdown(message.content)}
                  </div>
                </div>
                {/* The model actually used this round (truth returned by backend): always visible, not hidden behind hover like the action bar.
                    After switching models, comparing the before/after replies shows at a glance whether it took effect. */}
                {!isUser && message.model ? (
                  <span className="pl-8 text-xs text-[var(--text-faint)]">{message.model}</span>
                ) : null}
                {/* Action bar sits **below the bubble** (appears on hover): copy for every message, retry only for user messages.
                    Placing it above would crowd the avatar and doesn't match the conventions of this kind of UI. */}
                <div
                  className={`flex items-center gap-0.5 pl-8 pr-1 opacity-0 transition-opacity group-hover:opacity-100 ${
                    isUser ? 'justify-end self-end' : ''
                  }`}
                >
                  <button
                    type="button"
                    onClick={() => void handleCopy(message.content, index)}
                    title={t('copy')}
                    aria-label={t('copy')}
                    className="rounded-lg p-1 text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
                  >
                    {copiedIndex === index ? <Check className="h-3 w-3" /> : <Copy className="h-3 w-3" />}
                  </button>
                  {isUser ? (
                    <button
                      type="button"
                      onClick={() => onRetry(index)}
                      disabled={isSending}
                      title={t('retry')}
                      aria-label={t('retry')}
                      className="rounded-lg p-1 text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] disabled:cursor-not-allowed disabled:opacity-40"
                    >
                      <RefreshCw className="h-3 w-3" />
                    </button>
                  ) : null}
                </div>
              </div>
            );
          })
        )}

        {/* Answer being generated: shown as it arrives, so users see "it's writing" instead of waiting blindly */}
        {streamingText ? (
          <div className="flex items-start gap-2">
            <div className="mt-0.5 flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-[var(--brand-green-10)] text-[var(--brand-green)]">
              <AiMarkIcon className="h-3.5 w-3.5" />
            </div>
            {/* The streaming bubble must have the **same background** as the settled one:
                otherwise the bubble changes color the instant the answer finishes, which is jarring. */}
            <div className="max-w-[80%] whitespace-pre-wrap break-words rounded-xl bg-[var(--ai-bubble-bg)] px-2.5 py-1.5 text-sm leading-relaxed text-[var(--text-primary)]">
              {renderMarkdown(streamingText)}
              {/* Cursor: hints that output is still ongoing */}
              <span className="ml-0.5 inline-block h-3 w-1 translate-y-0.5 animate-pulse bg-[var(--brand-green)]" />
            </div>
          </div>
        ) : isSending ? (
          <div className="flex items-center gap-1.5 text-xs text-[var(--text-muted)]">
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
            {t('thinking')}
          </div>
        ) : null}
      </div>

      {/* Delete proposal: **do not execute, only confirm**. The AI can only propose; execution power lies with the user. */}
      {pendingDelete && pendingDelete.length > 0 ? (
        <div className="shrink-0 px-3 pb-2">
          <AiDeleteRequestCard
            title={t('delete.title')}
            items={pendingDelete}
            totalLabel={t('delete.total', { count: pendingDelete.length })}
            warning={t('delete.warning')}
            irreversibleLabel={t('delete.irreversible')}
            confirmLabel={t('delete.confirm')}
            cancelLabel={t('delete.cancel')}
            busy={isDeleting}
            onConfirm={onConfirmDelete}
            onCancel={onCancelDelete}
          />
        </div>
      ) : null}

      {/* Delete result: all numbers come from the backend, not narrated by the model */}
      {deleteResult ? (
        <div className="shrink-0 px-3 pb-2">
          <AiDeleteResultCard
            successLabel={
              deleteResult.freedBytes
                ? t('delete.doneWithSize', {
                    count: deleteResult.deletedCount,
                    size: formatSize(deleteResult.freedBytes),
                  })
                : t('delete.doneCount', { count: deleteResult.deletedCount })
            }
            failedLabel={
              deleteResult.failures.length > 0
                ? t('delete.failedCount', { count: deleteResult.failures.length })
                : undefined
            }
            failures={deleteResult.failures}
            detailsLabel={t('delete.details')}
            rebootNotice={deleteResult.needsReboot ? t('delete.needsReboot') : undefined}
          />
        </div>
      ) : null}

      {errorMessage ? (
        <div className="mx-3 mb-2 flex shrink-0 items-start gap-2 rounded-xl border border-[var(--color-danger)]/30 bg-[var(--color-danger)]/5 p-2 text-[0.79rem] text-[var(--color-danger)]">
          <AlertCircle className="mt-0.5 h-3 w-3 shrink-0" />
          {/* break-all cuts long English sentences from providers into unreadable pieces; use normal word-breaking + allow wrapping */}
          <span className="min-w-0 whitespace-pre-wrap break-words select-text">{errorMessage}</span>
          {/* The most likely thing to do on an error is "retry". Making users hover above to find the retry button
              would hide the most useful action. */}
          <button
            type="button"
            onClick={onRetryLast}
            className="ml-auto shrink-0 rounded-lg border border-[var(--color-danger)]/40 px-2 py-0.5 transition-colors hover:bg-[var(--color-danger)]/10"
          >
            {t('retry')}
          </button>
        </div>
      ) : null}

      {/* Input area: **one container overall**, text on top, bottom row for model and send — consistent with the ds harness.
          The model sits at bottom-right because it belongs to "what this message is sent with", part of the input itself. */}
      <div className="mx-2.5 mb-2.5 flex shrink-0 flex-col gap-1.5 rounded-2xl border border-[var(--border-color)] bg-[var(--bg-card)] p-2.5 transition-colors focus-within:border-[var(--brand-green)]">
        {/* Attached images: thumbnail + remove. Placed above the input so users see at a glance what they've attached */}
        {attachments.length > 0 ? (
          <div className="flex flex-wrap gap-1.5">
            {attachments.map((image, index) => (
              <div key={index} className="group relative">
                <img
                  src={image.previewUrl}
                  alt=""
                  className="h-14 w-14 rounded-lg border border-[var(--border-color)] object-cover"
                />
                <button
                  type="button"
                  onClick={() => setAttachments((previous) => previous.filter((_, i) => i !== index))}
                  title={t('attachRemove')}
                  aria-label={t('attachRemove')}
                  className="absolute -right-1 -top-1 rounded-full bg-[var(--bg-elevated)] p-0.5 text-[var(--text-muted)] shadow ring-1 ring-[var(--border-color)] transition-colors hover:text-[var(--color-danger)]"
                >
                  <X className="h-3 w-3" />
                </button>
              </div>
            ))}
          </div>
        ) : null}

        {/* ⚠️ Comments can't go between attributes (around rows={1} is attribute position, only attributes allowed).
            rows is only the initial value: real height is computed by the effect above from content.
            What really sets bounds is min-h / max-h — without max-h, a long prompt grows until it squeezes the message area out. */}
        <textarea
          ref={textareaRef}
          value={draft}
          disabled={!canChat}
          onChange={(event) => setDraft(event.target.value)}
          onPaste={(event) => {
            // Pasting a screenshot is the handiest entry: user just took a shot and hits Ctrl+V
            const files = collectImageFiles(event.clipboardData?.files);
            if (files.length > 0) {
              event.preventDefault();
              void handleAddImages(files);
            }
          }}
          onKeyDown={(event) => {
            // Enter sends, Shift+Enter adds a newline: long questions can be written across lines
            if (event.key === 'Enter' && !event.shiftKey) {
              event.preventDefault();
              handleSubmit();
            }
          }}
          rows={1}
          placeholder={canChat ? t('inputPlaceholder') : t('needConfigPlaceholder')}
          className="min-h-[3em] max-h-[min(13em,30vh)] w-full resize-none overflow-y-auto bg-transparent px-0.5 text-sm leading-relaxed text-[var(--text-primary)] outline-none placeholder:text-[var(--text-muted)] disabled:cursor-not-allowed disabled:opacity-60"
        />
        <div className="flex items-center justify-between gap-2">
          {/* Left side: attachment errors take priority (just happened), otherwise the not-configured guidance; stay quiet when both empty */}
          <span className="min-w-0 truncate text-[0.72rem] text-[var(--text-muted)]">
            {attachError || (canChat ? '' : t('needConfig'))}
          </span>
          <div className="flex shrink-0 items-center gap-1.5">
            {/* Image-picker entry. Placed bottom-left, consistent with the ds harness position */}
            <input
              ref={fileInputRef}
              type="file"
              accept="image/*"
              multiple
              className="hidden"
              onChange={(event) => {
                void handleAddImages(collectImageFiles(event.target.files));
                // Clear value: otherwise selecting the same image twice won't trigger change
                event.target.value = '';
              }}
            />
            <button
              type="button"
              onClick={() => fileInputRef.current?.click()}
              disabled={!canChat}
              title={t('attachImage')}
              aria-label={t('attachImage')}
              className="rounded-xl border border-[var(--border-color)] p-2 text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] disabled:cursor-not-allowed disabled:opacity-40"
            >
              <ImagePlus className="h-3.5 w-3.5" />
            </button>
            {modelOptions.length > 0 ? (
              <Select<string>
                value={activeModel}
                options={modelOptions.map((id) => ({ value: id, label: id }))}
                onChange={onSwitchModel}
                size="sm"
                widthClass="w-44"
              />
            ) : (
              <span className="truncate text-[0.79rem] text-[var(--text-muted)]">
                {activeModel || t('noModel')}
              </span>
            )}
            <button
              type="button"
              onClick={handleSubmit}
              disabled={!canChat || isSending || (!draft.trim() && attachments.length === 0)}
              title={t('send')}
              aria-label={t('send')}
              className="shrink-0 rounded-xl bg-[var(--brand-green)] p-2 text-white transition-colors hover:bg-[var(--brand-green-hover)] disabled:cursor-not-allowed disabled:opacity-50"
            >
              <Send className="h-3.5 w-3.5" />
            </button>
          </div>
        </div>
      </div>
    </motion.div>
      {/* Second confirmation for clearing conversation: clears **all dates** of history for this module, can't be done with one click */}
      <ConfirmDialog
        isOpen={isClearConfirmOpen}
        title={t('clearTitle')}
        description={t('clearBody')}
        confirmText={t('clearConfirm')}
        isDanger
        onConfirm={onConfirmClear}
        onCancel={onCancelClear}
      />
    </>,
    document.body,
  );
}
