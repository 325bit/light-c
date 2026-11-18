// ============================================================================
// The "Stop" button during scanning
//
// Fixed position in the card header (`ModuleCard`'s `headerExtra`), on the same row as the "scanning…" button and toggle:
// when users want to stop a scan their eyes are already at the top-right, the button should not scroll with body content, let alone be pushed away by the skeleton screen.
//
// Shared by junk cleanup / hotspot analysis / disk change analysis, styles maintained in only one place here —
// previously each of the three modules drew its own orange button, and colors and padding did not line up.
// Color goes through `--color-warning` (not hard-coded amber), so it stays clear in both light and dark themes.
// ============================================================================

import { StopCircle } from 'lucide-react';
import { useTranslation } from 'react-i18next';

interface StopScanButtonProps {
  onClick: () => void;
  /** Override the default text (defaults to `common:stop`) */
  label?: string;
}

export function StopScanButton({ onClick, label }: StopScanButtonProps) {
  const { t } = useTranslation('common');

  return (
    <button
      type="button"
      onClick={onClick}
      className="flex shrink-0 items-center gap-1.5 rounded-lg bg-[color-mix(in_srgb,var(--color-warning)_12%,transparent)] px-3 py-1.5 text-xs font-medium text-[var(--color-warning)] transition-colors hover:bg-[color-mix(in_srgb,var(--color-warning)_22%,transparent)]"
    >
      <StopCircle className="h-3.5 w-3.5" />
      {label ?? t('stop')}
    </button>
  );
}

export default StopScanButton;
