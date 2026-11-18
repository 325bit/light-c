// ============================================================================
// Portable edition update guidance dialog
//
// The portable edition can't use installer-style auto-update, or it would silently install the NSIS package into the system. This dialog only does second confirmation
// and channel guidance: it tells users the portable edition requires manually replacing files, and offers two official entries, website and net disk.
// Kept consistent with ClearLocalDataDialog, both use framer-motion for enter/exit animations.
// ============================================================================

import { useEffect } from 'react';
import { createPortal } from 'react-dom';
import { AnimatePresence, motion } from 'framer-motion';
import { X, Globe, HardDriveDownload, RefreshCw } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { MODAL_BACKDROP_MOTION, MODAL_CARD_MOTION } from '../../utils/modalMotion';

export interface PortableUpdateDialogProps {
  isOpen: boolean;
  /** Remote latest version string; null when not found, in which case only channel guidance is shown. */
  latestVersion: string | null;
  /** Current application version. */
  currentVersion: string;
  /** Currently querying the remote version. */
  isChecking: boolean;
  /** Whether the version check failed (on failure, don't falsely claim "up to date"). */
  checkFailed?: boolean;
  /** Author's net-disk entry, taken from the official download.json, falls back to a built-in address on failure. */
  netDiskUrl: string;
  /** Official website entry. */
  officialWebsiteUrl: string;
  /** Callback when a channel button is clicked; the caller opens the external link. */
  onOpenChannel: (url: string, channel: 'website' | 'netDisk') => void;
  onClose: () => void;
}

export function PortableUpdateDialog({
  isOpen,
  latestVersion,
  currentVersion,
  isChecking,
  checkFailed = false,
  netDiskUrl,
  officialWebsiteUrl,
  onOpenChannel,
  onClose,
}: PortableUpdateDialogProps) {
  const { t } = useTranslation('ui');
  const { t: commonT } = useTranslation('common');

  useEffect(() => {
    if (!isOpen) return;
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [isOpen, onClose]);

  // Three states degrade in order: checking → new version found → up to date / check failed.
  const subtitle = isChecking && !latestVersion
    ? t('portableUpdateChecking')
    : latestVersion
      ? t('portableUpdateCurrent', { version: currentVersion })
      : checkFailed
        ? t('portableUpdateCheckFailed')
        : t('portableUpdateUpToDate', { version: currentVersion });

  return createPortal(
    <AnimatePresence>
      {isOpen && (
        <motion.div
          className="fixed inset-0 z-[10000] flex items-center justify-center"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.16, ease: 'easeOut' }}
        >
          <motion.div
            className="absolute inset-0 bg-black/50 backdrop-blur-sm glass-backdrop"
            onClick={onClose}
            {...MODAL_BACKDROP_MOTION}
          />

          {/* Sizes all use em: body inherits the :root font size, so dialog width and height scale with the global font setting.
              Height must also be clamped by the viewport, otherwise in a short window the bottom buttons get pushed out of view;
              changed overflow-hidden to overflow-y-auto so content scrolls inside the dialog. */}
          <motion.div
            className="relative mx-[1em] w-[31.43em] max-w-[calc(100vw-2em)] max-h-[calc(100vh-2em)] overflow-y-auto rounded-[0.86em] border border-[var(--border-default)] bg-[var(--bg-card)] glass-dialog shadow-2xl"
            {...MODAL_CARD_MOTION}
          >
            <button
              type="button"
              onClick={onClose}
              aria-label={commonT('close')}
              className="absolute right-[1em] top-[1em] z-10 flex h-[2em] w-[2em] items-center justify-center rounded-[0.57em] text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
            >
              <X className="h-[1.14em] w-[1.14em]" />
            </button>

            <div className="p-[1.43em]">
              {/* Title area: light-green icon background + version info, same visual language as the main UI's module cards */}
              <div className="mb-[1.14em] flex items-start gap-[0.86em]">
                <div className="flex h-[2.57em] w-[2.57em] shrink-0 items-center justify-center rounded-[0.71em] bg-[var(--brand-green-10)]">
                  <HardDriveDownload className="h-[1.29em] w-[1.29em] text-[var(--brand-green)]" />
                </div>
                <div className="min-w-0 flex-1">
                  <h2 className="text-[1em] font-semibold text-[var(--text-primary)]">
                    {latestVersion ? t('portableUpdateFound', { version: latestVersion }) : t('portableUpdateTitle')}
                  </h2>
                  <p className="mt-[0.14em] flex items-center gap-[0.43em] text-[0.86em] text-[var(--text-muted)]">
                    {isChecking && <RefreshCw className="h-[0.86em] w-[0.86em] shrink-0 animate-spin" />}
                    {subtitle}
                  </p>
                </div>
              </div>

              <p className="text-[0.86em] leading-relaxed text-[var(--text-secondary)]">
                {t('portableUpdateIntro')}
              </p>

              <div className="mt-[1.43em] flex flex-col gap-[0.57em] sm:flex-row">
                <button
                  type="button"
                  onClick={() => onOpenChannel(officialWebsiteUrl, 'website')}
                  className="flex flex-1 items-center justify-center gap-[0.43em] rounded-[0.71em] bg-[var(--brand-green)] px-[1em] py-[0.64em] text-[0.86em] font-semibold text-white transition-colors hover:bg-[var(--brand-green-hover)]"
                >
                  <Globe className="h-[0.86em] w-[0.86em]" />
                  {t('portableUpdateWebsite')}
                </button>
                <button
                  type="button"
                  onClick={() => onOpenChannel(netDiskUrl, 'netDisk')}
                  className="flex flex-1 items-center justify-center gap-[0.43em] rounded-[0.71em] border border-[var(--border-default)] bg-[var(--bg-card)] px-[1em] py-[0.64em] text-[0.86em] font-medium text-[var(--text-secondary)] transition-colors hover:bg-[var(--bg-hover)]"
                >
                  <HardDriveDownload className="h-[0.86em] w-[0.86em]" />
                  {t('portableUpdateNetDisk')}
                </button>
              </div>

              <p className="mt-[0.86em] text-[0.79em] leading-relaxed text-[var(--text-faint)]">
                {t('portableUpdateHint')}
              </p>
            </div>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>,
    document.body,
  );
}

export default PortableUpdateDialog;
