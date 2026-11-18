// ============================================================================
// Startup elevation request
//
// Some modules are much faster or only usable with administrator rights (direct MFT read,
// driver package deletion, system settings). A user who always wants that can turn on
// "ask for administrator rights on startup" instead of clicking the per-module hint bar.
// ============================================================================

import { checkAdminPrivilege, relaunchAsAdmin } from '../api/commands';
import { readStoredAppSettings } from '../config/appSettings';

/** Guards against a second request in the same process (StrictMode, HMR, repeated calls). */
let requestStarted = false;

/**
 * Ask for elevation once at startup when the setting is on.
 *
 * Deliberately fire-and-forget and never awaited before the first paint: this runs while the
 * splash screen is still animating, and blocking there would freeze the window behind the
 * system prompt. The elevation restart itself kills the current process on success, so the
 * caller has nothing to clean up.
 */
export function requestAdminOnStartup(): void {
  if (requestStarted) return;
  requestStarted = true;

  if (!readStoredAppSettings().requestAdminOnStartup) return;

  void (async () => {
    // Probe first: without knowing the current state a blind relaunch could leave the user in a
    // loop of non-elevated restarts. A failed probe means the backend is not answering, which is
    // not a reason to raise a permission prompt.
    let isAdmin: boolean;
    try {
      isAdmin = await checkAdminPrivilege();
    } catch (error) {
      console.error('读取管理员权限状态失败，已跳过启动提权:', error);
      return;
    }
    if (isAdmin) return;

    try {
      const started = await relaunchAsAdmin();
      // false = the user declined the system prompt. That is a normal choice, not an error:
      // the app keeps running unelevated for this session and asks again next launch.
      if (!started) {
        console.info('用户取消了启动时的管理员权限请求，本次以普通权限运行');
      }
    } catch (error) {
      // A real failure (cannot locate the exe, ShellExecute rejected) must not block startup,
      // so only log it; every module still offers its own elevation hint.
      console.error('启动时请求管理员权限失败:', error);
    }
  })();
}
