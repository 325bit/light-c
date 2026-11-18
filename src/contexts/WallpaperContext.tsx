// ============================================================================
// Wallpaper context
// Wallpaper only handles the "background"; orthogonal to light/dark or glass effects: both themes share one image, each with its own overlay intensity.
// The image file is copied to the data directory by the backend; here we only persist display config + load via the asset protocol.
// ============================================================================

import { createContext, useContext, useEffect, useMemo, useState, useCallback, type ReactNode } from 'react';
import { convertFileSrc } from '@tauri-apps/api/core';
import { clearWallpaper, getWallpaperPath, installWallpaper, pickWallpaperDialog } from '../api/commands';

/** Image fit mode: fill and crop proportionally / fit fully with proportional letterboxing / original size centered */
export type WallpaperFit = 'cover' | 'contain' | 'center';

/** Display config (the image itself is not in localStorage, to avoid a few MB of base64 stalling settings reads) */
interface WallpaperSettings {
  fit: WallpaperFit;
  /** Light-theme overlay intensity 0-100; higher means whiter and text clearer */
  dimLight: number;
  /** Dark-theme overlay intensity 0-100; higher means darker */
  dimDark: number;
}

interface WallpaperContextValue extends WallpaperSettings {
  /** asset URL of the current wallpaper; an empty string means no wallpaper */
  url: string;
  /** Whether a wallpaper is installed */
  active: boolean;
  /** Pick an image and install it; cancel or failure leaves the existing config unchanged */
  choose: () => Promise<void>;
  /** Clear the wallpaper, returning to a solid-color background */
  remove: () => Promise<void>;
  setFit: (fit: WallpaperFit) => void;
  setDimLight: (value: number) => void;
  setDimDark: (value: number) => void;
}

const WallpaperContext = createContext<WallpaperContextValue | null>(null);

const SETTINGS_KEY = 'c-cleanup-wallpaper';
const DEFAULT_SETTINGS: WallpaperSettings = {
  fit: 'cover',
  dimLight: 55,
  dimDark: 50,
};

function readSettings(): WallpaperSettings {
  try {
    const saved = localStorage.getItem(SETTINGS_KEY);
    if (!saved) return DEFAULT_SETTINGS;
    const parsed = JSON.parse(saved) as Partial<WallpaperSettings>;
    const clamp = (value: unknown, fallback: number) =>
      Number.isFinite(value) ? Math.min(100, Math.max(0, Math.floor(Number(value)))) : fallback;
    return {
      fit: parsed.fit === 'contain' || parsed.fit === 'center' ? parsed.fit : 'cover',
      dimLight: clamp(parsed.dimLight, DEFAULT_SETTINGS.dimLight),
      dimDark: clamp(parsed.dimDark, DEFAULT_SETTINGS.dimDark),
    };
  } catch {
    return DEFAULT_SETTINGS;
  }
}

export function WallpaperProvider({ children }: { children: ReactNode }) {
  const [settings, setSettings] = useState<WallpaperSettings>(readSettings);
  const [url, setUrl] = useState('');
  /**
   * Wallpaper image revision. The wallpaper filename is fixed (wallpaper.png), so after changing the image convertFileSrc still returns the same
   * URL and the browser hits the cache and keeps showing the old image - an incrementing query parameter is required to force a re-request.
   * The asset protocol only uses the URL's path (the query is ignored), so adding a parameter does not affect backend file retrieval.
   */
  // Initialize with a timestamp rather than 0: the WebView's disk cache survives restarts,
  // so if every startup used the same `?v=0`, after changing the image and restarting the cached old image might show again.
  const [revision, setRevision] = useState(() => Date.now());

  // Ask the backend for the wallpaper path at startup: the file is in the data directory, so it travels with portable installations.
  // An empty string from the backend means no wallpaper was installed; keep the solid-color background.
  useEffect(() => {
    let cancelled = false;
    getWallpaperPath()
      .then((path) => {
        if (!cancelled && path) setUrl(convertFileSrc(path));
      })
      .catch((error) => console.error('读取壁纸失败:', error));
    return () => {
      cancelled = true;
    };
  }, []);

  // Sync config to the root node: the wallpaper layer is drawn in pure CSS, React only changes variables and classes.
  // This way switching theme/fit mode does not remount the main UI, so in-progress scan module state is not lost.
  useEffect(() => {
    const root = document.documentElement;
    root.classList.toggle('has-wallpaper', Boolean(url));
    const versionedUrl = url ? `${url}?v=${revision}` : '';
    root.style.setProperty('--wallpaper-image', versionedUrl ? `url("${versionedUrl}")` : 'none');
    root.style.setProperty('--wallpaper-size', settings.fit === 'center' ? 'auto' : settings.fit);
    // The overlay must be injected as a complete linear-gradient string, not a bare rgba variable.
    // ⚠️ Measured: using var(--x) inside background-image to expand rgba(...) invalidates the whole declaration
    // (overlay and image both disappear, leaving only a solid background); a gradient is an image type and expands correctly.
    const lightScrim = `rgba(255, 255, 255, ${settings.dimLight / 100})`;
    const darkScrim = `rgba(0, 0, 0, ${settings.dimDark / 100})`;
    root.style.setProperty('--wallpaper-scrim-light', `linear-gradient(${lightScrim}, ${lightScrim})`);
    root.style.setProperty('--wallpaper-scrim-dark', `linear-gradient(${darkScrim}, ${darkScrim})`);
    // ⚠️ revision must be in the dependencies: when the image changes the filename is unchanged -> the url string is unchanged,
    // relying on url alone will not re-run this effect and the new revision never gets injected (the direct cause of wallpaper not refreshing).
  }, [url, revision, settings]);

  // Config changes are persisted uniformly; the image URL is runtime state and is not written to disk.
  useEffect(() => {
    try {
      localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
    } catch (error) {
      console.error('保存壁纸设置失败:', error);
    }
  }, [settings]);

  const update = useCallback(<K extends keyof WallpaperSettings>(key: K, value: WallpaperSettings[K]) => {
    setSettings((current) => ({ ...current, [key]: value }));
  }, []);

  const choose = useCallback(async () => {
    const picked = await pickWallpaperDialog();
    if (!picked) return;
    const savedPath = await installWallpaper(picked);
    setUrl(convertFileSrc(savedPath));
    // Same as above: increment the revision so the new image bypasses the old URL's cache.
    setRevision((current) => current + 1);
  }, []);

  const remove = useCallback(async () => {
    await clearWallpaper();
    setUrl('');
  }, []);

  const value = useMemo<WallpaperContextValue>(() => ({
    ...settings,
    url,
    active: Boolean(url),
    choose,
    remove,
    setFit: (fit) => update('fit', fit),
    setDimLight: (dimLight) => update('dimLight', dimLight),
    setDimDark: (dimDark) => update('dimDark', dimDark),
  }), [settings, url, choose, remove, update]);

  return <WallpaperContext.Provider value={value}>{children}</WallpaperContext.Provider>;
}

export function useWallpaper(): WallpaperContextValue {
  const context = useContext(WallpaperContext);
  if (!context) throw new Error('useWallpaper 必须在 WallpaperProvider 内部使用');
  return context;
}
