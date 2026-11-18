// ============================================================================
// Risk badge for a single cleanable directory
//
// Three tones, matching the backend `DevRisk`:
// - `cache`    → no badge (it is the majority; badging every row would drown out the ones that matter)
// - `confirm`  → yellow "would lose data"
// - `readonly` → gray "display only"
//
// Extracted because two places now render the same rows: the dev cache page and the "other software caches" section
// on the leftover page. The `label` stays a prop so each page keeps its own i18n namespace.
// ============================================================================
import type { DevRisk } from '../../types/devCache';

export interface RiskTagProps {
  risk: DevRisk;
  label: string;
}

export function RiskTag({ risk, label }: RiskTagProps) {
  if (risk === 'cache') return null;
  const tone =
    risk === 'confirm'
      ? 'bg-[var(--warning)]/15 text-[var(--warning)]'
      : 'bg-[var(--bg-hover)] text-[var(--text-muted)]';
  return (
    <span className={`shrink-0 rounded px-1.5 py-0.5 text-[0.72rem] font-medium ${tone}`}>
      {label}
    </span>
  );
}

export default RiskTag;
