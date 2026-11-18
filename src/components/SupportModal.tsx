// ============================================================================
// Support author modal
//
// The sidebar heart icon and "Settings - Feedback - Support Author" share this one modal,
// the QR code, payment-method switching, and zoomed preview have only this single implementation.
//
// The title and the settings-page card share `supportTitle` (value consistently "Sponsor"): the sidebar tooltip says "Sponsor LightC",
// the modal title when opened is "Sponsor", and opening from the settings page uses the same word — three names for one thing are avoided.
//
// ⚠️ Do not copy another modal into the caller: QR image, switch animation and ESC-close scattered in two places
// means changing once requires changing twice, and they will sooner or later drift apart.
// ============================================================================

import { useEffect, useState } from 'react';
import { createPortal } from 'react-dom';
import { AnimatePresence, motion } from 'framer-motion';
import { Check, X } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import wechatQr from '../assets/r_wechat_qr.jpg';
import alipayQr from '../assets/r_alipay_qr.jpg';
import { MODAL_BACKDROP_MOTION, MODAL_CARD_MOTION } from '../utils/modalMotion';

export type PaymentType = 'wechat' | 'alipay';

/**
 * Size of the donation QR code in the modal (pixels).
 *
 * The settings-page card is 176px; the modal must be noticeably larger than that to count as a "zoom".
 * Use pixels rather than rem: the project root font size is changed to 14px by the font-size setting, so Tailwind's rem sizes scale down,
 * and once the code shrinks below 96px it can no longer be scanned.
 */
const QR_MODAL_SIZE = 220;

/** Payment-method metadata: colors and text are defined only here, shared by all entry points */
export const PAYMENT_OPTIONS: { type: PaymentType; activeClass: string; labelKey: string }[] = [
  { type: 'wechat', activeClass: 'bg-[#07C160]', labelKey: 'wechat' },
  { type: 'alipay', activeClass: 'bg-[#1677FF]', labelKey: 'alipay' },
];

interface SupportModalProps {
  isOpen: boolean;
  onClose: () => void;
  /** The payment method selected by default when opening, decided by the caller (the settings page passes the one currently selected on the card) */
  initialPayment?: PaymentType;
  /** Version number shown at the bottom; passed in by the sidebar entry, not passed by the settings-page entry */
  version?: string;
}

export function SupportModal({ isOpen, onClose, initialPayment = 'wechat', version }: SupportModalProps) {
  const { t } = useTranslation('common');
  const [paymentType, setPaymentType] = useState<PaymentType>(initialPayment);
  const [isTransitioning, setIsTransitioning] = useState(false);

  // Align to the caller's selection every time it opens: if a user switches to Alipay on the settings card and then opens,
  // the modal still sitting on WeChat would make them think the switch did not take effect.
  useEffect(() => {
    if (isOpen) setPaymentType(initialPayment);
  }, [isOpen, initialPayment]);

  // When switching payment methods, fade out before swapping the image: swapping src directly causes an abrupt jump, harsh visually
  const handlePaymentChange = (type: PaymentType) => {
    if (type === paymentType) return;
    setIsTransitioning(true);
    window.setTimeout(() => {
      setPaymentType(type);
      setIsTransitioning(false);
    }, 150);
  };

  // Close on ESC. Only attach the listener while open, to avoid keeping it resident.
  useEffect(() => {
    if (!isOpen) return;
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [isOpen, onClose]);

  const qrAlt = paymentType === 'wechat' ? t('wechatQr') : t('alipayQr');
  const qrSrc = paymentType === 'wechat' ? wechatQr : alipayQr;

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
            aria-label={t('supportTitle')}
            className="glass-dialog relative w-[26em] max-w-[calc(100vw-2em)] max-h-[calc(100vh-2em)] overflow-y-auto rounded-2xl bg-[var(--glass-modal-bg,var(--bg-card))] p-[1.43em] shadow-2xl"
            onClick={(event) => event.stopPropagation()}
          >
            <button
              type="button"
              onClick={onClose}
              aria-label={t('close')}
              className="absolute right-[0.86em] top-[0.86em] rounded-lg p-[0.35em] text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
            >
              <X className="h-[1.14em] w-[1.14em]" />
            </button>

            <h3 className="mb-[1em] pr-[2em] text-[1.14em] font-semibold text-[var(--text-primary)]">
              {t('supportTitle')}
            </h3>

            {/* Vertical layout: donation code → switch → description. WeChat/Alipay donation pages all use this structure,
                a horizontal layout squeezes the description to a few characters per line, tiring to read. */}
            <div className="flex flex-col items-center">
              <div
                style={{ width: QR_MODAL_SIZE, height: QR_MODAL_SIZE }}
                className="overflow-hidden rounded-xl border border-[var(--border-color)] bg-white"
              >
                <img
                  src={qrSrc}
                  alt={qrAlt}
                  className={`h-full w-full object-contain transition-opacity duration-150 ${
                    isTransitioning ? 'opacity-0' : 'opacity-100'
                  }`}
                />
              </div>

              <div className="mt-[0.86em] inline-flex rounded-xl border border-[var(--border-color)] bg-[var(--bg-card)] p-1">
                {PAYMENT_OPTIONS.map((option) => (
                  <button
                    key={option.type}
                    type="button"
                    onClick={() => handlePaymentChange(option.type)}
                    className={`rounded-lg px-4 py-1.5 text-xs font-medium transition-all duration-200 ${
                      paymentType === option.type
                        ? `${option.activeClass} text-white shadow-sm`
                        : 'text-[var(--text-muted)] hover:text-[var(--text-primary)]'
                    }`}
                  >
                    {t(option.labelKey)}
                  </button>
                ))}
              </div>

              <p className="mt-[1em] text-center text-sm leading-relaxed text-[var(--text-secondary)]">
                {t('supportDesc')}
              </p>
              <ul className="mt-[0.71em] w-full space-y-[0.5em] text-xs text-[var(--text-muted)]">
                <li className="flex items-start gap-[0.5em]">
                  <Check className="mt-px h-3.5 w-3.5 shrink-0 text-[var(--brand-green)]" />
                  <span>{t('supportPointDev')}</span>
                </li>
                <li className="flex items-start gap-[0.5em]">
                  <Check className="mt-px h-3.5 w-3.5 shrink-0 text-[var(--brand-green)]" />
                  <span>{t('supportPointVoluntary')}</span>
                </li>
              </ul>
            </div>

            {/* Keep the version number here: that spot in the sidebar was replaced by the heart icon,
                and the screenshots users most often attach when reporting a problem are the sidebar or this modal, so the version number must not disappear entirely. */}
            {version && (
              <p className="mt-[1.14em] text-center text-[0.71em] text-[var(--text-faint)] tabular-nums">
                LightC v{version}
              </p>
            )}
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>,
    document.body
  );
}
