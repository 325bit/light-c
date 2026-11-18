// ============================================================================
// Main window size persistence
// Saves the size of the normal (non-maximized) window the user adjusted, so a maximized size doesn't override the default for the next launch.
// ============================================================================

import { useEffect } from 'react';
import { currentMonitor, getCurrentWindow, LogicalSize } from '@tauri-apps/api/window';
import { getEffectiveWindowMinimumSize, getMaximumLogicalWindowSize, prepareWindowForSavedLayout } from './windowLayout';

const WINDOW_STATE_STORAGE_KEY = 'c-cleanup-window-state';
const WINDOW_STATE_VERSION = 1;
const WINDOW_RESIZE_SAVE_DELAY = 250;

// Kept consistent with tauri.conf.json, to prevent stale caches from restoring the window to an unusable size.
interface WindowSize {
  width: number;
  height: number;
}

interface PersistedWindowState extends WindowSize {
  version: number;
}

function readPersistedWindowSize(): WindowSize | null {
  try {
    const raw = localStorage.getItem(WINDOW_STATE_STORAGE_KEY);
    if (!raw) return null;

    const parsed = JSON.parse(raw) as Partial<PersistedWindowState>;
    const width = Number(parsed.width);
    const height = Number(parsed.height);

    // Accept only finite positive numbers, to avoid passing invalid sizes to Tauri after localStorage is manually edited.
    if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 0 || height <= 0) {
      return null;
    }

    return {
      width: Math.round(width),
      height: Math.round(height),
    };
  } catch (error) {
    console.error('读取窗口尺寸失败:', error);
    return null;
  }
}

function clampWindowSize(size: WindowSize, maximumSize: WindowSize | null, minimumSize: WindowSize): WindowSize {
  const maxWidth = Math.max(minimumSize.width, maximumSize?.width ?? Number.POSITIVE_INFINITY);
  const maxHeight = Math.max(minimumSize.height, maximumSize?.height ?? Number.POSITIVE_INFINITY);

  return {
    width: Math.min(maxWidth, Math.max(minimumSize.width, Math.round(size.width))),
    height: Math.min(maxHeight, Math.max(minimumSize.height, Math.round(size.height))),
  };
}

async function getMaximumLogicalSize(): Promise<WindowSize | null> {
  const [monitor, scaleFactor] = await Promise.all([currentMonitor(), getCurrentWindow().scaleFactor()]);
  if (!monitor || !Number.isFinite(scaleFactor) || scaleFactor <= 0) return null;

  // Tauri's monitor work-area size is in physical pixels; it must be converted to logical pixels when restoring the window.
  return getMaximumLogicalWindowSize(
    { width: monitor.workArea.size.width, height: monitor.workArea.size.height },
    scaleFactor,
  );
}

export function useWindowStatePersistence(): void {
  useEffect(() => {
    const appWindow = getCurrentWindow();
    let disposed = false;
    let restoring = true;
    let resizeTimer: number | null = null;
    let unlistenResize: (() => void) | null = null;

    const persistCurrentSize = async (physicalSize?: { width: number; height: number }) => {
      try {
        // When maximized, what's recorded is the work-area size; the user's last normal-window size must be preserved.
        if (await appWindow.isMaximized()) return;

        const scaleFactor = await appWindow.scaleFactor();
        const size = physicalSize ?? await appWindow.innerSize();
        if (!Number.isFinite(scaleFactor) || scaleFactor <= 0) return;

        const state: PersistedWindowState = {
          version: WINDOW_STATE_VERSION,
          width: Math.round(size.width / scaleFactor),
          height: Math.round(size.height / scaleFactor),
        };
        localStorage.setItem(WINDOW_STATE_STORAGE_KEY, JSON.stringify(state));
      } catch (error) {
        console.error('保存窗口尺寸失败:', error);
      }
    };

    const restoreWindowSize = async () => {
      try {
        await prepareWindowForSavedLayout();
        const savedSize = readPersistedWindowSize();
        if (!savedSize) return;

        const maximumSize = await getMaximumLogicalSize();
        const minimumSize = getEffectiveWindowMinimumSize(maximumSize);
        const restoredSize = clampWindowSize(savedSize, maximumSize, minimumSize);
        await appWindow.setSize(new LogicalSize(restoredSize.width, restoredSize.height));
      } catch (error) {
        console.error('恢复窗口尺寸失败:', error);
      } finally {
        restoring = false;
      }
    };

    const registerResizeListener = async () => {
      try {
        const unlisten = await appWindow.onResized(({ payload }) => {
          if (disposed || restoring) return;

          if (resizeTimer !== null) {
            window.clearTimeout(resizeTimer);
          }
          resizeTimer = window.setTimeout(() => {
            resizeTimer = null;
            void persistCurrentSize(payload);
          }, WINDOW_RESIZE_SAVE_DELAY);
        });
        if (disposed) {
          // The component may have unmounted while the listener was registering; immediately unlisten the one obtained after the async completes.
          unlisten();
          return;
        }
        unlistenResize = unlisten;
      } catch (error) {
        console.error('监听窗口尺寸变化失败:', error);
      }
    };

    // Register the listener before restoring the size, so events aren't missed if the user keeps dragging after restoration.
    void registerResizeListener();
    void restoreWindowSize();

    return () => {
      disposed = true;
      restoring = true;
      if (resizeTimer !== null) {
        window.clearTimeout(resizeTimer);
      }
      unlistenResize?.();
    };
  }, []);
}
