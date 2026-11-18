// ============================================================================
// Disk switch dialog
//
// The top bar row is only tens of pixels tall; trying to fit a dropdown there meant font size, width, and option truncation never lined up;
// switching to an icon-click dialog lets each partition show its volume label, free/total space, and usage percentage in full.
//
// ⚠️ A position: fixed dialog must use createPortal to document.body:
// the top bar container has an entrance transform, so fixed degrades to absolute and the overlay cannot cover the whole window.
// ============================================================================

import { useEffect } from 'react';
import { createPortal } from 'react-dom';
import { AnimatePresence, motion } from 'framer-motion';
import { Check, X } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { driveOptionLabel } from './ui/DriveSelect';
import { formatSize } from '../utils/format';
import { MODAL_BACKDROP_MOTION, MODAL_CARD_MOTION } from '../utils/modalMotion';
import type { LocalDriveInfo } from '../api/commands';

interface DiskSwitchModalProps {
  isOpen: boolean;
  onClose: () => void;
  /** Info for all fixed disks, provided by the top bar's useLocalDrives, not re-read here */
  drives: LocalDriveInfo[];
  /** Drive letter currently shown, e.g. "C:" */
  selectedDrive: string;
  onSelect: (driveLetter: string) => void;
}

export function DiskSwitchModal({
  isOpen,
  onClose,
  drives,
  selectedDrive,
  onSelect,
}: DiskSwitchModalProps) {
  const { t } = useTranslation('common');

  // ESC to close: all dialogs use this key consistently with the rest of the project
  useEffect(() => {
    if (!isOpen) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [isOpen, onClose]);

  return createPortal(
    <AnimatePresence>
      {isOpen && (
        <motion.div
          {...MODAL_BACKDROP_MOTION}
          className="fixed inset-0 z-[10000] flex items-center justify-center bg-black/50 backdrop-blur-sm glass-backdrop"
          onClick={onClose}
        >
          <motion.div
            {...MODAL_CARD_MOTION}
            role="dialog"
            aria-modal="true"
            aria-label={t('diskSwitchTitle')}
            className="glass-dialog relative w-[30em] max-w-[calc(100vw-2em)] max-h-[calc(100vh-2em)] overflow-y-auto rounded-2xl bg-[var(--glass-modal-bg,var(--bg-card))] p-[1.43em] shadow-2xl"
            onClick={(event) => event.stopPropagation()}
          >
            <div className="mb-[1em] flex items-center justify-between gap-2">
              <h2 className="text-[1.07em] font-semibold text-[var(--text-primary)]">{t('diskSwitchTitle')}</h2>
              <button
                type="button"
                onClick={onClose}
                aria-label={t('close')}
                className="rounded-lg p-[0.35em] text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
              >
                <X className="h-[1.14em] w-[1.14em]" />
              </button>
            </div>

            {drives.length === 0 ? (
              <p className="py-[1.4em] text-center text-[0.93em] text-[var(--text-muted)]">{t('diskSwitchEmpty')}</p>
            ) : (
              <div className="space-y-[0.57em]">
                {drives.map((drive) => {
                  const isSelected = drive.drive_letter === selectedDrive;
                  return (
                    <button
                      key={drive.drive_letter}
                      type="button"
                      onClick={() => {
                        onSelect(drive.drive_letter);
                        onClose();
                      }}
                      className={`w-full rounded-xl border p-[0.86em] text-left transition-colors ${
                        isSelected
                          ? 'border-[var(--brand-green)] bg-[var(--brand-green)]/5'
                          : 'border-[var(--border-color)] hover:bg-[var(--bg-hover)]'
                      }`}
                    >
                      <div className="flex items-start justify-between gap-2">
                        <div className="min-w-0">
                          <p className="truncate font-medium text-[var(--text-primary)]">{driveOptionLabel(drive)}</p>
                          <p className="mt-0.5 tabular-nums text-[0.86em] text-[var(--text-muted)]">
                            {t('freeOfTotal', {
                              free: formatSize(drive.free_space),
                              total: formatSize(drive.total_space),
                            })}
                          </p>
                        </div>
                        {isSelected && <Check className="mt-0.5 h-[1.14em] w-[1.14em] shrink-0 text-[var(--brand-green)]" />}
                      </div>
                      {/* Usage bar: same color thresholds as the top bar, so the two read as the same kind of info */}
                      <div className="mt-[0.57em] h-1.5 overflow-hidden rounded-full bg-[var(--bg-hover)]">
                        <div
                          className={`h-full rounded-full ${
                            drive.usage_percent > 90 ? 'bg-[var(--color-danger)]' :
                            drive.usage_percent > 75 ? 'bg-[var(--color-warning)]' : 'bg-[var(--brand-green)]'
                          }`}
                          style={{ width: `${drive.usage_percent}%` }}
                        />
                      </div>
                    </button>
                  );
                })}
              </div>
            )}
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>,
    document.body,
  );
}
