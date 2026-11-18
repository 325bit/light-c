/**
 * The "beta" marker.
 *
 * Used in two places: the "AI Assistant" entry in settings, and the AI panel title.
 * Why hardcode Chinese/English: "beta" is a universal term and is not translated in any language,
 * so it does not go into the language packs - avoiding the need to maintain an identical string in each of the four language files.
 *
 * The styling is deliberately understated (muted color + 1px border): it is a status note and should not steal attention from the title.
 */
export function BetaBadge({ className = '' }: { className?: string }) {
  return (
    <span
      className={`shrink-0 whitespace-nowrap rounded border border-[var(--border-color)] px-1.5 py-0.5 text-[0.71rem] font-medium leading-none text-[var(--text-muted)] ${className}`}
    >
      beta
    </span>
  );
}
