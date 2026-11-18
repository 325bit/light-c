// ============================================================================
// Common dropdown select component - adapted to theme colors
// ============================================================================

import { useState, useRef, useEffect, useCallback } from 'react';
import { ChevronDown } from 'lucide-react';

export interface SelectOption<T extends string = string> {
  value: T;
  label: string;
  /** Full hint text; when an item needs a short label, use title to keep the full information. */
  title?: string;
}

interface SelectProps<T extends string = string> {
  /** Currently selected value */
  value: T;
  /** Option list */
  options: SelectOption<T>[];
  /** Selection callback */
  onChange: (value: T) => void;
  /** Width class name, defaults to w-28 */
  widthClass?: string;
  /** Size mode; small is for module filter bars, default keeps the existing settings-page look */
  size?: 'sm' | 'md';
  /** Dropdown max-height class; long lists need a height limit to avoid breaking the module layout */
  menuMaxHeightClass?: string;
  /** Whether disabled */
  disabled?: boolean;
  /**
   * Visual form of the trigger.
   * - `default`: has a background and border, for standalone controls like settings pages and filter bars;
   * - `bare`: no background or border, only a faint background appears on hover.
   *   for containers that **already have their own material**, such as the top bar - stacking a bordered white box on top
   *   would read as "another thing stuck on the glass" rather than the same layer.
   */
  variant?: 'default' | 'bare';
}

export function Select<T extends string = string>({
  value,
  options,
  onChange,
  widthClass = 'w-28',
  size = 'md',
  menuMaxHeightClass = 'max-h-64',
  disabled = false,
  variant = 'default',
}: SelectProps<T>) {
  const [open, setOpen] = useState(false);
  /**
   * Open the dropdown upward.
   *
   * **Determine automatically** rather than having callers pass a parameter: controls stuck to the window bottom (e.g., the AI assistant panel's input area)
   * would be clipped or pushed off-screen if opened downward, and callers can hardly tell "how much room is left below".
   * Measure once on open; if space below is insufficient and there is more room above, flip upward.
   */
  const [openUpward, setOpenUpward] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  /** Approximate height the dropdown needs; below this value, consider "not enough room below". */
  const MENU_MIN_SPACE = 180;

  const updateDirection = useCallback(() => {
    const rect = containerRef.current?.getBoundingClientRect();
    if (!rect) return;
    const below = window.innerHeight - rect.bottom;
    const above = rect.top;
    setOpenUpward(below < MENU_MIN_SPACE && above > below);
  }, []);

  // Close when clicking outside
  const handleClickOutside = useCallback((e: MouseEvent) => {
    if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
      setOpen(false);
    }
  }, []);

  useEffect(() => {
    if (open) {
      document.addEventListener('mousedown', handleClickOutside);
      return () => document.removeEventListener('mousedown', handleClickOutside);
    }
  }, [open, handleClickOutside]);

  // Close on ESC
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [open]);

  const selectedOption = options.find(o => o.value === value);
  // Size only controls padding and border radius; **font size does not follow size**.
  // Previously sm=12px and md=14px, so the same control differed by one step between filter bars and settings pages, looking like two components.
  // Now uniformly text-sm (0.875rem, following the global font-size setting);
  // only the bare variant (top bar) uses inherit - there it follows the top bar's own rhythm, a special tuning.
  const triggerSizeClass = size === 'sm' ? 'px-2.5 py-1.5 rounded-lg' : 'px-3 py-2 rounded-xl';
  const textClass = variant === 'bare' ? 'text-[inherit]' : 'text-sm';
  const optionSizeClass = size === 'sm' ? 'px-2.5 py-1.5' : 'px-3 py-2';
  // bare: no background or border; clickability is shown by a faint background on hover; no green outline when open either,
  // otherwise a green box would suddenly appear on the glass top bar.
  const triggerVariantClass = variant === 'bare'
    ? 'bg-transparent border border-transparent hover:bg-[var(--bg-hover)]'
    : `bg-[var(--bg-card)] border border-[var(--border-color)] hover:border-[var(--brand-green)]/50 hover:bg-[var(--bg-hover)] ${
      open ? 'border-[var(--brand-green)] ring-2 ring-[var(--brand-green)]/20' : ''
    }`;

  return (
    <div ref={containerRef} className={`relative ${widthClass}`}>
      {/* Trigger */}
      <button
        type="button"
        disabled={disabled}
        onClick={() => {
          // Measure direction once before opening: controls at the window bottom must open upward
          if (!open) updateDirection();
          setOpen(!open);
        }}
        className={`w-full flex items-center ${variant === 'bare' ? 'justify-start gap-1' : 'justify-between gap-2'} ${triggerSizeClass} ${textClass}
          ${triggerVariantClass}
          text-[var(--text-primary)] font-medium
          focus:outline-none focus:ring-2 focus:ring-[var(--brand-green)]/20
          transition-all duration-200
          ${disabled ? 'opacity-50 cursor-not-allowed' : 'cursor-pointer'}`}
      >
        <span className="min-w-0 truncate" title={selectedOption?.title ?? selectedOption?.label ?? value}>
          {selectedOption?.label ?? value}
        </span>
        <ChevronDown
          className={`w-4 h-4 text-[var(--text-muted)] transition-transform duration-200 ${
            open ? 'rotate-180' : ''
          }`}
        />
      </button>

      {/* Dropdown list */}
      {open && (
        <div
          /* ⚠️ Width cannot be w-full: that means "follow the trigger width",
             if the trigger is auto-sized (w-auto, e.g., the top-bar drive selector is only as wide as "C: ⌄"),
             menu items would be truncated to "D: o...". Change it to "at least as wide as the trigger,
             expand to the longest item when content is wider" - covering both cases. */
          className={`glass-overlay absolute left-0 min-w-full w-max py-1 rounded-xl
            ${openUpward ? 'bottom-full mb-1.5 slide-in-from-bottom-1' : 'top-full mt-1.5 slide-in-from-top-1'}
            /* Use the overlay-specific --glass-overlay-bg as the background: .glass-overlay's default would be overridden by
               bg-[var(--bg-card)] (only 0.42 white in glass mode), letting the list text behind show through,
               which is distracting when reading options. In non-glass themes the variable is undefined, falling back to --bg-card. */
            bg-[var(--glass-overlay-bg,var(--bg-card))] border border-[var(--border-color)]
            shadow-lg shadow-black/5 z-50 overflow-y-auto ${menuMaxHeightClass}
            animate-in fade-in duration-150`}
        >
          {/* Filter items may come from platform/model types with unpredictable counts; truncation and scrolling keep the dropdown from squeezing the result area. */}
          {options.map((option) => {
            const isSelected = option.value === value;
            return (
              <button
                key={option.value}
                type="button"
                onClick={() => {
                  onChange(option.value);
                  setOpen(false);
                }}
                className={`w-full flex items-center justify-between gap-2 ${optionSizeClass} ${textClass}
                  transition-colors duration-100
                  ${isSelected
                    ? 'bg-[var(--brand-green)]/10 text-[var(--brand-green)] font-medium'
                    : 'text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]'
                  }`}
              >
                <span className="min-w-0 truncate" title={option.title ?? option.label}>{option.label}</span>
                {isSelected && (
                  <span className="w-1.5 h-1.5 shrink-0 rounded-full bg-[var(--brand-green)]" />
                )}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
