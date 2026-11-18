// ============================================================================
// Generic empty-data placeholder component
// ============================================================================

import type { ComponentType, ReactNode } from 'react';
import { CheckCircle2, Sparkles } from 'lucide-react';
import { useTranslation } from 'react-i18next';

interface EmptyStateProps {
  /** Use an icon to carry the current state, avoiding each module writing its own blank placeholder. */
  icon?: ComponentType<{ className?: string }>;
  title?: string;
  description?: string;
  action?: ReactNode;
  tone?: 'neutral' | 'success';
  compact?: boolean;
  className?: string;
}

export function EmptyState({
  icon,
  title,
  description,
  action,
  tone = 'neutral',
  compact = false,
  className = '',
}: EmptyStateProps) {
  const { t } = useTranslation('common');
  // The default empty state comes from the shared language pack; a page-specific title passed in still takes priority.
  const resolvedTitle = title ?? t('noData');
  const resolvedDescription = description ?? t('emptyDescription');
  const Icon = icon ?? (tone === 'success' ? CheckCircle2 : Sparkles);
  const iconClassName = tone === 'success'
    ? 'bg-[var(--brand-green-10)] text-[var(--brand-green)]'
    : 'bg-[var(--brand-green-10)] text-[var(--brand-green)]';

  return (
    <div
      className={`empty-state relative overflow-hidden flex flex-col items-center justify-center rounded-xl border border-dashed border-[var(--border-color)] bg-[linear-gradient(135deg,var(--bg-main),var(--brand-green-10))] px-6 text-center ${
        compact ? 'empty-state--compact min-h-[18.57rem] py-8' : 'min-h-[22.86rem] py-14'
      } ${className}`}
    >
      {/* Use a low-contrast background layer to add texture to the empty state, avoiding a plain gray placeholder looking empty in page mode. */}
      <div className="pointer-events-none absolute -left-10 -top-10 h-32 w-32 rounded-full bg-[var(--brand-green)]/5" />
      <div className="pointer-events-none absolute -right-12 bottom-4 h-28 w-28 rounded-full bg-[var(--brand-green)]/5" />
      <div className={`relative mb-3 flex h-14 w-14 items-center justify-center rounded-2xl shadow-sm ${iconClassName}`}>
        <Icon className="h-6 w-6" />
      </div>
      <p className="relative text-sm font-semibold text-[var(--text-primary)]">{resolvedTitle}</p>
      {resolvedDescription && (
        <p className="relative mt-1 max-w-sm text-xs leading-relaxed text-[var(--text-muted)]">
          {resolvedDescription}
        </p>
      )}
      {action && <div className="relative mt-4">{action}</div>}
    </div>
  );
}
