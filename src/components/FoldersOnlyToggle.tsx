// ============================================================================
// "Folders only" switch for every file tree
//
// One shared component so the control looks and behaves the same in the junk
// cleanup modal, the card lists, social cleanup and the disk-growth detail
// tree — the preference itself lives in useFoldersOnly.
// ============================================================================

import { FolderTree } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { useFoldersOnly } from '../hooks/useFoldersOnly';

interface FoldersOnlyToggleProps {
  /** `label` for modal headers with room, `icon` for dense toolbars */
  variant?: 'label' | 'icon';
  className?: string;
}

export function FoldersOnlyToggle({ variant = 'label', className = '' }: FoldersOnlyToggleProps) {
  const { t } = useTranslation('common');
  const [foldersOnly, setFoldersOnly] = useFoldersOnly();
  const label = t('onlyFolders');

  return (
    <button
      type="button"
      onClick={() => setFoldersOnly(!foldersOnly)}
      aria-pressed={foldersOnly}
      title={`${label} — ${t('onlyFoldersHint')}`}
      className={`flex shrink-0 items-center gap-1 rounded transition ${
        foldersOnly
          ? 'bg-[var(--brand-green-10)] text-[var(--brand-green)]'
          : 'text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--brand-green)]'
      } ${variant === 'icon' ? 'p-1' : 'px-1.5 py-0.5 text-[0.79rem]'} ${className}`}
    >
      <FolderTree className={variant === 'icon' ? 'h-3.5 w-3.5' : 'h-3.5 w-3.5 shrink-0'} />
      {variant === 'label' && <span>{label}</span>}
    </button>
  );
}

export default FoldersOnlyToggle;
