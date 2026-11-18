// ============================================================================
// Back to top button
// Fades in slowly after the main content scrolls a certain distance; clicking smoothly returns to the top
// ============================================================================

import { useEffect, useState } from 'react';
import { ArrowUp } from 'lucide-react';
import { AnimatePresence, motion } from 'framer-motion';
import { useTranslation } from 'react-i18next';

interface BackToTopButtonProps {
  /** Main content scroll container; passed in by the caller, the component does not query the DOM itself. */
  scrollContainerRef: React.RefObject<HTMLElement | null>;
  /** Show the button after this scroll distance; by default avoids accidental triggers from slight scrolling on page entry. */
  threshold?: number;
}

export function BackToTopButton({ scrollContainerRef, threshold = 360 }: BackToTopButtonProps) {
  const { t } = useTranslation('common');
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    const container = scrollContainerRef.current;
    if (!container) return;

    const updateVisibility = () => {
      setVisible(container.scrollTop > threshold);
    };

    updateVisibility();
    container.addEventListener('scroll', updateVisibility, { passive: true });
    return () => container.removeEventListener('scroll', updateVisibility);
  }, [scrollContainerRef, threshold]);

  const handleBackToTop = () => {
    const container = scrollContainerRef.current;
    if (!container) return;

    // Use native smooth scroll, to avoid burdening long-list pages with our own frame-by-frame calculations.
    container.scrollTo({ top: 0, behavior: 'smooth' });
  };

  return (
    // top-1/2
    <AnimatePresence>
      {visible && (
        /* ⚠️ z-index z-30: **deliberately lower** than the AI assistant panel (z-40).
           This button is in the bottom-right corner, exactly the area the assistant panel covers when expanded;
           writing z-50 before made it "poke through" above the panel, blocking content and easy to misclick.
           It is still above module content and the module action bar (z-20), so it shows up when it should.

           ⚠️ The comment position matters: this is at a JS expression position inside {visible && ( … )},
           so only this form works; a brace-wrapped JSX comment would be parsed as an object literal
           and fail to compile. Conversely, at a JSX child position you must use the brace form,
           otherwise the comment is rendered as text into the UI. */
        <motion.button
          type="button"
          onClick={handleBackToTop}
          className="
            glass-overlay fixed right-8 bottom-10 z-30 flex h-8 w-8 items-center justify-center rounded-lg
            border border-[var(--brand-green-20)] bg-[var(--bg-card)]/95 text-[var(--brand-green)]
            shadow-lg shadow-black/10 backdrop-blur-md transition-colors
            hover:border-[var(--brand-green)] hover:bg-[var(--brand-green)] hover:text-white
            active:scale-95
          "
          title={t('backToTop')}
          aria-label={t('backToTop')}
          initial={{ opacity: 0, y: 16, scale: 0.92 }}
          animate={{ opacity: 1, y: 0, scale: 1 }}
          exit={{ opacity: 0, y: 12, scale: 0.94 }}
          transition={{ duration: 0.28, ease: [0.22, 1, 0.36, 1] }}
        >
          <ArrowUp className="h-5 w-5" />
        </motion.button>
      )}
    </AnimatePresence>
  );
}

export default BackToTopButton;
