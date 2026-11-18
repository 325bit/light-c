// ============================================================================
// Settings modal component
// Only handles the modal lifecycle, left navigation and page routing; the concrete settings pages are split and maintained by feature.
// ============================================================================

import { useCallback, useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type PointerEvent as ReactPointerEvent } from 'react';
import { createPortal } from 'react-dom';
import { X } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { FONT_SIZE_CONFIGS, useFontSize, useTheme } from '../contexts';
import { AboutSettings } from './settings/AboutSettings';
import { FeedbackSettings } from './settings/FeedbackSettings';
import { FeatureSettings } from './settings/FeatureSettings';
import { GeneralSettings } from './settings/GeneralSettings';
import { AiSettings } from './settings/AiSettings';
import { BetaBadge } from './BetaBadge';
import { GuideSettings } from './settings/GuideSettings';
import { SecuritySettings } from './settings/SecuritySettings';
import { DiskInfoSettings } from './settings/DiskInfoSettings';
import { SETTINGS_TABS } from './settings/constants';
import type { SettingsTab } from './settings/types';

interface SettingsModalProps {
  isOpen: boolean;
  onClose: () => void;
}

// The nav width is saved in pixels at standard font size and multiplied by the font scale at render time, matching the main UI sidebar:
// a pixel value does not drift when the window resizes, while a ratio would squeeze the menu until text no longer fits in narrow windows.
const SETTINGS_NAV_WIDTH_STORAGE_KEY = 'lightc.settings.navWidth';
const SETTINGS_NAV_WIDTH_DEFAULT = 220;
const SETTINGS_NAV_WIDTH_MIN = 180;
const SETTINGS_NAV_WIDTH_MAX = 360;
// Keyboard adjustment step (pixels at standard font size), so width can still be fine-tuned while focus is on the divider.
const SETTINGS_NAV_KEYBOARD_STEP = 12;
// The maximum proportion of the modal width the nav may take when the window narrows, preventing a saved larger width from squeezing out the right content.
const SETTINGS_NAV_MAX_WIDTH_RATIO = 0.4;
// The modal is 76vw, capped at viewport - 24px; the nav cap is converted back to standard-font pixels from the viewport width.
const SETTINGS_MODAL_VIEWPORT_WIDTH_RATIO = 0.76;
const SETTINGS_MODAL_VIEWPORT_WIDTH_MARGIN = 24;

/** Compute the display-width cap of the nav for the current viewport (pixels): the smaller the window, the lower the proportion the nav may take. */
function getSettingsNavMaxRenderedWidth(viewportWidth: number): number {
  const safeViewportWidth = Math.max(1, Math.round(viewportWidth));
  const modalWidth = Math.min(
    safeViewportWidth * SETTINGS_MODAL_VIEWPORT_WIDTH_RATIO,
    safeViewportWidth - SETTINGS_MODAL_VIEWPORT_WIDTH_MARGIN,
  );
  // Use the display width for the ratio constraint, so a minimum width enlarged at large font sizes does not squeeze out the right content.
  return Math.round(modalWidth * SETTINGS_NAV_MAX_WIDTH_RATIO);
}

/** Convert the display-width cap back to a drag cap at standard font size, and clamp it within the global lower/upper bounds. */
function getSettingsNavMaxStoredWidth(viewportWidth: number, fontScale: number): number {
  const ratioLimitedWidth = Math.round(getSettingsNavMaxRenderedWidth(viewportWidth) / fontScale);
  // At a very small viewport the ratio cap may fall below the minimum width; in that case the lower bound keeps the minimum width so menu text stays readable.
  return Math.min(SETTINGS_NAV_WIDTH_MAX, Math.max(SETTINGS_NAV_WIDTH_MIN, ratioLimitedWidth));
}

/** Read the saved nav width; on out-of-range values or unavailable storage fall back to the default, avoiding dirty data breaking the modal layout. */
function readSettingsNavWidth(): number {
  try {
    const savedWidth = Number(localStorage.getItem(SETTINGS_NAV_WIDTH_STORAGE_KEY));
    return Number.isFinite(savedWidth) && savedWidth > 0
      ? Math.min(SETTINGS_NAV_WIDTH_MAX, Math.max(SETTINGS_NAV_WIDTH_MIN, Math.round(savedWidth)))
      : SETTINGS_NAV_WIDTH_DEFAULT;
  } catch (error) {
    console.warn('读取设置导航宽度失败:', error);
    return SETTINGS_NAV_WIDTH_DEFAULT;
  }
}

/** Save the nav width; a write failure only affects next startup's width, the current session's drag result remains valid. */
function writeSettingsNavWidth(width: number): void {
  try {
    localStorage.setItem(SETTINGS_NAV_WIDTH_STORAGE_KEY, String(width));
  } catch (error) {
    console.warn('保存设置导航宽度失败:', error);
  }
}

export function SettingsModal({ isOpen, onClose }: SettingsModalProps) {
  const { t } = useTranslation('settings');
  const [activeTab, setActiveTab] = useState<SettingsTab>('general');
  const { mode, setMode } = useTheme();
  const { level, customFontSize } = useFontSize();
  const [isVisible, setIsVisible] = useState(false);
  const [isAnimating, setIsAnimating] = useState(false);
  const [navWidth, setNavWidth] = useState(readSettingsNavWidth);
  const [isResizingNav, setIsResizingNav] = useState(false);
  const [viewportWidth, setViewportWidth] = useState(() => window.innerWidth);
  // Track whether it has ever been visible, to distinguish initial pre-hide on mount from the closing animation.
  const enteredRef = useRef(false);
  // During dragging read the latest width via a ref, avoiding putting width in the event listener dependencies which would rebuild the listener repeatedly.
  const navWidthRef = useRef(navWidth);
  const resizeStartRef = useRef<{ x: number; width: number } | null>(null);
  if (isVisible) enteredRef.current = true;

  const fontScale = (level === 'custom' ? customFontSize : FONT_SIZE_CONFIGS[level].baseSize + FONT_SIZE_CONFIGS[level].offset) / FONT_SIZE_CONFIGS.standard.baseSize;
  const navMaxStoredWidth = getSettingsNavMaxStoredWidth(viewportWidth, fontScale);
  const navMaxRenderedWidth = getSettingsNavMaxRenderedWidth(viewportWidth);
  // A saved width may exceed the current cap in narrow windows; clamp it for display, but do not overwrite the stored value.
  const renderedNavWidth = Math.min(
    Math.round(Math.min(navWidth, navMaxStoredWidth) * fontScale),
    navMaxRenderedWidth,
  );
  // The minimum width is only a drag lower bound and is not part of display clamping: when it does not fit at a large font size, prioritize the right content.
  const renderedNavWidthMin = Math.round(SETTINGS_NAV_WIDTH_MIN * fontScale);
  const renderedNavWidthMax = navMaxRenderedWidth;

  useEffect(() => {
    navWidthRef.current = navWidth;
  }, [navWidth]);

  // The viewport width determines the nav's proportion cap; it must be recomputed when the window resizes, otherwise narrow windows keep an over-wide nav.
  useEffect(() => {
    const handleViewportResize = () => setViewportWidth(window.innerWidth);
    window.addEventListener('resize', handleViewportResize);
    return () => window.removeEventListener('resize', handleViewportResize);
  }, []);

  useEffect(() => {
    if (isOpen) {
      setIsAnimating(true);
      setIsVisible(true);
      return;
    }

    setIsVisible(false);
    const timer = setTimeout(() => setIsAnimating(false), 190);
    return () => clearTimeout(timer);
  }, [isOpen]);

  // Dragging and keyboard adjustment share the same clamping logic, so the two entry points never disagree on width.
  // The cap is the current window's maximum, so in narrow windows you cannot drag to the full width, and after enlarging the window you can keep dragging wider.
  const applyNavWidth = useCallback((nextWidth: number) => {
    const clampedWidth = Math.min(navMaxStoredWidth, Math.max(SETTINGS_NAV_WIDTH_MIN, Math.round(nextWidth)));
    navWidthRef.current = clampedWidth;
    setNavWidth(clampedWidth);
  }, [navMaxStoredWidth]);

  useEffect(() => {
    const handlePointerMove = (event: PointerEvent) => {
      const resizeStart = resizeStartRef.current;
      if (!resizeStart) return;

      // Mouse movement is converted back by the font scale to a width at standard font size, keeping the drag feel consistent across font sizes.
      applyNavWidth(resizeStart.width + (event.clientX - resizeStart.x) / fontScale);
    };

    const handlePointerUp = () => {
      if (!resizeStartRef.current) return;
      resizeStartRef.current = null;
      // Write to storage only once when the drag ends, avoiding high-frequency writes during pointermove.
      writeSettingsNavWidth(navWidthRef.current);
      setIsResizingNav(false);
    };

    window.addEventListener('pointermove', handlePointerMove);
    window.addEventListener('pointerup', handlePointerUp);
    // End the drag on pointer cancel or window blur as well, otherwise the state stays stuck in "dragging".
    window.addEventListener('pointercancel', handlePointerUp);
    window.addEventListener('blur', handlePointerUp);
    return () => {
      window.removeEventListener('pointermove', handlePointerMove);
      window.removeEventListener('pointerup', handlePointerUp);
      window.removeEventListener('pointercancel', handlePointerUp);
      window.removeEventListener('blur', handlePointerUp);
    };
  }, [applyNavWidth, fontScale]);

  const handleResizeStart = (event: ReactPointerEvent<HTMLDivElement>) => {
    event.preventDefault();
    resizeStartRef.current = { x: event.clientX, width: navWidthRef.current };
    setIsResizingNav(true);
  };

  const handleResizeKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
    event.preventDefault();
    const direction = event.key === 'ArrowRight' ? 1 : -1;
    applyNavWidth(navWidth + direction * SETTINGS_NAV_KEYBOARD_STEP);
    // Keyboard adjustment has no drag-end event, so it must be persisted immediately.
    writeSettingsNavWidth(navWidthRef.current);
  };

  if (!isOpen && !isAnimating) return null;

  return createPortal(
    <div className="fixed inset-0 z-[9999] flex items-center justify-center">
      <div
        className={`absolute inset-0 bg-black/40 backdrop-blur-sm glass-backdrop ${isVisible ? 'modal-overlay-in' : enteredRef.current ? 'modal-overlay-out' : 'opacity-0'}`}
        onClick={onClose}
      />

      <div className={`glass-dialog relative h-[80vh] w-[76vw] min-h-0 min-w-0 max-h-[calc(100vh-24px)] max-w-[calc(100vw-24px)] overflow-hidden rounded-2xl bg-[var(--bg-card)] shadow-2xl ${isVisible ? 'modal-content-in' : enteredRef.current ? 'modal-content-out' : 'opacity-0'}`}>
        <div className={`flex h-full${isResizingNav ? ' settings-modal--resizing' : ''}`}>
          <aside
            className="glass-nav-pane relative shrink-0 border-r border-[var(--border-color)] bg-[var(--bg-main)] py-4"
            style={{ width: renderedNavWidth }}
          >
            <div className="mb-4 px-4">
              <h2 className="text-sm font-semibold text-[var(--text-primary)]">{t('title')}</h2>
            </div>
            <nav className="space-y-1 px-2">
              {SETTINGS_TABS.map(({ id, label, icon: Icon }) => (
                <button
                  key={id}
                  type="button"
                  onClick={() => setActiveTab(id)}
                  /* The selected state gets an extra semantic class: under the glass theme it must switch to a "glass capsule",
                     with styles written under html.glass in App.css — no appearance-toggle check in JSX,
                     and when glass is off this class has no styles, so it is preserved as-is. */
                  className={`settings-tab${activeTab === id ? ' settings-tab--active' : ''} flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-sm transition-colors ${activeTab === id
                    ? 'bg-[var(--brand-green-10)] font-medium text-[var(--brand-green)]'
                    : 'text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]'
                  }`}
                >
                  <Icon className="h-4 w-4 shrink-0" />
                  <span className="whitespace-nowrap">{t(label)}</span>
                  {/* The AI assistant is a new capability, clearly marked as still being polished, so users do not use it with the expectations of a stable feature */}
                  {id === 'ai' && <BetaBadge />}
                </button>
              ))}
            </nav>

            {/* The drag hot zone sits on the left/right dividing line, does not affect menu content width, and also supports left/right arrow-key fine-tuning. */}
            <div
              className="settings-modal__nav-resize-handle"
              role="separator"
              tabIndex={0}
              aria-orientation="vertical"
              aria-label={t('navResize')}
              aria-valuemin={renderedNavWidthMin}
              aria-valuemax={renderedNavWidthMax}
              aria-valuenow={renderedNavWidth}
              style={{ touchAction: 'none' }}
              onPointerDown={handleResizeStart}
              onKeyDown={handleResizeKeyDown}
            />
          </aside>

          <section className="flex min-w-0 flex-1 flex-col bg-[var(--bg-card)] settings-content">
            <div className="flex min-h-12 items-center justify-between border-b border-[var(--border-color)] px-5">
              <h3 className="text-sm font-medium text-[var(--text-primary)]">
                {t(SETTINGS_TABS.find((tab) => tab.id === activeTab)?.label ?? 'title')}
              </h3>
              <button
                type="button"
                onClick={onClose}
                className="rounded-lg p-1.5 text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
                aria-label={t('close', { defaultValue: 'Close settings' })}
              >
                <X className="h-4 w-4" />
              </button>
            </div>

            <div className="min-h-0 flex-1 overflow-auto p-5">
              {activeTab === 'general' && <GeneralSettings mode={mode} setMode={setMode} />}
              {activeTab === 'ai' && <AiSettings />}
              {activeTab === 'features' && <FeatureSettings />}
              {activeTab === 'disk-info' && <DiskInfoSettings />}
              {activeTab === 'guide' && <GuideSettings />}
              {activeTab === 'security' && <SecuritySettings />}
              {activeTab === 'feedback' && <FeedbackSettings />}
              {activeTab === 'about' && <AboutSettings />}
            </div>
          </section>
        </div>
      </div>
    </div>,
    document.body,
  );
}

export default SettingsModal;
