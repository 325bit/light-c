// ============================================================================
// AI delete result card (standalone, reusable)
//
// Like the request card, this is a pure presentation component. Its point: **delete results always follow what the backend returns**,
// rather than being expressed through text in the chat bubble — the model restating numbers is the most error-prone part,
// while the strings shown here come directly from the result structure the caller received.
//
// Failed items are an expandable list: what users most need to know is "which item wasn't deleted, and why".
// ============================================================================

import { useState } from 'react';
import { AlertTriangle, CheckCircle2, ChevronDown, ChevronRight, RotateCw } from 'lucide-react';

export interface AiDeleteFailure {
  name: string;
  reason: string;
}

export interface AiDeleteResultCardProps {
  /** Success summary, e.g. "Deleted 1,233 items, freed 615.02 MB" */
  successLabel: string;
  /** Failure summary; omit when there are no failures */
  failedLabel?: string;
  /** Failure details (name + reason), expandable */
  failures?: AiDeleteFailure[];
  /** Copy for the failure-detail expand/collapse button */
  detailsLabel?: string;
  /** Hint shown when a reboot is required for changes to take effect */
  rebootNotice?: string;
}

export function AiDeleteResultCard({
  successLabel,
  failedLabel,
  failures,
  detailsLabel,
  rebootNotice,
}: AiDeleteResultCardProps) {
  const [expanded, setExpanded] = useState(false);
  const hasFailures = Boolean(failedLabel) && (failures?.length ?? 0) > 0;

  return (
    <div className="overflow-hidden rounded-xl border border-[var(--border-color)] bg-[var(--bg-card)]">
      <div className="flex items-center gap-2 px-3 py-2">
        <CheckCircle2 className="h-3.5 w-3.5 shrink-0 text-[var(--brand-green)]" />
        <span className="min-w-0 flex-1 text-xs font-medium text-[var(--text-primary)]">{successLabel}</span>
      </div>

      {hasFailures ? (
        <div className="border-t border-[var(--border-color)] px-3 py-2">
          <button
            type="button"
            onClick={() => setExpanded((previous) => !previous)}
            className="flex w-full items-center gap-1.5 text-left text-xs text-[var(--color-warning)]"
          >
            {expanded ? <ChevronDown className="h-3 w-3 shrink-0" /> : <ChevronRight className="h-3 w-3 shrink-0" />}
            <AlertTriangle className="h-3 w-3 shrink-0" />
            <span className="min-w-0 flex-1">{failedLabel}</span>
            {detailsLabel ? <span className="shrink-0 text-[var(--text-muted)]">{detailsLabel}</span> : null}
          </button>

          {expanded ? (
            <ul className="mt-1.5 space-y-1 pl-6">
              {failures?.map((failure, index) => (
                <li key={`${failure.name}-${index}`} className="text-[0.79rem] leading-relaxed">
                  <span className="break-all text-[var(--text-primary)]">{failure.name}</span>
                  <span className="ml-1 break-all text-[var(--text-muted)]">{failure.reason}</span>
                </li>
              ))}
            </ul>
          ) : null}
        </div>
      ) : null}

      {rebootNotice ? (
        <div className="flex items-start gap-1.5 border-t border-[var(--border-color)] px-3 py-2 text-[0.79rem] text-[var(--text-muted)]">
          <RotateCw className="mt-0.5 h-3 w-3 shrink-0" />
          <span className="min-w-0">{rebootNotice}</span>
        </div>
      ) : null}
    </div>
  );
}
