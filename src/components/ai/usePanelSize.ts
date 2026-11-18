// ============================================================================
// AI panel size: draggable resize + persistence
//
// Two easily overlooked points:
//
// 1. **Inline styles override CSS max-* limits**. The panel defaults to em + vw/vh for sizing,
//    once the user writes inline px while dragging, the CSS layer's "shrink with window" protection stops working —
//    so clamping must be done here, and must be recomputed when the window size changes.
// 2. **Dragging must be clamped to a reasonable range**: too small is unreadable, too large covers the module's main page (exactly what we do not want).
// ============================================================================

import { useCallback, useEffect, useState, type CSSProperties, type PointerEvent as ReactPointerEvent } from 'react';

const STORAGE_KEY = 'c-cleanup-ai-panel-size';

/** Lower bound for dragging: any smaller and the conversation becomes unreadable. */
const MIN_WIDTH = 320;
/* Drag lower bound. It used to be 240px: after a 40px header and ~70px input area, only ~130px was left for messages,
   giving a "cramped" look right away, so it was raised to 320. */
const MIN_HEIGHT = 320;
/** Minimum margin to keep from the viewport (20px on the right + left whitespace). */
const VIEWPORT_MARGIN_X = 48;
const VIEWPORT_MARGIN_Y = 120;

export interface AiPanelSize {
  width: number;
  height: number;
}

function readStoredSize(): AiPanelSize | null {
  if (typeof window === 'undefined') return null;
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<AiPanelSize>;
    if (typeof parsed.width !== 'number' || typeof parsed.height !== 'number') return null;
    if (!Number.isFinite(parsed.width) || !Number.isFinite(parsed.height)) return null;
    return { width: parsed.width, height: parsed.height };
  } catch {
    // Treat corrupt storage as never saved; a failed settings read must not crash
    return null;
  }
}

/** Clamp the size to "the maximum allowed by the current viewport". Must re-clamp when the window shrinks. */
function clampToViewport(size: AiPanelSize): AiPanelSize {
  const maxWidth = Math.max(MIN_WIDTH, window.innerWidth - VIEWPORT_MARGIN_X);
  const maxHeight = Math.max(MIN_HEIGHT, window.innerHeight - VIEWPORT_MARGIN_Y);
  return {
    width: Math.min(Math.max(size.width, MIN_WIDTH), maxWidth),
    height: Math.min(Math.max(size.height, MIN_HEIGHT), maxHeight),
  };
}

export interface UsePanelSizeResult {
  /** Inline sizes applied to the panel; with the default size, width is left to CSS (em-based, following the font-size setting) */
  sizeStyle: CSSProperties;
  hasCustomSize: boolean;
  startResize: (event: ReactPointerEvent<HTMLElement>) => void;
  resetSize: () => void;
}

export function usePanelSize(): UsePanelSizeResult {
  const [size, setSize] = useState<AiPanelSize | null>(() => {
    const stored = readStoredSize();
    return stored ? clampToViewport(stored) : null;
  });

  // Re-clamp when the viewport changes: otherwise if the user dragged it larger and the window later shrinks, the panel sticks out of the screen.
  useEffect(() => {
    if (!size) return;
    const handleResize = () => setSize((previous) => (previous ? clampToViewport(previous) : previous));
    window.addEventListener('resize', handleResize);
    return () => window.removeEventListener('resize', handleResize);
    // Depending on the size reference would re-attach the listener on every drag; only the fact "a custom size exists" is needed here
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [size !== null]);

  const startResize = useCallback((event: ReactPointerEvent<HTMLElement>) => {
    // The panel is fixed at the bottom-right, so dragging happens at the top-left handle:
    // moving the pointer left/up = panel grows, so just negate the delta.
    const panel = event.currentTarget.parentElement as HTMLElement | null;
    if (!panel) return;
    const rect = panel.getBoundingClientRect();
    const startX = event.clientX;
    const startY = event.clientY;
    const initial = { width: rect.width, height: rect.height };

    event.preventDefault();
    const handle = event.currentTarget;
    handle.setPointerCapture(event.pointerId);

    // Keep the latest size in a ref: read it directly on release to write to disk.
    // Do not put the localStorage write inside the setSize updater — that is a pure function,
    // and under StrictMode it is called twice, making the disk write a side effect.
    let latest = initial;

    const handleMove = (moveEvent: PointerEvent) => {
      latest = clampToViewport({
        width: initial.width + (startX - moveEvent.clientX),
        height: initial.height + (startY - moveEvent.clientY),
      });
      setSize(latest);
    };
    const handleUp = () => {
      // When the pointer is taken by the system (pointercancel), the capture may already be gone, so a direct release would throw
      if (handle.hasPointerCapture(event.pointerId)) {
        handle.releasePointerCapture(event.pointerId);
      }
      handle.removeEventListener('pointermove', handleMove);
      handle.removeEventListener('pointerup', handleUp);
      handle.removeEventListener('pointercancel', handleUp);
      // Only write to disk on release: repeatedly writing localStorage during dragging is needless IO
      localStorage.setItem(STORAGE_KEY, JSON.stringify(latest));
    };

    handle.addEventListener('pointermove', handleMove);
    handle.addEventListener('pointerup', handleUp);
    // When the pointer is stolen by the system (touch gesture, focus loss, interrupted by a dialog) only pointercancel arrives:
    // without handling it, the move listener stays on the handle and the next drag stacks an old copy.
    handle.addEventListener('pointercancel', handleUp);
  }, []);

  const resetSize = useCallback(() => {
    localStorage.removeItem(STORAGE_KEY);
    setSize(null);
  }, []);

  return {
    sizeStyle: size ? { width: size.width, height: size.height } : {},
    hasCustomSize: size !== null,
    startResize,
    resetSize,
  };
}
