// ============================================================================
// AI delete request confirmation card (standalone, reusable)
//
// Deliberately a **pure presentation component**: it knows nothing about AI or any module, only props.
//   - All copy comes from the caller (title, per-item detail, total, warning, buttons) → naturally supports multiple languages and changes
//   - Button actions come from the caller (onConfirm / onCancel) → which module performs the "delete" is decided by that module
//   - Doesn't send requests itself, pop up dialogs itself, or navigate to settings itself
//
// So the same card can be reused by any module: junk cleanup, old driver, big files...
// Each provides "what to delete and how", while the card only makes the scope clear and holds the confirmation threshold.
//
// ⚠️ Design constraint: **this is the last gate for write operations**. Therefore
//   1. The scope must be listed item by item, not just "we'll clean it up for you";
//   2. Irrecoverable items must be marked separately (like the Recycle Bin);
//   3. The confirm button uses the danger color, consistent with the module's original delete buttons.
// ============================================================================

import { AlertTriangle, Loader2, ShieldAlert } from 'lucide-react';

export interface AiDeleteRequestItem {
  /** Stable identifier, passed back verbatim in callbacks; the module uses it to locate the real target */
  id: string;
  /** Main copy: name */
  label: string;
  /** Secondary detail: size, item count, etc., formatted by the caller (this component does no unit conversion) */
  detail?: string;
  /** This item cannot be recovered after deletion: marked separately to remind the user it's not an ordinary cache */
  irreversible?: boolean;
}

export interface AiDeleteRequestCardProps {
  title: string;
  items: AiDeleteRequestItem[];
  /** Total description, e.g. "Total 616.14 MB / 1,235 items" */
  totalLabel: string;
  /** Risk warning; if omitted, this section isn't shown */
  warning?: string;
  /** Reason shown when disabled (e.g. "Enable it first in Settings - AI Assistant").
   * When present the confirm button is disabled — the card is **still shown** when the toggle is off, which is clearer than asking AI to pretend it can't.
   */
  disabledReason?: string;
  /** Copy for the "irrecoverable" marker. When there are irreversible items it **must** be passed by the caller (for i18n). */
  irreversibleLabel?: string;
  confirmLabel: string;
  cancelLabel: string;
  /** Executing: both buttons are disabled and spin, to avoid duplicate submission */
  busy?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

export function AiDeleteRequestCard({
  title,
  items,
  totalLabel,
  warning,
  disabledReason,
  irreversibleLabel,
  confirmLabel,
  cancelLabel,
  busy = false,
  onConfirm,
  onCancel,
}: AiDeleteRequestCardProps) {
  const confirmDisabled = busy || Boolean(disabledReason);

  return (
    <div className="overflow-hidden rounded-xl border border-[var(--color-danger)]/40 bg-[var(--bg-card)]">
      <div className="flex items-center gap-2 border-b border-[var(--color-danger)]/25 bg-[var(--color-danger)]/5 px-3 py-2">
        <ShieldAlert className="h-3.5 w-3.5 shrink-0 text-[var(--color-danger)]" />
        <span className="min-w-0 flex-1 text-xs font-medium text-[var(--text-primary)]">{title}</span>
      </div>

      <ul className="max-h-[12em] space-y-1 overflow-y-auto px-3 py-2">
        {items.map((item) => (
          <li key={item.id} className="flex items-start justify-between gap-3 text-xs">
            <span className="flex min-w-0 items-center gap-1.5">
              <span className="truncate text-[var(--text-primary)]">{item.label}</span>
              {item.irreversible && irreversibleLabel ? (
                <span className="shrink-0 rounded px-1 py-0.5 text-[0.72rem] text-[var(--color-danger)] ring-1 ring-inset ring-[var(--color-danger)]/40">
                  {irreversibleLabel}
                </span>
              ) : null}
            </span>
            {item.detail ? (
              <span className="shrink-0 tabular-nums text-[var(--text-muted)]">{item.detail}</span>
            ) : null}
          </li>
        ))}
      </ul>

      <div className="border-t border-[var(--border-color)] px-3 py-2">
        <p className="text-xs font-medium text-[var(--text-primary)]">{totalLabel}</p>
        {warning ? (
          <p className="mt-1 flex items-start gap-1.5 text-[0.79rem] text-[var(--color-warning)]">
            <AlertTriangle className="mt-0.5 h-3 w-3 shrink-0" />
            <span className="min-w-0">{warning}</span>
          </p>
        ) : null}
        {disabledReason ? (
          <p className="mt-1 text-[0.79rem] text-[var(--text-muted)]">{disabledReason}</p>
        ) : null}
      </div>

      <div className="flex items-center justify-end gap-2 border-t border-[var(--border-color)] px-3 py-2">
        <button
          type="button"
          onClick={onCancel}
          disabled={busy}
          className="rounded-lg border border-[var(--border-color)] px-2.5 py-1 text-xs text-[var(--text-secondary)] transition-colors hover:bg-[var(--bg-hover)] disabled:cursor-not-allowed disabled:opacity-50"
        >
          {cancelLabel}
        </button>
        <button
          type="button"
          onClick={onConfirm}
          disabled={confirmDisabled}
          className="inline-flex items-center gap-1.5 rounded-lg bg-red-600 px-2.5 py-1 text-xs font-medium text-white transition-colors hover:bg-red-700 disabled:cursor-not-allowed disabled:opacity-50"
        >
          {busy ? <Loader2 className="h-3 w-3 animate-spin" /> : null}
          {confirmLabel}
        </button>
      </div>
    </div>
  );
}
