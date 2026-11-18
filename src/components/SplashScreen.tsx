// ============================================================================
// Single-window splash screen
// Keeps only the brand logo, a lightweight gradient background, and author platform info, avoiding creating a second WebView during startup.
// ============================================================================

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useTheme } from '../contexts';

const SPLASH_DURATION = 1400;
const SPLASH_EXIT_DURATION = 220;

interface SplashScreenProps {
  onComplete?: () => void;
}

/**
 * Reads the splash screen's current theme.
 *
 * Chinese note: the static splash screen in index.html writes `splash-screen--light|dark` according to the user's theme before React takes over;
 * here we first read the existing value on the DOM, ensuring the first-frame render matches the static node exactly,
 * otherwise hydrateRoot reports "Hydration failed". Only when DOM info is unavailable do we fall back to the theme context.
 */
function readPrerenderedSplashTheme(fallback: string): string {
  const classList = document.getElementById('initial-splash')?.classList;
  if (!classList) return fallback;
  if (classList.contains('splash-screen--light')) return 'light';
  if (classList.contains('splash-screen--dark')) return 'dark';
  return fallback;
}

export function SplashScreen({ onComplete }: SplashScreenProps) {
  const { theme } = useTheme();
  const { t } = useTranslation('ui');
  const [isExiting, setIsExiting] = useState(false);
  const [splashTheme] = useState(() => readPrerenderedSplashTheme(theme));

  useEffect(() => {
    const startedAt = Number(document.documentElement.dataset.splashStartedAt);
    const elapsed = Number.isFinite(startedAt) ? Math.max(0, performance.now() - startedAt) : 0;
    const exitDelay = Math.max(0, SPLASH_DURATION - elapsed);
    const completeDelay = Math.max(0, SPLASH_DURATION + SPLASH_EXIT_DURATION - elapsed);
    const exitTimer = window.setTimeout(() => setIsExiting(true), exitDelay);
    const completeTimer = window.setTimeout(() => {
      onComplete?.();
    }, completeDelay);

    // Clear both timers at the same time, to avoid triggering a page switch after the component unmounts.
    return () => {
      window.clearTimeout(exitTimer);
      window.clearTimeout(completeTimer);
    };
  }, [onComplete]);

  return (
    <div
      id="initial-splash"
      className={`splash-screen splash-screen--${splashTheme}${isExiting ? ' splash-screen--exiting' : ''}`}
      role="status"
      aria-label="LightC"
    >
      <div className="splash-screen__aurora splash-screen__aurora--one" aria-hidden="true" />
      <div className="splash-screen__aurora splash-screen__aurora--two" aria-hidden="true" />

      <div className="splash-screen__content">
        <img className="splash-screen__logo" src="/logo.svg" alt="LightC" />
        <p id="splash-slogan" className="splash-screen__slogan">{t('splashSlogan')}</p>
        <div className="splash-screen__platform" aria-label="Evan的像素空间">
          {/* <span className="splash-screen__author">Evan's Pixel Space</span> */}
          <span className="splash-screen__channel">Bilibili · @Evan的像素空间</span>
        </div>
      </div>
    </div>
  );
}

export default SplashScreen;
