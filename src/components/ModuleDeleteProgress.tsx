// ============================================================================
// Module deletion progress state
//
// Two origins:
// 1. Deletion used to be shown with a full-screen overlay (fixed inset-0 + createPortal), which covered the title bar —
//    during deletion you could neither drag the window nor interact with the top bar (reported in an issue). The deletion itself runs
//    on a backend thread, so there's no reason to block the UI.
// 2. Presentation splits into two forms based on each module's deletion semantics:
//
//    · variant="block" (junk cleanup)
//      Modules where "delete everything is safe" — risk ≤ 2 is preselected by default, users don't need to watch the list while deleting.
//      So replace the whole content with a "working" panel: progress ring + phase name and processed count +
//      a divider + freed / failed / speed / remaining. Block centered, text left-aligned (see StatePanel).
//
//      ⚠️ This area failed twice before; don't repeat either mistake:
//      ① Hand-drawing a "red box + magenta background + centered block": not the same language as the site-wide green/white glass, and it didn't get
//         flex height, so in page mode it ended after 100-something pixels with a big blank area below;
//      ② Switching to "skeleton rows exactly like scanning": deletion isn't "results not ready yet",
//         a row of fake lists just looks like the UI froze — the deleting state needs its own look (other software also uses
//         a spinner/progress ring + numbers; nobody expresses deletion with a skeleton screen).
//
//    · variant="floating" (social cleanup / big files / context menu / registry)
//      Modules where "users pick the subset to delete" — during deletion the reference of "which items I'm deleting" must stay visible,
//      so the list can't move. This content is carried by ModuleOperationToolbar: it is position: fixed
//      floating layer, **completely out of document flow**, so the list is neither covered nor pushed/shifted.
//
// Copy is passed in by callers: each module has its own i18n namespace and progress fields; this file only handles layout.
// ============================================================================

import { Loader2 } from 'lucide-react';
import { StatePanel } from './StatePanel';

interface ModuleDeleteProgressProps {
  /** Localized phase label (e.g. "Cleaning junk files") */
  phaseLabel: string;
  /** Localized progress description (e.g. "Processed 501 / 12,731 files") */
  progressLabel: string;
  /**
   * Pass 0-100 when progress is known; omit it to show an indeterminate bar.
   * Some modules' delete commands don't report progress (only "deleting N items"), they reuse this same panel.
   */
  percent?: number;
  /** Localized extra stats (block variant only), spread centered, wrap automatically on narrow windows */
  details?: string[];
  /** Optional bottom hint (block variant only), e.g. "Do not close the window" */
  hint?: string;
  /** Presentation variant, see the file header. Defaults to block. */
  variant?: 'block' | 'floating';
}

export function ModuleDeleteProgress({
  phaseLabel,
  progressLabel,
  percent,
  details = [],
  hint,
  variant = 'block',
}: ModuleDeleteProgressProps) {
  const hasPercent = typeof percent === 'number';
  // The floating form (deleting the items the user picked) keeps the danger color — that spot calls for "attention, permanent deletion in progress";
  // the block form (junk cleanup swaps the whole area for a status panel) uses the same language as the scanning state, i.e. brand green.
  const barColorClass = variant === 'floating' ? 'bg-[var(--color-danger)]' : 'bg-[var(--brand-green)]';

  /** The progress bar itself: fills proportionally when progress is known, otherwise pulses across the whole bar to mean "in progress" */
  const progressBar = (
    <div className={`w-full rounded-full bg-[var(--bg-hover)] overflow-hidden ${variant === 'floating' ? 'h-1' : 'h-1.5'}`}>
      {hasPercent ? (
        <div
          className={`h-full rounded-full transition-all duration-300 ${barColorClass}`}
          style={{ width: `${percent}%` }}
        />
      ) : (
        <div className={`h-full w-full rounded-full animate-pulse ${barColorClass}`} />
      )}
    </div>
  );

  // Floating form: placed in the bottom ModuleOperationToolbar (horizontal action bar, with a centered progress panel).
  // Give it enough width so both lines "phase name + processed count" aren't truncated — too narrow turns them into a string of ellipses.
  if (variant === 'floating') {
    return (
      <div className="flex w-[20em] flex-col gap-1.5 px-1 py-0.5">
        <div className="flex items-center gap-1.5">
          <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin text-[var(--color-danger)]" />
          <span className="truncate text-[0.93rem] font-medium text-[var(--fg-primary)]">{phaseLabel}</span>
          {hasPercent && (
            <span className="ml-auto shrink-0 text-[0.79rem] font-medium text-[var(--color-danger)] tabular-nums">{percent}%</span>
          )}
        </div>
        <span className="truncate text-[0.79rem] text-[var(--fg-muted)] tabular-nums">{progressLabel}</span>
        {progressBar}
        {hint && <span className="text-[0.71rem] text-[var(--fg-faint)]">{hint}</span>}
      </div>
    );
  }

  // Block form: progress ring + phase name + stats. No linear bar — the ring already shows the percentage,
  // adding another would mean "two progress lines" (the scanning one is in the card header, so it shouldn't be repeated here).
  return (
    <StatePanel>
      <div className="flex items-center gap-4">
        <ProgressRing percent={percent} indeterminate={!hasPercent} />
        <div className="min-w-0 flex-1">
          <p className="text-sm font-semibold text-[var(--text-primary)]">{phaseLabel}</p>
          <p className="mt-1 text-xs text-[var(--text-muted)] tabular-nums">{progressLabel}</p>
        </div>
      </div>

      {details.length > 0 && (
        <div className="mt-6 grid grid-cols-2 gap-x-6 gap-y-2 border-t border-[var(--border-muted)] pt-4 text-xs text-[var(--text-muted)] tabular-nums sm:grid-cols-4">
          {details.map((item) => (
            <span key={item}>{item}</span>
          ))}
        </div>
      )}

      {hint && <p className="mt-4 text-[0.79rem] text-[var(--fg-faint)]">{hint}</p>}
    </StatePanel>
  );
}

/**
 * Progress ring.
 *
 * Why a ring instead of a bar: the scanning state's progress bar is fixed in the **card header** (the flowing highlight),
 * another green bar of the same shape in the body makes it unclear which one is this operation's progress.
 * The ring gathers the percentage in its center, is denser, and is the most common look for "deletion in progress".
 */
function ProgressRing({ percent, indeterminate }: { percent?: number; indeterminate: boolean }) {
  const size = 56;
  const stroke = 4;
  const radius = (size - stroke) / 2;
  const circumference = 2 * Math.PI * radius;
  const clamped = Math.max(0, Math.min(100, percent ?? 0));
  const filled = (circumference * clamped) / 100;

  return (
    <div className="relative shrink-0" style={{ width: size, height: size }}>
      {/* Rotate -90° so progress starts drawing from 12 o'clock */}
      <svg width={size} height={size} viewBox={`0 0 ${size} ${size}`} className="-rotate-90" aria-hidden="true">
        <circle cx={size / 2} cy={size / 2} r={radius} fill="none" stroke="var(--bg-hover)" strokeWidth={stroke} />
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius}
          fill="none"
          stroke="var(--brand-green)"
          strokeWidth={stroke}
          strokeLinecap="round"
          strokeDasharray={`${filled} ${circumference - filled}`}
          className="transition-all duration-500"
        />
      </svg>
      <span className="absolute inset-0 flex items-center justify-center text-[0.79rem] font-semibold text-[var(--text-primary)] tabular-nums">
        {/* If no percentage is available (some delete commands only report "deleting N items"), spin instead of faking a 0% */}
        {indeterminate ? <Loader2 className="h-4 w-4 animate-spin text-[var(--brand-green)]" /> : `${clamped}%`}
      </span>
    </div>
  );
}
