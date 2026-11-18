// ============================================================================
// Quick "Ask AI Assistant" button on data rows
//
// All modules attach this component to their data rows, so styling and behavior stay consistent without rewriting it everywhere.
// On click, the assistant opens the panel and sends the question directly — users need not retype the keywords.
//
// The question text is provided by the caller, usually **reusing the module's existing "one-click search" keywords** (path + what is it/can it be deleted),
// so browser search and asking the AI use the same sentence, avoiding two sets of wording.
// ============================================================================

import { useTranslation } from 'react-i18next';
import { AiMarkIcon } from './AiMarkIcon';
import { requestAiAsk } from './aiAsk';
import { useSettings } from '../../contexts';

interface AiAskButtonProps {
  /** The question to ask the AI; when empty the button does not render, avoiding a dead entry that does nothing when clicked */
  question: string;
  /**
   * This question needs no data lookup (the user has already expanded and seen the row in the UI; the path in the question is that very row).
   * When set, the assistant declares no tools and answers directly from the model's own knowledge — otherwise it would waste a read attempt.
   */
  skipLookup?: boolean;
  /** Allows the caller to add positioning classes (e.g. the module's own hover state) */
  className?: string;
}

export function AiAskButton({ question, skipLookup = false, className = '' }: AiAskButtonProps) {
  const { t } = useTranslation('ai');
  const { settings } = useSettings();

  // When the entry is disabled, inline buttons hide too: hiding only the floating ball leaves a bunch of entries in the list that pop up
  // the assistant panel when clicked, meaning it was not fully turned off.
  if (!settings.aiAssistantEnabled) return null;

  if (!question.trim()) return null;

  return (
    <button
      type="button"
      onClick={() => requestAiAsk(question, { skipLookup })}
      title={t('askHint')}
      aria-label={t('askHint')}
      className={`rounded-lg p-1.5 text-[var(--text-muted)] transition-colors hover:bg-[var(--brand-green-10)] hover:text-[var(--brand-green)] ${className}`}
    >
      <AiMarkIcon className="h-3.5 w-3.5" />
    </button>
  );
}
