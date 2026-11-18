// ============================================================================
// Themed toggle switch
// Follows the same convention as Checkbox: size uses em to scale with the global font-size setting, colors always go through theme variables.
// Underneath it is still a native checkbox, only role is changed to switch, so keyboard operation and screen readers work directly.
// ============================================================================

import type { ChangeEvent, InputHTMLAttributes } from 'react';

export interface SwitchProps extends Omit<InputHTMLAttributes<HTMLInputElement>, 'type' | 'onChange'> {
  /** Callback when the switch state changes */
  onChange?: (checked: boolean, event: ChangeEvent<HTMLInputElement>) => void;
}

export function Switch({ className = '', onChange, ...props }: SwitchProps) {
  return (
    <span className="relative inline-flex h-[1.43em] w-[2.57em] shrink-0 items-center">
      {/* Track: the native input itself, carrying clicks, focus and accessibility semantics */}
      <input
        {...props}
        type="checkbox"
        role="switch"
        onChange={(event) => onChange?.(event.currentTarget.checked, event)}
        className={`peer absolute inset-0 m-0 h-full w-full cursor-pointer appearance-none rounded-full border border-[var(--border-color)] bg-[var(--bg-switch)] transition-colors duration-150 focus:outline-none focus-visible:ring-2 focus-visible:ring-[var(--brand-green)]/30 checked:border-[var(--brand-green)] checked:bg-[var(--brand-green)] disabled:cursor-not-allowed disabled:opacity-50 ${className}`}
      />
      {/* Thumb: purely decorative, displacement = track width - thumb width - left/right padding */}
      <span
        aria-hidden="true"
        className="pointer-events-none absolute left-[0.14em] top-[0.14em] h-[1.14em] w-[1.14em] rounded-full bg-white shadow-sm transition-transform duration-150 peer-checked:translate-x-[1.15em]"
      />
    </span>
  );
}
