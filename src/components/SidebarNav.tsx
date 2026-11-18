// Sidebar layout navigation
// Sidebar mode turns the original floating nav into a stable content column, preventing menus from covering module results.

import { useEffect, useRef, useState, type CSSProperties, type MouseEvent as ReactMouseEvent, type PointerEvent as ReactPointerEvent } from 'react';
import { createPortal } from 'react-dom';
import { Github, Globe, Heart, ScrollText, UserRound } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { getVersion } from '@tauri-apps/api/app';
import { APP_MODULE_META, type AppModuleId } from '../config/moduleMeta';
import { FONT_SIZE_CONFIGS, useFontSize, useSettings } from '../contexts';
import { SupportModal } from './SupportModal';
import { AUTHOR_HOMEPAGE_URL, LIGHTC_CHANGELOG_URL, LIGHTC_GITHUB_URL, LIGHTC_OFFICIAL_WEBSITE_URL } from '../config/officialLinks';

const SIDEBAR_COLLAPSED_STORAGE_KEY = 'c-cleanup-sidebar-collapsed';
const SIDEBAR_WIDTH_STORAGE_KEY = 'c-cleanup-sidebar-width';
// Width is stored in pixels at the standard font size; the displayed width scales proportionally with the user's font size.
const SIDEBAR_WIDTH_DEFAULT = 220;
const SIDEBAR_WIDTH_MIN = 180;
const SIDEBAR_WIDTH_MAX = 360;

/**
 * Sidebar grouping.
 *
 * The grouping axis is **"what gets changed"**, not "cleanup granularity" (the latter is forced: 8 of the 12 are called cleanup,
 * and slicing by granularity is never accurate — Evan pointed this out).
 *   Cleanup: deletes **files** (caches, big files, chat history, uninstall leftover files)
 *   System: changes **system configuration** (system components, driver store, registry, context menus, shell icons)
 *   Analysis: **read-only**, changes nothing (usage distribution, change amount, model space)
 * The three axes do not overlap, so the user can tell at a glance whether a group will touch their system settings.
 */
const SIDEBAR_GROUPS: { id: string; ids: AppModuleId[] }[] = [
  { id: 'cleanup', ids: ['junk-clean', 'big-files', 'social-clean', 'ai-tool-cleanup', 'dev-cache', 'leftovers'] },
  { id: 'system', ids: ['system-slim', 'driver-cleanup', 'registry', 'context-menu', 'shell-icons'] },
  { id: 'analyze', ids: ['hotspot', 'disk-growth', 'ai-models'] },
];

/** Module id → metadata, used to look up icons and text by id when rendering groups */
const MODULE_META_BY_ID = new Map(APP_MODULE_META.map((meta) => [meta.id, meta]));

/**
 * Hand-drawn collapse icon: a rounded outer frame plus a round-capped vertical line.
 *
 * Why not use the icon library's PanelLeft: its frame corner radius is too small and its strokes too hard, not matching the sidebar's
 * large rounded panels / pill-shaped selected state (Evan explicitly said "it looks like a default icon library icon").
 * Here the radius is enlarged to 4.5/16, stroke width reduced to 1.5, and the vertical line flips to the right after collapsing,
 * so the icon itself expresses the current state.
 */
function SidebarToggleIcon({ collapsed }: { collapsed: boolean }) {
  return (
    <svg
      viewBox="0 0 20 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.5}
      strokeLinecap="round"
      aria-hidden="true"
    >
      <rect x="1" y="1" width="18" height="14" rx="4.5" />
      <path d={collapsed ? 'M12.5 2v12' : 'M8 2v12'} />
    </svg>
  );
}

function readCollapsedState(): boolean {
  try {
    return localStorage.getItem(SIDEBAR_COLLAPSED_STORAGE_KEY) === 'true';
  } catch (error) {
    // When storage is unavailable, use the expanded state so the user can still see the full feature names.
    console.warn('读取侧边栏状态失败:', error);
    return false;
  }
}

function writeCollapsedState(isCollapsed: boolean): void {
  try {
    localStorage.setItem(SIDEBAR_COLLAPSED_STORAGE_KEY, String(isCollapsed));
  } catch (error) {
    // Collapsing in the current session still works; a storage failure only affects the default state on next launch.
    console.warn('保存侧边栏状态失败:', error);
  }
}

function readSidebarWidth(): number {
  try {
    const savedWidth = Number(localStorage.getItem(SIDEBAR_WIDTH_STORAGE_KEY));
    return Number.isFinite(savedWidth)
      ? Math.min(SIDEBAR_WIDTH_MAX, Math.max(SIDEBAR_WIDTH_MIN, Math.round(savedWidth)))
      : SIDEBAR_WIDTH_DEFAULT;
  } catch (error) {
    // When storage is unavailable, use the default width so the sidebar does not fail to display due to local data issues.
    console.warn('读取侧边栏宽度失败:', error);
    return SIDEBAR_WIDTH_DEFAULT;
  }
}

function writeSidebarWidth(width: number): void {
  try {
    localStorage.setItem(SIDEBAR_WIDTH_STORAGE_KEY, String(width));
  } catch (error) {
    // The drag result in the current session is still kept; a storage failure only affects the width on next launch.
    console.warn('保存侧边栏宽度失败:', error);
  }
}

export function SidebarNav() {
  const { t } = useTranslation('nav');
  const { level, customFontSize } = useFontSize();
  const { settings, updateSettings } = useSettings();
  const [isCollapsed, setIsCollapsed] = useState(readCollapsedState);
  const [sidebarWidth, setSidebarWidth] = useState(readSidebarWidth);
  const [isResizing, setIsResizing] = useState(false);
  const [tooltip, setTooltip] = useState<{ label: string; top: number; left: number } | null>(null);
  const sidebarWidthRef = useRef(sidebarWidth);
  const resizeStartRef = useRef<{ x: number; width: number } | null>(null);
  const fontScale = (level === 'custom' ? customFontSize : FONT_SIZE_CONFIGS[level].baseSize + FONT_SIZE_CONFIGS[level].offset) / FONT_SIZE_CONFIGS.standard.baseSize;
  const renderedSidebarWidth = Math.round(sidebarWidth * fontScale);
  const renderedSidebarWidthMin = Math.round(SIDEBAR_WIDTH_MIN * fontScale);
  const renderedSidebarWidthMax = Math.round(SIDEBAR_WIDTH_MAX * fontScale);

  useEffect(() => {
    writeCollapsedState(isCollapsed);
  }, [isCollapsed]);

  useEffect(() => {
    sidebarWidthRef.current = sidebarWidth;
  }, [sidebarWidth]);

  useEffect(() => {
    if (!isCollapsed) setTooltip(null);
  }, [isCollapsed]);

  /**
   * Version info at the bottom.
   *
   * Only **certain facts** are shown: program version and release channel (portable/installed). These come from the app itself and install registration,
   * so there is no "measurement basis" issue — the previous "total freed" was aggregated from cleanup logs,
   * which are rotated, so the number did not equal the real total and was removed.
   */
  const [appVersion, setAppVersion] = useState('');
  /** Support-the-author dialog. Shares the same component as the settings page "Feedback - Support the Author" */
  const [isSupportOpen, setIsSupportOpen] = useState(false);

  useEffect(() => {
    getVersion()
      .then(setAppVersion)
      .catch((error) => console.warn('读取版本号失败:', error));
  }, []);

  useEffect(() => {
    const handlePointerMove = (event: PointerEvent) => {
      const resizeStart = resizeStartRef.current;
      if (!resizeStart) return;

      const nextWidth = Math.min(
        SIDEBAR_WIDTH_MAX,
        Math.max(SIDEBAR_WIDTH_MIN, Math.round(resizeStart.width + (event.clientX - resizeStart.x) / fontScale)),
      );
      sidebarWidthRef.current = nextWidth;
      setSidebarWidth(nextWidth);
    };

    const handlePointerUp = () => {
      if (!resizeStartRef.current) return;
      resizeStartRef.current = null;
      writeSidebarWidth(sidebarWidthRef.current);
      setIsResizing(false);
    };

    window.addEventListener('pointermove', handlePointerMove);
    window.addEventListener('pointerup', handlePointerUp);
    window.addEventListener('pointercancel', handlePointerUp);
    // Also end the drag when the window loses focus, so the width does not stay stuck in "dragging" after the mouse leaves the app.
    window.addEventListener('blur', handlePointerUp);
    return () => {
      window.removeEventListener('pointermove', handlePointerMove);
      window.removeEventListener('pointerup', handlePointerUp);
      window.removeEventListener('pointercancel', handlePointerUp);
      window.removeEventListener('blur', handlePointerUp);
    };
  }, [fontScale]);

  const handleNavigate = (moduleId: AppModuleId) => {
    if (moduleId === settings.activeModuleId) return;
    // Only update the active module, without touching module instances, so in-progress scan state and results are preserved.
    updateSettings({ activeModuleId: moduleId });
  };

  const toggleLabel = isCollapsed ? t('sidebarExpand') : t('sidebarCollapse');

  const handleResizeStart = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (isCollapsed) return;
    event.preventDefault();
    resizeStartRef.current = { x: event.clientX, width: sidebarWidthRef.current };
    setIsResizing(true);
  };

  const handleItemMouseEnter = (event: ReactMouseEvent<HTMLButtonElement>, label: string) => {
    if (!isCollapsed) return;
    const itemRect = event.currentTarget.getBoundingClientRect();
    setTooltip({ label, top: itemRect.top + itemRect.height / 2, left: itemRect.right + 10 });
  };

  return (
    <aside
      className={`sidebar-nav glass-chrome${isCollapsed ? ' sidebar-nav--collapsed' : ''}${isResizing ? ' sidebar-nav--resizing' : ''}`}
      style={{ '--sidebar-nav-width': `${renderedSidebarWidth}px` } as CSSProperties}
      aria-label={t('sidebarLabel')}
    >
      <div className="sidebar-nav__header">
        {!isCollapsed && <span className="sidebar-nav__title">{t('sidebarLabel')}</span>}
        <button
          type="button"
          className="sidebar-nav__toggle"
          onClick={() => setIsCollapsed((previous) => !previous)}
          aria-label={toggleLabel}
          aria-expanded={!isCollapsed}
        >
          <SidebarToggleIcon collapsed={isCollapsed} />
        </button>
      </div>

      <nav className="sidebar-nav__list">
        {SIDEBAR_GROUPS.map((group) => (
          <div key={group.id} className="sidebar-nav__group-block">
            {/* In collapsed state there is no text, so this becomes a very faint group divider line (see layout.css) */}
            <span className="sidebar-nav__group">{t(`groups.${group.id}`)}</span>
            {group.ids.map((moduleId) => {
              const meta = MODULE_META_BY_ID.get(moduleId);
              // Skip an item when metadata is missing, so the whole sidebar does not crash
              if (!meta) return null;
              const { id, label, icon: Icon } = meta;
              const isActive = settings.activeModuleId === id;
              return (
                <button
                  key={id}
                  type="button"
                  onClick={() => handleNavigate(id)}
                  className={`sidebar-nav__item${isActive ? ' sidebar-nav__item--active' : ''}`}
                  aria-current={isActive ? 'page' : undefined}
                  aria-label={t(label)}
                  onMouseEnter={(event) => handleItemMouseEnter(event, t(label))}
                  onMouseLeave={() => setTooltip(null)}
                >
                  <Icon className="sidebar-nav__item-icon" />
                  {!isCollapsed && <span className="sidebar-nav__item-label">{t(label)}</span>}
                </button>
              );
            })}
          </div>
        ))}
      </nav>

      {/* Bottom: a line of "official site + version", laid out exactly like a menu item (same left edge, same icon spacing),
          to read as the ending of the menu rather than two scattered pieces of metadata.
          Version right-aligned with tabular numbers; the channel (portable/installed) goes into title, taking no layout space. */}
      <div className="sidebar-nav__footer">
        {/* In collapsed state only the official site icon remains (4.25em cannot fit two, and the user is in "icon mode",
            where an extra entry is harder to recognize). All text goes through title/aria-label. */}
        <a
          href={LIGHTC_OFFICIAL_WEBSITE_URL}
          target="_blank"
          rel="noopener noreferrer"
          className="sidebar-nav__footer-icon"
          aria-label={t('footerWebsite')}
          title={t('footerWebsite')}
        >
          <Globe size={15} strokeWidth={1.7} />
        </a>
        {!isCollapsed && (
          <a
            href={LIGHTC_CHANGELOG_URL}
            target="_blank"
            rel="noopener noreferrer"
            className="sidebar-nav__footer-icon"
            aria-label={t('footerChangelog')}
            title={t('footerChangelog')}
          >
            <ScrollText size={15} strokeWidth={1.7} />
          </a>
        )}
        {!isCollapsed && (
          <a
            href={LIGHTC_GITHUB_URL}
            target="_blank"
            rel="noopener noreferrer"
            className="sidebar-nav__footer-icon"
            aria-label="GitHub"
            title="GitHub"
          >
            <Github size={15} strokeWidth={1.7} />
          </a>
        )}
        {!isCollapsed && (
          <a
            href={AUTHOR_HOMEPAGE_URL}
            target="_blank"
            rel="noopener noreferrer"
            className="sidebar-nav__footer-icon"
            aria-label={t('footerAuthor')}
            title={t('footerAuthor')}
          >
            <UserRound size={15} strokeWidth={1.7} />
          </a>
        )}
        {/* This used to be version text; now it is a heart: click to open the sponsorship dialog.
            The version moved to the bottom of that dialog (it is also in Settings - About).
            Deliberately not put in the hover tooltip: that only says "Sponsor LightC"; adding version/channel there would hide the fact that this is a clickable entry. */}
        {!isCollapsed && (
          <button
            type="button"
            onClick={() => setIsSupportOpen(true)}
            className="sidebar-nav__footer-icon sidebar-nav__footer-support"
            aria-label={t('footerSupport')}
            title={t('footerSupport')}
          >
            <Heart size={15} strokeWidth={1.7} />
          </button>
        )}
      </div>

      {!isCollapsed && (
        <div
          className="sidebar-nav__resize-handle"
          role="separator"
          tabIndex={0}
          aria-label={t('sidebarResize')}
          aria-orientation="vertical"
          aria-valuemin={renderedSidebarWidthMin}
          aria-valuemax={renderedSidebarWidthMax}
          aria-valuenow={renderedSidebarWidth}
          style={{ touchAction: 'none' }}
          onPointerDown={handleResizeStart}
          onKeyDown={(event) => {
            if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
            event.preventDefault();
            const direction = event.key === 'ArrowRight' ? 1 : -1;
            const nextWidth = Math.min(SIDEBAR_WIDTH_MAX, Math.max(SIDEBAR_WIDTH_MIN, sidebarWidth + direction * Math.max(1, Math.round(10 / fontScale))));
            setSidebarWidth(nextWidth);
            sidebarWidthRef.current = nextWidth;
            writeSidebarWidth(nextWidth);
          }}
        />
      )}
      {isCollapsed && tooltip && createPortal(
        <div
          className="sidebar-nav__tooltip"
          role="tooltip"
          style={{ top: `${tooltip.top}px`, left: `${tooltip.left}px` }}
        >
          {tooltip.label}
        </div>,
        document.body,
      )}

      {/* Support-the-author dialog: shared with the settings page, does not maintain its own QR code and payment switching */}
      <SupportModal isOpen={isSupportOpen} onClose={() => setIsSupportOpen(false)} version={appVersion} />
    </aside>
  );
}
