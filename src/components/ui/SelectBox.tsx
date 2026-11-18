// ============================================================================
// Decorative tri-state checkbox
//
// Not `ui/Checkbox`: that one is a real `<input type=checkbox>`, which cannot draw the "partial" (dash) state used by
// group headers of selectable lists. Here the row itself is the click target (`onClick` lives on the parent row/header),
// so this component stays purely presentational.
//
// Extracted because the dev cache page and the multi-version leftover section both need the same group/entry checkbox;
// keeping two copies had already started to drift (border width, tick path).
// ============================================================================

export interface SelectBoxProps {
  checked: boolean;
  /** Group level only: draw a dash when part of the group is selected instead of an empty box */
  partial?: boolean;
}

export function SelectBox({ checked, partial = false }: SelectBoxProps) {
  return (
    <div
      className={`flex h-5 w-5 shrink-0 items-center justify-center rounded border-2 transition-colors ${
        checked
          ? 'border-[var(--brand-green)] bg-[var(--brand-green)]'
          : partial
            ? 'border-[var(--brand-green)] bg-[var(--brand-green)]/50'
            : 'border-[var(--text-faint)]'
      }`}
    >
      {(checked || partial) && (
        <svg
          className="h-3 w-3 text-white"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
          strokeWidth={3}
        >
          {checked ? (
            <path strokeLinecap="round" strokeLinejoin="round" d="M5 13l4 4L19 7" />
          ) : (
            <path strokeLinecap="round" d="M5 12h14" />
          )}
        </svg>
      )}
    </div>
  );
}

export default SelectBox;
