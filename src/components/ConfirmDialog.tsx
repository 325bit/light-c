// ============================================================================
// Confirmation dialog component - for second confirmation before cleanup
// ============================================================================

import { memo, useState, useEffect, useRef } from 'react';
import { createPortal } from 'react-dom';
import { AlertTriangle, X } from 'lucide-react';
import { useTranslation } from 'react-i18next';

interface ConfirmDialogProps {
  /** Whether to show */
  isOpen: boolean;
  /** Title */
  title: string;
  /** Description text */
  description: string;
  /** Warning message (optional) */
  warning?: string;
  /** Confirm button text */
  confirmText?: string;
  /** Cancel button text */
  cancelText?: string;
  /** Confirm callback */
  onConfirm: () => void;
  /** Cancel callback */
  onCancel: () => void;
  /** Whether this is a dangerous operation */
  isDanger?: boolean;
}

export const ConfirmDialog = memo(function ConfirmDialog({
  isOpen,
  title,
  description,
  warning,
  confirmText,
  cancelText,
  onConfirm,
  onCancel,
  isDanger = false,
}: ConfirmDialogProps) {
  const { t } = useTranslation('common');
  // Use global translations only for buttons not explicitly specified; business modules can still provide more specific action copy.
  const resolvedConfirmText = confirmText ?? t('confirm');
  const resolvedCancelText = cancelText ?? t('cancel');
  const [isVisible, setIsVisible] = useState(false);
  const [isAnimating, setIsAnimating] = useState(false);
  const enteredRef = useRef(false);
  if (isVisible) enteredRef.current = true;

  useEffect(() => {
    if (isOpen) {
      setIsAnimating(true);
      setIsVisible(true);
    } else {
      setIsVisible(false);
      const timer = setTimeout(() => {
        setIsAnimating(false);
      }, 190);
      return () => clearTimeout(timer);
    }
  }, [isOpen]);

  if (!isOpen && !isAnimating) return null;

  return createPortal(
    // The confirmation dialog must sit above the settings page and module detail dialogs, so the dangerous-operation confirmation isn't covered by a parent dialog.
    <div className="fixed inset-0 z-[10050] flex items-center justify-center">
      {/* Overlay */}
      <div 
        className={`absolute inset-0 bg-black/50 backdrop-blur-sm glass-backdrop ${isVisible ? 'modal-overlay-in' : enteredRef.current ? 'modal-overlay-out' : 'opacity-0'}`}
        onClick={onCancel}
      />
      
      {/* Dialog */}
      {/* Dialog: width uses em to follow the global font size, 30em ≈ 420px at standard size.
          Height must also be clamped by the viewport: constraining width only means content taller than the window overflows vertically,
          pushing the title and confirm button out of view. Changed overflow-hidden to overflow-y-auto so content scrolls inside the dialog. */}
      <div className={`confirm-dialog glass-dialog relative bg-[var(--bg-elevated)] rounded-xl shadow-2xl border border-[var(--border-default)] w-[30em] max-w-[calc(100vw-2em)] max-h-[calc(100vh-2em)] overflow-y-auto ${isVisible ? 'modal-content-in' : enteredRef.current ? 'modal-content-out' : 'opacity-0'}`}>
        {/* Header */}
        <div className="flex items-center justify-between px-[1.43em] py-[1.14em] border-b border-[var(--border-default)]">
          <div className="flex items-center gap-[0.86em]">
            <div className={`w-[2.86em] h-[2.86em] rounded-full flex items-center justify-center ${
              isDanger ? 'bg-amber-500/15' : 'bg-emerald-500/15'
            }`}>
              <AlertTriangle className={`w-[1.43em] h-[1.43em] ${
                isDanger ? 'text-amber-500' : 'text-emerald-500'
              }`} />
            </div>
            <h3 className="text-[1.14em] font-semibold text-[var(--fg-primary)]">
              {title}
            </h3>
          </div>
          <button
            onClick={onCancel}
            className="p-[0.43em] rounded-lg text-[var(--fg-muted)] hover:text-[var(--fg-primary)] hover:bg-[var(--bg-hover)] transition-colors"
          >
            <X className="w-[1.14em] h-[1.14em]" />
          </button>
        </div>

        {/* Content */}
        <div className="px-[1.43em] py-[1.14em] space-y-[1.14em]">
          <p className="text-[1em] text-[var(--fg-secondary)] leading-relaxed">
            {description}
          </p>
          
          {warning && (
            <div className="bg-amber-500/10 border border-amber-500/30 rounded-lg p-[0.86em]">
              <p className="text-[0.86em] text-amber-600 dark:text-amber-400 leading-relaxed break-words whitespace-pre-wrap">
                {warning}
              </p>
            </div>
          )}
        </div>

        {/* Footer buttons */}
        <div className="glass-bar flex items-center justify-end gap-[0.86em] px-[1.43em] py-[1.14em] border-t border-[var(--border-default)] bg-[var(--bg-card)]">
          <button
            onClick={onCancel}
            className="px-[1.14em] py-[0.57em] rounded-lg text-[1em] font-medium text-[var(--fg-secondary)] hover:text-[var(--fg-primary)] hover:bg-[var(--bg-hover)] transition-colors"
          >
            {resolvedCancelText}
          </button>
          <button
            onClick={onConfirm}
            className={`px-[1.14em] py-[0.57em] rounded-lg text-[1em] font-medium text-white transition-all ${
              isDanger
                ? 'bg-gradient-to-r from-rose-500 to-red-500 hover:from-rose-600 hover:to-red-600 shadow-lg shadow-rose-500/25'
                : 'bg-gradient-to-r from-emerald-500 to-teal-500 hover:from-emerald-600 hover:to-teal-600 shadow-lg shadow-emerald-500/25'
            }`}
          >
            {resolvedConfirmText}
          </button>
        </div>
      </div>
    </div>,
    document.body
  );
});
