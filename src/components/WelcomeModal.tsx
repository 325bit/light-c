// ============================================================================
// Welcome modal component
// Shows a welcome message on first use, with a "Don't show again" option.
//
// Visuals and sizing follow global settings:
// - colors use theme variables, auto-adapting in dark mode;
// - all sizes use em, inheriting :root's font-size (= --base-font-size + --font-size-offset),
//   so when the user adjusts font size, the modal's width/height and text scale proportionally with the main UI.
// ============================================================================

import { useState, useEffect, useRef } from 'react';
import { createPortal } from 'react-dom';
import { X, Shield, Zap, Sparkles } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Checkbox } from './ui/Checkbox';

interface WelcomeModalProps {
  /** Whether to show the modal */
  isOpen: boolean;
  /** Callback to close the modal */
  onClose: () => void;
}

const STORAGE_KEY = 'lightc_welcome_dismissed';

export function WelcomeModal({ isOpen, onClose }: WelcomeModalProps) {
  const { t } = useTranslation('ui');
  const [dontShowAgain, setDontShowAgain] = useState(false);
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
      const timer = setTimeout(() => setIsAnimating(false), 280);
      return () => clearTimeout(timer);
    }
  }, [isOpen]);

  const handleClose = () => {
    if (dontShowAgain) {
      localStorage.setItem(STORAGE_KEY, 'true');
    }
    onClose();
  };

  if (!isOpen && !isAnimating) return null;

  const features = [
    { icon: Sparkles, text: t('welcomeLight'), desc: t('welcomeLightDesc') },
    { icon: Shield, text: t('welcomeSafe'), desc: t('welcomeSafeDesc') },
    { icon: Zap, text: t('welcomeFast'), desc: t('welcomeFastDesc') },
  ];

  return createPortal(
    // Internal sizes uniformly use em: body has already inherited :root's font size, so checkboxes/width scale with the global font size.
    <div className="fixed inset-0 z-[9999] flex items-center justify-center">
      <div
        className={`absolute inset-0 bg-black/40 backdrop-blur-sm glass-backdrop ${isVisible ? 'modal-overlay-in' : enteredRef.current ? 'modal-overlay-out' : 'opacity-0'}`}
        onClick={handleClose}
      />
      {/* Width scales with font size: about 400px at standard size, with a vw cap to handle small windows at large font sizes.
          Height likewise needs a viewport constraint, otherwise in short windows the bottom buttons get pushed out of view;
          overflow-hidden changed to overflow-y-auto so content scrolls inside the modal. */}
      <div
        className={`glass-dialog relative mx-[1em] w-[28.5em] max-w-[calc(100vw-2em)] max-h-[calc(100vh-2em)] overflow-y-auto rounded-[0.86em] border border-[var(--border-default)] bg-[var(--bg-card)] shadow-2xl ${isVisible ? 'modal-content-in' : enteredRef.current ? 'modal-content-out' : 'opacity-0'}`}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="p-[1.43em]">
          {/* Title area: icon and title on the same line, fixed close entry on the right */}
          <div className="flex items-start gap-[0.86em]">
            {/* Use the project's own logo (same asset as the splash screen), no longer a letter placeholder as icon.
                The logo itself has a green gradient background and rounded corners, so here we only control size, no background or radius. */}
            <img
              src="/logo.svg"
              alt=""
              aria-hidden="true"
              className="h-[3.14em] w-[3.14em] shrink-0 object-contain"
            />
            <div className="min-w-0 flex-1 pt-[0.14em]">
              <h2 className="text-[1.14em] font-semibold text-[var(--text-primary)]">
                {t('welcomeTitle')}
              </h2>
              <p className="mt-[0.14em] text-[0.86em] font-medium text-[var(--brand-green)]">
                {t('welcomeSubtitle')}
              </p>
            </div>
            <button
              onClick={handleClose}
              aria-label="close"
              className="-mr-[0.29em] -mt-[0.29em] flex h-[2em] w-[2em] shrink-0 items-center justify-center rounded-[0.57em] text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
            >
              <X className="h-[1.14em] w-[1.14em]" />
            </button>
          </div>

          {/* Description */}
          <p className="mt-[1.14em] text-[0.86em] leading-relaxed text-[var(--text-muted)]">
            {t('welcomeDescription')}
          </p>

          {/* Highlights: three-line list, separated by very faint dividers, closer to panel layout than side-by-side cards */}
          <div className="mt-[1.14em] divide-y divide-[var(--border-color)] border-y border-[var(--border-color)]">
            {features.map((feature) => (
              <div key={feature.text} className="flex items-center gap-[0.86em] py-[0.71em]">
                <div className="flex h-[2.29em] w-[2.29em] shrink-0 items-center justify-center rounded-[0.57em] bg-[var(--brand-green-10)]">
                  <feature.icon className="h-[1.14em] w-[1.14em] text-[var(--brand-green)]" />
                </div>
                <div className="min-w-0">
                  <p className="text-[0.86em] font-semibold text-[var(--text-primary)]">
                    {feature.text}
                  </p>
                  <p className="mt-[0.14em] text-[0.79em] leading-tight text-[var(--text-faint)]">
                    {feature.desc}
                  </p>
                </div>
              </div>
            ))}
          </div>
        </div>

        {/* Bottom action area: checkbox on the left, primary button on the right, consistent with settings modals */}
        <div className="glass-bar flex items-center justify-between gap-[0.86em] border-t border-[var(--border-color)] bg-[var(--bg-main)] px-[1.43em] py-[0.86em]">
          <label className="flex cursor-pointer select-none items-center gap-[0.57em]">
            <Checkbox
              checked={dontShowAgain}
              onChange={setDontShowAgain}
            />
            <span className="text-[0.86em] text-[var(--text-muted)]">{t('welcomeDismiss')}</span>
          </label>
          <button
            onClick={handleClose}
            className="rounded-[0.57em] bg-[var(--brand-green)] px-[1.14em] py-[0.57em] text-[0.86em] font-semibold text-white transition-colors hover:bg-[var(--brand-green-hover)]"
          >
            {t('welcomeStart')}
          </button>
        </div>
      </div>
    </div>,
    document.body
  );
}

/**
 * Checks whether the welcome modal should be shown
 */
export function shouldShowWelcome(): boolean {
  return localStorage.getItem(STORAGE_KEY) !== 'true';
}

/**
 * Resets the welcome modal state (for testing)
 */
export function resetWelcomeState(): void {
  localStorage.removeItem(STORAGE_KEY);
}
