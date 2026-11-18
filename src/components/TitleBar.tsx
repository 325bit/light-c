// ============================================================================
// Custom title bar component
// ============================================================================

import { useState, useEffect, useRef, useCallback } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Copy, Minus, Square, X, Settings } from 'lucide-react';
import { useTranslation } from 'react-i18next';

interface TitleBarProps {
  onSettingsClick: () => void;
}

const DRAG_START_THRESHOLD = 4;

export function TitleBar({ onSettingsClick }: TitleBarProps) {
  const { t } = useTranslation('ui');
  const { t: commonT } = useTranslation('common');
  const [isMaximized, setIsMaximized] = useState(false);
  const dragStartRef = useRef<{ x: number; y: number } | null>(null);
  const appWindowRef = useRef(getCurrentWindow());
  const appWindow = appWindowRef.current;

  const refreshMaximizedState = useCallback(() => {
    appWindow.isMaximized().then(setIsMaximized).catch((err) => {
      console.error('同步窗口最大化状态失败:', err);
    });
  }, [appWindow]);

  useEffect(() => {
    // Check window state at initialization
    refreshMaximizedState();

    const unlisteners: Array<() => void> = [];
    let disposed = false;

    Promise.all([
      appWindow.onResized(refreshMaximizedState),
      appWindow.onMoved(refreshMaximizedState),
      appWindow.onScaleChanged(refreshMaximizedState),
      appWindow.onFocusChanged(refreshMaximizedState),
    ]).then((items) => {
      if (disposed) {
        items.forEach((unlisten) => unlisten());
        return;
      }
      unlisteners.push(...items);
    }).catch((err) => {
      console.error('监听窗口状态失败:', err);
    });

    return () => {
      disposed = true;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, [appWindow, refreshMaximizedState]);

  const handleMinimize = () => appWindow.minimize();
  
  const handleMaximize = async () => {
    const maximized = await appWindow.isMaximized();
    if (maximized) {
      await appWindow.unmaximize();
      setIsMaximized(false);
    } else {
      await appWindow.maximize();
      setIsMaximized(true);
    }
  };

  const handleClose = () => appWindow.close();

  const isWindowControl = (target: EventTarget | null) => {
    return target instanceof HTMLElement && Boolean(target.closest('[data-window-control]'));
  };

  const handleTitleBarMouseDown = (event: React.MouseEvent<HTMLDivElement>) => {
    if (event.button !== 0 || event.detail > 1 || isWindowControl(event.target)) return;
    // When Windows has multiple monitors with inconsistent DPI, calling startDragging directly on a maximized window can easily trigger wrong restore coordinates.
    if (isMaximized) return;
    dragStartRef.current = { x: event.clientX, y: event.clientY };
  };

  const handleTitleBarMouseMove = (event: React.MouseEvent<HTMLDivElement>) => {
    const dragStart = dragStartRef.current;
    if (!dragStart) return;

    const distanceX = Math.abs(event.clientX - dragStart.x);
    const distanceY = Math.abs(event.clientY - dragStart.y);
    if (distanceX < DRAG_START_THRESHOLD && distanceY < DRAG_START_THRESHOLD) return;

    dragStartRef.current = null;
    appWindow.startDragging().catch((err) => {
      console.error('拖动窗口失败:', err);
    });
  };

  const handleTitleBarMouseUp = () => {
    dragStartRef.current = null;
  };

  const handleTitleBarDoubleClick = (event: React.MouseEvent<HTMLDivElement>) => {
    if (isWindowControl(event.target)) return;
    event.preventDefault();
    dragStartRef.current = null;
    handleMaximize();
  };

  return (
    <div 
      className="h-10 glass-chrome glass-header-top bg-[var(--bg-card)] border-b border-[var(--border-color)] flex items-center justify-between shrink-0 select-none"
      onDoubleClick={handleTitleBarDoubleClick}
      onMouseDown={handleTitleBarMouseDown}
      onMouseMove={handleTitleBarMouseMove}
      onMouseLeave={handleTitleBarMouseUp}
      onMouseUp={handleTitleBarMouseUp}
    >
      {/* Left: app icon and title - use primary text color */}
      <div className="flex items-center gap-2 px-3 h-full flex-1">
        {/* Use the project's real logo. Previously it was an approximation pieced together with "green background + C: text + rounded-lg",
            with 33% rounding (8/24) not matching the logo's own 18.75% (rx 96/512), making it look rounder than the real logo.
            The logo has its own background and rounding; here we only control size. */}
        <img src="/logo.svg" alt="" aria-hidden="true" className="h-6 w-6 shrink-0 object-contain" />
        <span className="text-sm font-semibold text-[var(--text-primary)]">LightC</span>
      </div>

      {/* Right: settings + window control buttons - minimal style */}
      <div className="flex items-center h-full">
        {/* Settings button - Ghost style */}
        <button
          data-window-control
          data-titlebar-control
          onClick={onSettingsClick}
          className="h-full px-3 flex items-center justify-center text-[var(--text-muted)] hover:text-[var(--text-primary)] hover:bg-[var(--bg-hover)] transition-colors"
          title={t('windowSettings')}
        >
          <Settings className="w-4 h-4" />
        </button>

        {/* Divider */}
        <div className="w-px h-4 bg-[var(--border-color)] mx-1" />

        {/* Minimize */}
        <button
          data-window-control
          data-titlebar-control
          onClick={handleMinimize}
          className="h-full px-3 flex items-center justify-center text-[var(--text-muted)] hover:text-[var(--text-primary)] hover:bg-[var(--bg-hover)] transition-colors"
          title={t('windowMinimize')}
        >
          <Minus className="w-4 h-4" />
        </button>

        {/* Maximize/Restore */}
        <button
          data-window-control
          data-titlebar-control
          onClick={handleMaximize}
          className="h-full px-3 flex items-center justify-center text-[var(--text-muted)] hover:text-[var(--text-primary)] hover:bg-[var(--bg-hover)] transition-colors"
          title={isMaximized ? t('windowRestore') : t('windowMaximize')}
        >
          {/* Show an overlapping-boxes icon when maximized, matching Windows' visual convention for "Restore window". */}
          {isMaximized ? <Copy className="w-3.5 h-3.5" /> : <Square className="w-3.5 h-3.5" />}
        </button>

        {/* Close */}
        <button
          data-window-control
          data-titlebar-control
          onClick={handleClose}
          className="h-full px-3 flex items-center justify-center text-[var(--text-muted)] hover:bg-[var(--color-danger)] hover:text-white transition-colors"
          title={commonT('close')}
        >
          <X className="w-4 h-4" />
        </button>
      </div>
    </div>
  );
}
