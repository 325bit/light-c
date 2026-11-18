// ============================================================================
// File list sort control (name / size / modification time)
//
// Note: the junk cleanup "View All Files" dialog uses this sorting first, and the social-specific and AI-tool cleanup
// "All Files" dialogs need the same controls. Rather than writing buttons and direction toggles in each dialog,
// consolidate into one component - the visuals and semantics of the three entry points (clicking the same key toggles direction, clicking a different key uses that key's default direction)
// now have a single implementation.
// ============================================================================

import { ArrowDown, ArrowUp } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { DEFAULT_SORT_DESC, type TreeSort, type TreeSortKey } from '../utils/fileTree';

/** Default display order of sort keys: name / size / modification time */
export const FILE_SORT_KEYS: TreeSortKey[] = ['name', 'size', 'time'];

/**
 * Clicking the same key toggles ascending/descending; clicking a different key switches to that key's default direction
 * (size and time default to descending / newest first, name defaults to ascending).
 */
export function nextSort(previous: TreeSort, key: TreeSortKey): TreeSort {
  return previous.key === key
    ? { key, desc: !previous.desc }
    : { key, desc: DEFAULT_SORT_DESC[key] };
}

export interface FileSortControlProps {
  sort: TreeSort;
  /** The final target sort; the component has already computed the direction, the caller just receives it */
  onSortChange: (next: TreeSort) => void;
  /**
   * The sort keys actually usable for this batch of data.
   *
   * Note: when the data source has no modification time, do not show a "modification time" button - clicking it would fill the whole column with
   * "—", and users would just think sorting is broken. By default all three are given.
   */
  keys?: TreeSortKey[];
  className?: string;
}

export function FileSortControl({
  sort,
  onSortChange,
  keys = FILE_SORT_KEYS,
  className = '',
}: FileSortControlProps) {
  const { t } = useTranslation('common');

  const labelOf = (key: TreeSortKey) => {
    if (key === 'size') return t('sortBySize');
    if (key === 'time') return t('sortByTime');
    return t('sortByName');
  };

  return (
    <div className={`flex shrink-0 items-center gap-0.5 rounded-lg bg-[var(--bg-hover)] p-0.5 ${className}`}>
      {keys.map((key) => {
        const active = sort.key === key;
        return (
          <button
            key={key}
            type="button"
            onClick={() => onSortChange(nextSort(sort, key))}
            title={active
              ? (sort.desc ? t('sortDescendingHint') : t('sortAscendingHint'))
              : t('sortSwitchHint')}
            className={`flex items-center gap-0.5 rounded-md px-1.5 py-0.5 text-[0.79rem] transition
              ${active
                ? 'bg-[var(--bg-card)] font-medium text-[var(--brand-green)] shadow-sm'
                : 'text-[var(--text-muted)] hover:text-[var(--brand-green)]'}`}
          >
            {labelOf(key)}
            {active && (sort.desc
              ? <ArrowDown className="h-3 w-3" />
              : <ArrowUp className="h-3 w-3" />)}
          </button>
        );
      })}
    </div>
  );
}
