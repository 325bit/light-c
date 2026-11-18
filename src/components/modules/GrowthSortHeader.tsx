// ============================================================================
// Sortable column header shared by the growth result list and the drill-down modal
//
// Both surfaces sort siblings only (a parent never moves below its children), so
// the header has to say the same thing in both places: active column, direction,
// and what a click will do next.
// ============================================================================

import { ArrowDown, ArrowUp, ChevronsUpDown } from 'lucide-react';
import { useTranslation } from 'react-i18next';

export interface SortHeaderProps {
  label: string;
  active: boolean;
  desc: boolean;
  align?: 'left' | 'right';
  onClick: () => void;
}

export function SortHeader({ label, active, desc, align = 'left', onClick }: SortHeaderProps) {
  const { t } = useTranslation('common');
  const hint = active
    ? (desc ? t('sortDescendingHint') : t('sortAscendingHint'))
    : t('sortSwitchHint');
  return (
    <button
      type="button"
      onClick={onClick}
      title={hint}
      className={`flex w-full min-w-0 items-center gap-1 transition-colors hover:text-[var(--text-primary)] ${
        align === 'right' ? 'justify-end' : ''
      } ${active ? 'text-[var(--brand-green)]' : ''}`}
    >
      <span className="truncate">{label}</span>
      {active
        ? (desc ? <ArrowDown className="h-3 w-3 shrink-0" /> : <ArrowUp className="h-3 w-3 shrink-0" />)
        : <ChevronsUpDown className="h-3 w-3 shrink-0 opacity-40" />}
    </button>
  );
}

export default SortHeader;
