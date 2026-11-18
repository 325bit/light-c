// ============================================================================
// Scan timing details popover
//
// Numbers like phase durations and $MFT segments / disk reads / parsing are the basis for troubleshooting and further optimization,
// but they are noise for ordinary users - after release, tuck them all into this "little stopwatch" button, visible only when clicked.
//
// Why click instead of hover: a hover popover disappears as soon as you press a screenshot shortcut (native title has the same problem),
// and this is exactly the data people want to screenshot for feedback. Once clicked open, the mouse can move away and you can screenshot freely.
// ============================================================================

import { useEffect, useRef, useState } from 'react';
import { AnimatePresence, motion } from 'framer-motion';
import { Timer } from 'lucide-react';
import { useTranslation } from 'react-i18next';

export interface TimingRow {
  label: string;
  value: string;
}

export interface TimingGroup {
  /** Group title (e.g., "C: drive"); pass an empty string to omit the title row */
  title: string;
  rows: TimingRow[];
  /** Group note, suited for a one-line long text such as "MFT segments 11 · read 2.4s, parse 1.1s" */
  note?: string;
}

interface ScanTimingPopoverProps {
  groups: TimingGroup[];
  /** Whether the popover hugs the left or right side; use right near the page's right edge to avoid overflow */
  align?: 'left' | 'right';
}

export function ScanTimingPopover({ groups, align = 'right' }: ScanTimingPopoverProps) {
  const { t } = useTranslation('common');
  const [open, setOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  // Close on blank click / Esc: the popover is not modal, so do not force the user to hunt for a close button
  useEffect(() => {
    if (!open) return;

    const handlePointerDown = (event: MouseEvent) => {
      if (!containerRef.current?.contains(event.target as Node)) {
        setOpen(false);
      }
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        setOpen(false);
      }
    };

    document.addEventListener('mousedown', handlePointerDown);
    document.addEventListener('keydown', handleKeyDown);
    return () => {
      document.removeEventListener('mousedown', handlePointerDown);
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, [open]);

  // Do not show at all when there are no numbers to display, to avoid leaving a button that shows nothing when clicked
  const hasContent = groups.some((group) => group.rows.length > 0 || Boolean(group.note));
  if (!hasContent) return null;

  return (
    <div ref={containerRef} className="relative inline-flex">
      <button
        type="button"
        onClick={() => setOpen((previous) => !previous)}
        aria-expanded={open}
        aria-label={t('timingDetails')}
        title={t('timingDetails')}
        className={`inline-flex h-5 w-5 items-center justify-center rounded-md transition ${
          open
            ? 'bg-[var(--bg-hover)] text-[var(--brand-green)]'
            : 'text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--brand-green)]'
        }`}
      >
        <Timer className="h-3.5 w-3.5" />
      </button>

      <AnimatePresence>
        {open && (
          <motion.div
            initial={{ opacity: 0, y: -4, scale: 0.98 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: -4, scale: 0.98 }}
            transition={{ duration: 0.18, ease: [0.22, 1, 0.36, 1] }}
            className={`absolute top-full z-40 mt-1 w-[18rem] ${align === 'right' ? 'right-0' : 'left-0'}`}
          >
            <div className="glass-overlay rounded-xl border border-[var(--border-color)] bg-[var(--bg-card)] p-3 text-xs shadow-lg">
              <div className="mb-2 flex items-center justify-between gap-3">
                <span className="font-semibold text-[var(--text-primary)]">{t('timingDetails')}</span>
                <span className="text-[var(--text-faint)]">{t('timingDetailsHint')}</span>
              </div>
              <div className="max-h-96 space-y-2 overflow-y-auto pr-1">
                {groups.map((group) => (
                  <div key={group.title || group.note} className="space-y-1">
                    {group.title && (
                      <div className="font-medium text-[var(--text-muted)]">{group.title}</div>
                    )}
                    {group.rows.map((row) => (
                      <div
                        key={row.label}
                        className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-2"
                      >
                        <span className="truncate text-[var(--text-muted)]">{row.label}</span>
                        <span className="tabular-nums text-[var(--text-primary)]">{row.value}</span>
                      </div>
                    ))}
                    {group.note && (
                      <div className="leading-snug text-[var(--text-faint)]">{group.note}</div>
                    )}
                  </div>
                ))}
              </div>
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}
