// ============================================================================
// Inline module scan state
// Used for the waiting state inside the scan page, avoiding each module maintaining its own low-profile Loading UI.
//
// [Why it looks like this]
// The old version was "a block of pale green background + spinner" with a hard-coded height of 220px: when the window grew taller, only a short green strip hung at the top of the card,
// with a large blank area below; a large brand-color block outshone the content and clashed with the green success state of "cleaned".
//
// Current division of labor:
//   ① Keep only one progress bar, in the card header (handled by ModuleCard), not drawn again in the body —
//      drawing the same info twice means "two loading lines";
//   ② Stage line: icon + stage name + elapsed time, same font size as body text, not enlarged;
//   ③ Skeleton list: laid out **in the shape of real result rows** (left icon slot + main title + right-side value),
//      with row spacing stretched along with body height. Previously it was five centered thin lines that shrank into a small cluster in the middle on large windows,
//      leaving big empty areas above and below — that was not whitespace, it was unfilled space.
// ============================================================================

import type { ReactNode } from 'react';
import { Loader2 } from 'lucide-react';

interface ModuleScanProgressProps {
  /** Scan stage title */
  title: string;
  /** Scan stage description */
  description?: string;
  /** Module-specific icon */
  icon?: ReactNode;
  /** Extra stage info or action buttons */
  footer?: ReactNode;
  /** Additional style class name */
  className?: string;
}

/**
 * Shape of a skeleton row: `[main bar width, value bar width]` (percentages).
 *
 * Stagger the widths to look like real content (each line has a different length); equal widths look like table separators.
 * The value bar is right-aligned, corresponding to file size / count in result rows.
 */
const SKELETON_ROWS: [number, number][] = [
  [58, 14],
  [46, 10],
  [52, 12],
  [38, 9],
  [50, 11],
  [34, 8],
];

/**
 * The skeleton row itself.
 *
 * Shape follows real result rows: left square icon slot + main title bar + right-side value bar.
 * (The deleting state once used it too, but that was removed — deleting is not "results not out yet", and a row of fake lists just looks stuck.)
 */
function ScanSkeletonRows() {
  // The skeleton list is only a visual hint, screen readers do not need it; mark the whole block aria-hidden
  return (
    <div className="scan-state__skeleton" aria-hidden="true">
      {SKELETON_ROWS.map(([titleWidth, metaWidth]) => (
        <div key={titleWidth} className="scan-state__skeleton-row">
          <i className="scan-state__skeleton-icon" />
          <i className="scan-state__skeleton-title" style={{ width: `${titleWidth}%` }} />
          <i className="scan-state__skeleton-meta" style={{ width: `${metaWidth}%` }} />
        </div>
      ))}
    </div>
  );
}

export function ModuleScanProgress({
  title,
  description,
  icon,
  footer,
  className = '',
}: ModuleScanProgressProps) {
  // No longer render the "scanning" pill: the card header's status badge already says the same thing, a second one in the body is redundant.
  // Appearance goes through `.scan-state`; `module-scan-progress-area` is only used to let page mode fill this area.
  return (
    <div className={`module-scan-progress-area scan-state ${className}`}>
      <div className="scan-state__stack">
        <div className="scan-state__head">
          <div className="scan-state__icon">
            {icon ?? <Loader2 className="h-5 w-5 animate-spin text-[var(--brand-green)]" />}
          </div>
          <div className="min-w-0">
            <p className="text-sm font-medium text-[var(--text-primary)]">{title}</p>
            {description && (
              <p className="mt-0.5 text-xs leading-relaxed text-[var(--text-muted)]">
                {description}
              </p>
            )}
          </div>
        </div>

        <ScanSkeletonRows />
      </div>

      {footer && <div className="scan-state__footer">{footer}</div>}
    </div>
  );
}

export default ModuleScanProgress;
