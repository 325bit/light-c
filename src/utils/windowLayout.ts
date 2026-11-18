// Window size coordination at startup
// The sidebar needs a stable content width, so complete the minimum-size constraints and necessary expansion before restoring the window.

import { currentMonitor, getCurrentWindow, LogicalSize, PhysicalPosition } from '@tauri-apps/api/window';

// The sidebar contains the full function menu and page content; a 720px height reduces bottom-menu truncation.
export const WINDOW_MIN_SIZE = { width: 1080, height: 720 } as const;
// Reserve a small workspace margin, so a window snapped to the edge is not misjudged as off-screen due to taskbar or system scaling.
export const WINDOW_WORK_AREA_MARGIN = 24;

interface WindowLayoutSize {
  width: number;
  height: number;
}

function clamp(value: number, minimum: number, maximum: number) {
  return Math.min(maximum, Math.max(minimum, value));
}

function toLogicalSize(physicalSize: { width: number; height: number }, scaleFactor: number): WindowLayoutSize {
  return {
    width: physicalSize.width / scaleFactor,
    height: physicalSize.height / scaleFactor,
  };
}

export function getMaximumLogicalWindowSize(
  workArea: { width: number; height: number },
  scaleFactor: number,
): WindowLayoutSize | null {
  if (!Number.isFinite(scaleFactor) || scaleFactor <= 0) return null;

  // Workspace dimensions come from physical pixels, and the margin is also deducted in physical pixels first, keeping rules consistent across DPIs.
  return {
    width: Math.max(1, Math.floor((workArea.width - WINDOW_WORK_AREA_MARGIN) / scaleFactor)),
    height: Math.max(1, Math.floor((workArea.height - WINDOW_WORK_AREA_MARGIN) / scaleFactor)),
  };
}

export function getEffectiveWindowMinimumSize(maximumSize: WindowLayoutSize | null): WindowLayoutSize {
  if (!maximumSize) return WINDOW_MIN_SIZE;

  // On low-resolution devices that cannot reach the design target, obey the workspace, avoiding a minimum size larger than the available screen.
  return {
    width: Math.max(1, Math.min(WINDOW_MIN_SIZE.width, maximumSize.width)),
    height: Math.max(1, Math.min(WINDOW_MIN_SIZE.height, maximumSize.height)),
  };
}

interface WindowBounds {
  currentSize: WindowLayoutSize;
  maximumSize: WindowLayoutSize | null;
  outerPosition: { x: number; y: number };
  outerSize: WindowLayoutSize;
}

async function readWindowBounds(): Promise<WindowBounds> {
  const appWindow = getCurrentWindow();
  const scaleFactor = await appWindow.scaleFactor();
  if (!Number.isFinite(scaleFactor) || scaleFactor <= 0) {
    throw new Error('无法读取当前窗口缩放比例');
  }

  const [physicalInnerSize, monitor, outerPosition, physicalOuterSize] = await Promise.all([
    appWindow.innerSize(),
    currentMonitor(),
    appWindow.outerPosition(),
    appWindow.outerSize(),
  ]);

  return {
    currentSize: toLogicalSize(physicalInnerSize, scaleFactor),
    maximumSize: monitor
      ? getMaximumLogicalWindowSize({ width: monitor.workArea.size.width, height: monitor.workArea.size.height }, scaleFactor)
      : null,
    outerPosition,
    outerSize: toLogicalSize(physicalOuterSize, scaleFactor),
  };
}

async function setWindowMinimumSize(maximumSize: WindowLayoutSize | null) {
  const minimumSize = getEffectiveWindowMinimumSize(maximumSize);
  await getCurrentWindow().setMinSize(new LogicalSize(minimumSize.width, minimumSize.height));
}

async function expandWindowForSidebar(bounds: WindowBounds, minimumSize: WindowLayoutSize): Promise<void> {
  const appWindow = getCurrentWindow();
  if (await appWindow.isMaximized()) return;

  const scaleFactor = await appWindow.scaleFactor();
  if (!Number.isFinite(scaleFactor) || scaleFactor <= 0) return;

  const nextWidth = Math.max(bounds.currentSize.width, minimumSize.width);
  const nextHeight = Math.max(bounds.currentSize.height, minimumSize.height);
  if (nextWidth === bounds.currentSize.width && nextHeight === bounds.currentSize.height) return;

  // If the current window is already larger than the workspace, do not force-shrink it for layout preparation, avoiding overriding a user-set size.
  const safeWidth = bounds.maximumSize
    ? Math.max(bounds.currentSize.width, Math.min(nextWidth, bounds.maximumSize.width))
    : nextWidth;
  const safeHeight = bounds.maximumSize
    ? Math.max(bounds.currentSize.height, Math.min(nextHeight, bounds.maximumSize.height))
    : nextHeight;
  const nextSize = new LogicalSize(Math.round(safeWidth), Math.round(safeHeight));
  await appWindow.setSize(nextSize);

  const monitor = await currentMonitor();
  if (!monitor) return;

  // When expanding, keep the window center as unchanged as possible and clamp the result within the current monitor's workspace, avoiding window edges going off-screen.
  const nextOuterWidth = bounds.outerSize.width + (safeWidth - bounds.currentSize.width) * scaleFactor;
  const nextOuterHeight = bounds.outerSize.height + (safeHeight - bounds.currentSize.height) * scaleFactor;
  const centeredX = bounds.outerPosition.x - (nextOuterWidth - bounds.outerSize.width) / 2;
  const centeredY = bounds.outerPosition.y - (nextOuterHeight - bounds.outerSize.height) / 2;
  const workAreaLeft = monitor.workArea.position.x;
  const workAreaTop = monitor.workArea.position.y;
  const workAreaRight = workAreaLeft + monitor.workArea.size.width;
  const workAreaBottom = workAreaTop + monitor.workArea.size.height;
  // In extreme cases where the old window was already larger than the workspace, keep the original position and do not create a reverse jump.
  const nextX = nextOuterWidth <= monitor.workArea.size.width
    ? clamp(centeredX, workAreaLeft, workAreaRight - nextOuterWidth)
    : bounds.outerPosition.x;
  const nextY = nextOuterHeight <= monitor.workArea.size.height
    ? clamp(centeredY, workAreaTop, workAreaBottom - nextOuterHeight)
    : bounds.outerPosition.y;
  if (Math.round(nextX) !== bounds.outerPosition.x || Math.round(nextY) !== bounds.outerPosition.y) {
    await appWindow.setPosition(new PhysicalPosition(Math.round(nextX), Math.round(nextY)));
  }
}

export async function prepareWindowForSavedLayout(): Promise<void> {
  const bounds = await readWindowBounds();
  const minimumSize = getEffectiveWindowMinimumSize(bounds.maximumSize);

  // Update the minimum size first, then expand the current window, so users cannot immediately drag it back to an unsuitable width.
  await setWindowMinimumSize(bounds.maximumSize);
  await expandWindowForSidebar(bounds, minimumSize);
}
