// ============================================================================
// Support author component (the card in Settings - Feedback)
//
// The card itself only "shows the donation code + switching"; the zoomed view after clicking is provided by `SupportModal`.
// The sidebar heart icon uses the same modal, so the QR code and switching logic exist in only one place.
// ============================================================================

import { useState } from 'react';
import { Check, Coffee } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import wechatQr from '../../assets/r_wechat_qr.jpg';
import alipayQr from '../../assets/r_alipay_qr.jpg';
import { PAYMENT_OPTIONS, SupportModal, type PaymentType } from '../SupportModal';

/**
 * Display size of the donation code in the card (pixels).
 * The project root font size is changed to 14px by the font-size setting, so Tailwind's rem sizes scale down (w-32 is actually only 112px),
 * and once the QR code shrinks below 96px it can no longer be recognized, so a pixel value that does not scale with font size is fixed here.
 */
const QR_DISPLAY_SIZE = 176;

export function SupportAuthor() {
  const { t } = useTranslation('common');
  const [paymentType, setPaymentType] = useState<PaymentType>('wechat');
  const [isTransitioning, setIsTransitioning] = useState(false);
  const [isModalOpen, setIsModalOpen] = useState(false);

  // When switching payment methods, fade out before swapping the image: swapping src directly causes an abrupt jump, harsh visually
  const handlePaymentChange = (type: PaymentType) => {
    if (type === paymentType) return;
    setIsTransitioning(true);
    setTimeout(() => {
      setPaymentType(type);
      setIsTransitioning(false);
    }, 150);
  };

  const qrAlt = paymentType === 'wechat' ? t('wechatQr') : t('alipayQr');
  const qrSrc = paymentType === 'wechat' ? wechatQr : alipayQr;

  return (
    <>
      <div className="space-y-3">
        <h4 className="text-xs font-medium text-[var(--text-muted)] uppercase tracking-wider flex items-center gap-2">
          <Coffee className="w-3.5 h-3.5" />
          {t('supportTitle')}
        </h4>
        <div className="glass-block @container bg-[var(--bg-main)] rounded-2xl p-5">
          {/* Two columns: the left holds the donation code and payment-method switch (both part of "scan to support"), the right holds only text.
              Container queries are used rather than viewport breakpoints: the settings modal occupies only 76% of the viewport and the right content area is narrower,
              so judging by viewport width would squeeze the text into a few characters per line in medium windows. */}
          <div className="flex flex-col @[24rem]:flex-row items-center @[24rem]:items-start gap-5">
            {/* Left column: donation code, scan hint, payment-method switch */}
            <div className="flex flex-col items-center shrink-0">
              <div
                onClick={() => setIsModalOpen(true)}
                style={{ width: QR_DISPLAY_SIZE, height: QR_DISPLAY_SIZE }}
                className="relative rounded-xl border border-[var(--border-color)] overflow-hidden bg-white cursor-pointer hover:shadow-lg hover:border-[var(--brand-green)] transition-all duration-200 group"
              >
                <img
                  src={qrSrc}
                  alt={qrAlt}
                  className={`w-full h-full object-contain transition-opacity duration-150 ${isTransitioning ? 'opacity-0' : 'opacity-100'}`}
                />
                {/* Hover-to-enlarge hint */}
                <div className="absolute inset-0 bg-black/0 group-hover:bg-black/10 transition-colors duration-200 flex items-center justify-center">
                  <div className="opacity-0 group-hover:opacity-100 transition-opacity duration-200 bg-black/60 text-white text-[0.71rem] px-2 py-1 rounded-full">
                    {t('zoom')}
                  </div>
                </div>
              </div>
              <p className="text-[0.71rem] text-[var(--text-faint)] text-center mt-2">
                {t('supportQrHint')}
              </p>

              {/* Payment-method switch: in the same column as the donation code, the result applies directly to the image above */}
              <div className="mt-3 inline-flex bg-[var(--bg-card)] rounded-xl p-1 border border-[var(--border-color)]">
                {PAYMENT_OPTIONS.map((option) => (
                  <button
                    key={option.type}
                    onClick={() => handlePaymentChange(option.type)}
                    className={`px-4 py-1.5 text-xs font-medium rounded-lg transition-all duration-200 ${
                      paymentType === option.type
                        ? `${option.activeClass} text-white shadow-sm`
                        : 'text-[var(--text-muted)] hover:text-[var(--text-primary)]'
                    }`}
                  >
                    {t(option.labelKey)}
                  </button>
                ))}
              </div>
            </div>

            {/* Right column: description text + support highlights */}
            <div className="min-w-0 flex-1 space-y-3">
              <p className="text-sm leading-relaxed text-[var(--text-secondary)]">
                {t('supportDesc')}
              </p>
              <ul className="space-y-1.5 text-xs text-[var(--text-muted)]">
                <li className="flex items-start gap-1.5">
                  <Check className="w-3.5 h-3.5 mt-px shrink-0 text-[var(--brand-green)]" />
                  <span>{t('supportPointDev')}</span>
                </li>
                <li className="flex items-start gap-1.5">
                  <Check className="w-3.5 h-3.5 mt-px shrink-0 text-[var(--brand-green)]" />
                  <span>{t('supportPointVoluntary')}</span>
                </li>
              </ul>
            </div>
          </div>
        </div>
      </div>

      {/* Zoomed view: shares the same modal component as the sidebar heart icon.
          Carry over the payment method currently selected on the card, otherwise the modal stays on WeChat and makes users think the switch did not take effect. */}
      <SupportModal
        isOpen={isModalOpen}
        onClose={() => setIsModalOpen(false)}
        initialPayment={paymentType}
      />
    </>
  );
}
