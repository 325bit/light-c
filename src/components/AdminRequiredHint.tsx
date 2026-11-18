// ============================================================================
// Admin privilege hint bar
//
// Modules needing admin privileges share this bar: it hides itself when elevated, and shows a "Restart" button that goes through system UAC when not.
// Currently wired into 8 modules — disk change analysis / old driver cleanup / system slim / context-menu cleanup (cannot work without privileges),
// junk cleanup deep discovery / AI model space deep discovery / big files / hotspot analysis (without privileges they only fall back to a slow path, using variant="performance").
//
// Why not make it a floating overlay: it is a status hint, not a persistent action — users glance at it, either click the button or scroll past.
// Making it fixed would keep pressing on content (already hit this with the bottom action bar, needing padding added to every module),
// and modules' original positions differ anyway, so fixing it in one place would be awkward. Put it in document flow as a thin one-line bar,
// minimizing its impact on layout.
//
// How to integrate: write a line <AdminRequiredHint /> in the module, no state needs to be passed —
// privileges are checked internally and cached at module level, so multiple modules mounted at once only incur one IPC.
// ============================================================================

import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ShieldAlert, X } from 'lucide-react';
import { checkAdminPrivilege, relaunchAsAdmin } from '../api/commands';

interface AdminRequiredHintProps {
  /**
   * Tone, determining the title:
   * - `permission` (default): cannot do the task without elevation (disk change analysis / delete driver packages / change system settings)
   * - `performance`: works without elevation but much slower (direct MFT read degrades to traversal, tens of seconds vs seconds)
   *
   * The two must not be mixed: saying "admin privileges required" for a degraded scenario makes users think the feature is broken.
   */
  variant?: 'permission' | 'performance';
  /**
   * Why this feature needs admin (one sentence). When omitted, only a generic explanation is shown.
   * Modules' purposes differ greatly (read MFT / delete driver packages / change system settings), so spelling it out is more useful than a generic "need privileges".
   */
  description?: string;
  /** Allow users to manually dismiss this hint. System slim uses this; non-dismissible by default */
  dismissible?: boolean;
  /** Notify the caller when the user clicks close (caller decides whether to remember this action) */
  onDismiss?: () => void;
  /** Outer spacing, none by default (controlled by the caller's container) */
  className?: string;
}

/**
 * Privilege query is done only once: three modules may mount simultaneously, no need to send IPC each.
 * After an elevation restart it is a new process, so the cache naturally invalidates.
 */
let adminProbe: Promise<boolean> | null = null;

function probeAdminOnce(): Promise<boolean> {
  adminProbe ??= checkAdminPrivilege().catch(() => false);
  return adminProbe;
}

/**
 * Read whether currently running as admin.
 *
 * Return `null` while querying — the caller must distinguish "not known yet" from "definitely no privilege",
 * treating `null` as `false` would flash a hint and then make it disappear.
 *
 * The cache is shared with `AdminRequiredHint`, so multiple modules mounted at once still send only one IPC.
 */
export function useAdminPrivilege(): boolean | null {
  const [isAdmin, setIsAdmin] = useState<boolean | null>(null);

  useEffect(() => {
    let cancelled = false;
    void probeAdminOnce().then((result) => {
      if (!cancelled) setIsAdmin(result);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  return isAdmin;
}

export function AdminRequiredHint({
  variant = 'permission',
  description,
  dismissible = false,
  onDismiss,
  className = '',
}: AdminRequiredHintProps) {
  const { t } = useTranslation('common');
  const isAdmin = useAdminPrivilege();
  /** Elevation requested, waiting on UAC. On success the window will be closed, so do not clear the busy state */
  const [relaunching, setRelaunching] = useState(false);

  const handleRelaunch = useCallback(async () => {
    setRelaunching(true);
    try {
      const started = await relaunchAsAdmin();
      // false = user cancelled in UAC (or was already admin). Cancellation is normal, just restore the button
      if (!started) {
        setRelaunching(false);
      }
    } catch (error) {
      setRelaunching(false);
      console.error('请求管理员权限失败:', error);
    }
  }, []);

  // Do not render while querying (null) or when already privileged (true), avoiding a hint that flashes then disappears
  if (isAdmin !== false) return null;

  return (
    <div
      // Colors all go through theme variables (soft-warning uses --color-warning). Previously written as
      // `text-amber-700 dark:text-amber-400` — this project's `dark:` binds to system light/dark mode,
      // not the app's own `.dark` class, so when the app switches dark the text stays dark brown and is nearly invisible on a warm background.
      className={`soft-warning flex items-start gap-2 rounded-xl border px-3 py-2 text-xs ${className}`}
    >
      <ShieldAlert className="mt-0.5 h-4 w-4 shrink-0" />
      <div className="min-w-0 flex-1 leading-snug text-[var(--fg-secondary)]">
        <span className="font-medium text-[var(--color-warning)]">
          {variant === 'performance' ? t('adminPerformanceTitle') : t('adminRequiredTitle')}
        </span>
        {description && <span className="opacity-90"> — {description}</span>}
      </div>
      {/* Give a direct button: let users exit the program themselves, then right-click the exe and choose "Run as administrator" — this step is the biggest drop-off */}
      <button
        type="button"
        onClick={handleRelaunch}
        disabled={relaunching}
        className="shrink-0 rounded-lg bg-[color-mix(in_srgb,var(--color-warning)_18%,transparent)] px-2 py-0.5 font-medium text-[var(--color-warning)] transition-colors hover:bg-[color-mix(in_srgb,var(--color-warning)_30%,transparent)] disabled:cursor-not-allowed disabled:opacity-60"
      >
        {relaunching ? t('adminRequesting') : t('adminRelaunch')}
      </button>
      {dismissible && (
        <button
          type="button"
          onClick={onDismiss}
          aria-label={t('close')}
          className="shrink-0 rounded p-0.5 text-[var(--color-warning)] transition-colors hover:bg-[color-mix(in_srgb,var(--color-warning)_20%,transparent)]"
        >
          <X className="h-3.5 w-3.5" />
        </button>
      )}
    </div>
  );
}
