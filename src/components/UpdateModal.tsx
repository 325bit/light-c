// ============================================================================
// Update notice modal component
// Automatically checks for updates at startup and shows the notice when a new version is found.
//
// Visuals and sizing follow global settings:
// - Colors use theme variables and adapt to dark mode automatically;
// - All sizes use em, inheriting :root font-size (= --base-font-size + --font-size-offset),
//   so when users adjust the font size the modal's width/height and text scale with the main UI.
// - Enter/exit animations reuse the project's unified modal-overlay-* / modal-content-* classes (see App.css),
//   keeping consistency with ConfirmDialog / SettingsModal.
// ============================================================================

import { useState, useEffect, useCallback, useRef } from 'react';
import { createPortal } from 'react-dom';
import { X, Download, RefreshCw, CheckCircle, AlertCircle, Sparkles, FileText, AlertTriangle } from 'lucide-react';
import { check, type Update } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';
import { openUrl } from '@tauri-apps/plugin-opener';
import { getVersion } from '@tauri-apps/api/app';
import { useToast } from './Toast';
import { PortableUpdateDialog } from './settings/PortableUpdateDialog';
import { registerPortableUpdateDebugTrigger } from '../utils/portableUpdateDebug';
import { getDistributionChannel, type DistributionChannel } from '../api/commands';
import { getOfficialDownloadConfig } from '../utils/downloadConfig';
import { LIGHTC_DEFAULT_DOWNLOAD_CONFIG, LIGHTC_OFFICIAL_WEBSITE_URL } from '../config/officialLinks';
import { useTranslation } from 'react-i18next';

// ============================================================================
// Type definitions
// ============================================================================

type UpdateStatus = 
  | 'checking'      // checking
  | 'available'     // a new version is available
  | 'downloading'   // downloading
  | 'ready'         // downloaded, ready to install
  | 'error';        // error

interface UpdateModalProps {
  /** Whether to check automatically at startup */
  autoCheck?: boolean;
}

// ============================================================================
// Error message mapping
// ============================================================================

function getErrorMessage(error: unknown, translate: (key: string) => string): string {
  const errorStr = error instanceof Error ? error.message : String(error);
  
  // Network-related errors
  if (errorStr.includes('network') || errorStr.includes('fetch') || errorStr.includes('connect')) {
    return translate('updateErrorNetwork');
  }
  
  // Signature verification errors
  if (errorStr.includes('signature') || errorStr.includes('pubkey') || errorStr.includes('verify')) {
    return translate('updateErrorSignature');
  }
  
  // Timeout errors
  if (errorStr.includes('timeout') || errorStr.includes('timed out')) {
    return translate('updateErrorTimeout');
  }
  
  // 404 errors
  if (errorStr.includes('404') || errorStr.includes('not found')) {
    return translate('updateErrorNotFound');
  }
  
  // JSON parsing errors
  if (errorStr.includes('JSON') || errorStr.includes('parse')) {
    return translate('updateErrorFormat');
  }
  
  // Permission errors
  if (errorStr.includes('permission') || errorStr.includes('access denied')) {
    return translate('updateErrorPermission');
  }
  
  // Disk space errors
  if (errorStr.includes('disk') || errorStr.includes('space') || errorStr.includes('storage')) {
    return translate('updateErrorDisk');
  }
  
  return errorStr || translate('unknown');
}

// ============================================================================
// Update modal component
// ============================================================================

export function UpdateModal({ autoCheck = true }: UpdateModalProps) {
  const { t } = useTranslation('common');
  const { t: uiT } = useTranslation('ui');
  const [isOpen, setIsOpen] = useState(false);
  const [status, setStatus] = useState<UpdateStatus>('checking');
  const [update, setUpdate] = useState<Update | null>(null);
  const [currentVersion, setCurrentVersion] = useState('');
  const [downloadProgress, setDownloadProgress] = useState(0);
  const [errorMessage, setErrorMessage] = useState('');
  const [isVisible, setIsVisible] = useState(false);
  // Keep the DOM during the exit animation, otherwise animate-out would be unmounted before it can play.
  const [isAnimating, setIsAnimating] = useState(false);
  const enteredRef = useRef(false);
  if (isVisible) enteredRef.current = true;
  const [distributionChannel, setDistributionChannel] = useState<DistributionChannel | null>(null);
  // Portable-build update notice: only guides download, no install; latestVersion being null means not found or already latest.
  const [portableDialogOpen, setPortableDialogOpen] = useState(false);
  const [portableLatestVersion, setPortableLatestVersion] = useState<string | null>(null);
  const [isCheckingPortableVersion, setIsCheckingPortableVersion] = useState(false);
  const [portableCheckFailed, setPortableCheckFailed] = useState(false);
  const [downloadConfig, setDownloadConfig] = useState({ netDiskUrl: LIGHTC_DEFAULT_DOWNLOAD_CONFIG.netDiskUrl });
  const { showToast } = useToast();
  const sourceRef = useRef<'auto' | 'manual'>('auto');

  // Get the current version
  useEffect(() => {
    getVersion().then(setCurrentVersion).catch(() => setCurrentVersion(t('unknown')));
  }, []);

  // Portable builds cannot use installer-style auto-update, otherwise users would be misled into downloading and installing the NSIS package.
  useEffect(() => {
    getDistributionChannel()
      .then(setDistributionChannel)
      .catch((error) => {
        console.error('获取发行渠道失败:', error);
        setDistributionChannel('installer');
      });
  }, []);

  /**
   * Portable update guide: show the second-confirmation window immediately, then query the remote version in the background for supplementary info.
   *
   * Note: portable builds upgrade by replacing files; never call downloadAndInstall (that would silently install the NSIS package).
   * Explain clearly here that "manual replacement is needed", and give both the official website and the cloud-drive entry.
   */
  const openPortableUpdateDialog = useCallback(async () => {
    setPortableLatestVersion(null);
    setPortableCheckFailed(false);
    setPortableDialogOpen(true);
    setIsCheckingPortableVersion(true);

    // Channel URLs come from the official download.json; on failure use the built-in cloud-drive address so the button always works.
    try {
      const config = await getOfficialDownloadConfig();
      setDownloadConfig({ netDiskUrl: config.netDiskUrl });
    } catch (error) {
      console.error('读取官方下载配置失败，沿用内置网盘地址:', error);
    }

    try {
      // Only query the version for the notice; do not download or install.
      const updateResult = await check();
      setPortableLatestVersion(updateResult?.version ?? null);
      setPortableCheckFailed(false);
    } catch (error) {
      // A failed query does not affect the guide flow; the modal degrades to a pure channel entry and honestly states the version was not found.
      console.error('查询便携版最新版本失败:', error);
      setPortableLatestVersion(null);
      setPortableCheckFailed(true);
    } finally {
      setIsCheckingPortableVersion(false);
    }
  }, []);

  // Provide a console entry in development for previewing the portable modal style directly.
  useEffect(() => {
    if (!import.meta.env.DEV) return;
    registerPortableUpdateDebugTrigger((overrides = {}) => {
      setPortableLatestVersion(overrides.latestVersion ?? null);
      setIsCheckingPortableVersion(overrides.isChecking ?? false);
      setPortableDialogOpen(true);
    });
  }, []);

  /** Open the official channel; the cloud-drive address keeps using the author's cloud drive from official config. */
  const handleOpenPortableChannel = useCallback(async (url: string, channel: 'website' | 'netDisk') => {
    try {
      await openUrl(url);
      showToast({
        type: 'info',
        title: channel === 'website' ? uiT('websiteOpened') : uiT('netDiskOpened'),
        description: uiT('portableUpdateHint'),
      });
    } catch (error) {
      console.error('打开便携版下载渠道失败:', error);
      showToast({
        type: 'error',
        title: uiT('downloadOpenFailed'),
        description: uiT('downloadOpenFailedDesc'),
      });
    }
  }, [showToast, uiT]);

  // Check for updates (source: 'auto' startup automatic check / 'manual' user-triggered)
  const checkForUpdate = useCallback(async (source: 'auto' | 'manual' = 'auto') => {
    sourceRef.current = source;
    let currentDistributionChannel = distributionChannel;

    if (!currentDistributionChannel) {
      try {
        // A manual check may happen before initialization completes; fetch channels on demand so the button always gives feedback.
        currentDistributionChannel = await getDistributionChannel();
        setDistributionChannel(currentDistributionChannel);
      } catch (error) {
        // When the channel is unknown, never assume an installed build: that would make a portable build call installer-style auto-update.
        // Leave it empty here; the check below treats it as "non-installed" and opens the official download page instead.
        console.error('获取发行渠道失败，将按便携版方式提示手动下载:', error);
        currentDistributionChannel = null;
        setDistributionChannel(null);
      }
    }

    // Only an explicitly installed build may use Tauri auto-update; portable builds and unknown channels only guide download.
    if (currentDistributionChannel !== 'installer') {
      if (source === 'manual') {
        await openPortableUpdateDialog();
      }
      return;
    }

    setStatus('checking');
    setErrorMessage('');

    // On manual trigger, open the modal immediately showing loading to give instant feedback
    if (source === 'manual') {
      openModal();
    }

    try {
      const updateResult = await check();

      if (updateResult) {
        setUpdate(updateResult);
        setStatus('available');
        // In auto mode the modal needs opening; in manual mode it is already open, only switch state
        if (source === 'auto') {
          openModal();
        }
      } else if (source === 'manual') {
        // Already the latest version: close modal + toast notice
        closeModal();
        showToast({
          type: 'success',
          title: uiT('upToDate'),
          description: uiT('upToDateDesc', { version: currentVersion }),
        });
      }
      // In auto mode, no update: silent (no modal)
    } catch (error) {
      console.error('检查更新失败:', error);
      // In auto mode: fail silently, do not disturb the user with a modal
      // In manual mode: modal already open, switch to error state for display
      if (source === 'manual') {
        setErrorMessage(getErrorMessage(error, uiT));
        setStatus('error');
      }
    }
  }, [currentVersion, distributionChannel, openPortableUpdateDialog, showToast, t, uiT]);

  // Automatic check at startup
  useEffect(() => {
    if (autoCheck && distributionChannel === 'installer') {
      const timer = setTimeout(() => checkForUpdate('auto'), 2000);
      return () => clearTimeout(timer);
    }
  }, [autoCheck, checkForUpdate, distributionChannel]);

  // Listen for the manual trigger event (from SettingsModal's "Check for updates" button)
  useEffect(() => {
    const handler = () => checkForUpdate('manual');
    window.addEventListener('lightc:check-update', handler);
    return () => window.removeEventListener('lightc:check-update', handler);
  }, [checkForUpdate]);

  // Download and install the update: only installed builds may execute; portable builds never invoke the installer.
  const handleDownloadAndInstall = async () => {
    if (!update) return;
    if (distributionChannel !== 'installer') {
      // Double safeguard: even if modal state is accidentally advanced to available, do not let a portable build launch the NSIS installer.
      closeModal();
      showToast({
        type: 'warning',
        title: uiT('downloadOpenFailed'),
        description: uiT('portableUpdateBlocked'),
      });
      return;
    }
    
    setStatus('downloading');
    setDownloadProgress(0);
    
    try {
      let downloaded = 0;
      let contentLength = 0;
      
      await update.downloadAndInstall((event) => {
        if (event.event === 'Started') {
          contentLength = event.data.contentLength || 0;
        } else if (event.event === 'Progress') {
          downloaded += event.data.chunkLength;
          if (contentLength > 0) {
            setDownloadProgress((downloaded / contentLength) * 100);
          }
        }
      });
      
      setStatus('ready');
    } catch (error) {
      console.error('下载更新失败:', error);
      setErrorMessage(getErrorMessage(error, uiT));
      setStatus('error');
    }
  };

  // Restart the application
  const handleRelaunch = async () => {
    try {
      await relaunch();
    } catch (error) {
      console.error('重启失败:', error);
      setErrorMessage(uiT('relaunchFailed'));
      setStatus('error');
    }
  };

  /**
   * Open the modal.
   * Note: mount first, then set visible on the next frame so the CSS animation starts from its initial state properly;
   * also turn on isAnimating so the exit animation has DOM to play on close.
   */
  const openModal = () => {
    setIsAnimating(true);
    setIsOpen(true);
    requestAnimationFrame(() => setIsVisible(true));
  };

  /** Close the modal: play the exit animation first, then unmount after it ends (aligned with modal-content-out duration). */
  const closeModal = () => {
    setIsVisible(false);
    // 280ms is the enter animation duration and exit is fixed at 185ms; use 200ms here to match the existing ConfirmDialog.
    setTimeout(() => {
      setIsAnimating(false);
      setIsOpen(false);
    }, 200);
  };

  // Close the modal (shared by button and overlay)
  const handleClose = closeModal;

  // Retry (reuse the last trigger source)
  const handleRetry = () => {
    checkForUpdate(sourceRef.current);
  };

  if (!isOpen && !portableDialogOpen && !isAnimating) return null;

  return (
    <>
      {isOpen && createPortal(
    <div className="fixed inset-0 z-[10000] flex items-center justify-center">
      {/* Overlay */}
      <div
        className={`absolute inset-0 bg-black/50 backdrop-blur-sm glass-backdrop ${isVisible ? 'modal-overlay-in' : enteredRef.current ? 'modal-overlay-out' : 'opacity-0'}`}
        onClick={status !== 'downloading' ? handleClose : undefined}
      />

      {/* Modal body: width, height and radius all scale with the global font size, 1.86em ≈ 26px.
          Height must also have a viewport constraint, otherwise in short windows the top content and bottom buttons get pushed out of view;
          change overflow-hidden to overflow-y-auto so content scrolls inside the modal. */}
      <div className={`glass-dialog relative mx-[1em] w-[28.5em] max-w-[calc(100vw-2em)] max-h-[calc(100vh-2em)] overflow-y-auto rounded-[0.86em] border border-[var(--border-default)] bg-[var(--bg-card)] shadow-2xl ${isVisible ? 'modal-content-in' : enteredRef.current ? 'modal-content-out' : 'opacity-0'}`}>
        {/* Close button: not rendered because downloading cannot be interrupted */}
        {status !== 'downloading' && (
          <button
            type="button"
            onClick={handleClose}
            aria-label={t('close')}
            className="absolute right-[1em] top-[1em] z-10 flex h-[2em] w-[2em] items-center justify-center rounded-[0.57em] text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
          >
            <X className="h-[1.14em] w-[1.14em]" />
          </button>
        )}

        {/* Content area */}
        <div className="p-[1.43em]">
          {/* A new version is available */}
          {status === 'available' && update && (
            <>
              {/* Version title: light-green icon background + version number, same visual language as main-UI module cards */}
              <div className="flex items-start gap-[0.86em]">
                <div className="flex h-[2.57em] w-[2.57em] shrink-0 items-center justify-center rounded-[0.71em] bg-[var(--brand-green-10)]">
                  <Sparkles className="h-[1.29em] w-[1.29em] text-[var(--brand-green)]" />
                </div>
                <div className="min-w-0 flex-1">
                  <h2 className="text-[1em] font-semibold text-[var(--text-primary)]">
                    {t('updateAvailable')}
                  </h2>
                  <p className="mt-[0.14em] text-[0.86em] text-[var(--text-muted)]">
                    v{currentVersion} → v{update.version}
                  </p>
                </div>
              </div>

              {/* Release notes */}
              <div className="mt-[1.14em]">
                <div className="flex items-center gap-[0.43em]">
                  <FileText className="h-[0.86em] w-[0.86em] text-[var(--text-muted)]" />
                  <span className="text-[0.86em] font-medium text-[var(--text-secondary)]">{t('updateNotes')}</span>
                </div>
                <div className="mt-[0.57em] max-h-[14em] overflow-auto rounded-[0.71em] border border-[var(--border-color)] bg-[var(--bg-main)] p-[1em]">
                  <div className="whitespace-pre-wrap text-[0.86em] leading-relaxed text-[var(--text-secondary)]">
                    {update.body || t('updateNotes')}
                  </div>
                </div>
              </div>

              {/* Action buttons */}
              <div className="mt-[1.43em] flex gap-[0.71em]">
                <button
                  onClick={handleClose}
                  className="flex-1 rounded-[0.71em] border border-[var(--border-default)] bg-[var(--bg-card)] px-[1em] py-[0.64em] text-[0.86em] font-medium text-[var(--text-secondary)] transition-colors hover:bg-[var(--bg-hover)]"
                >
                  {t('cancel')}
                </button>
                <button
                  onClick={handleDownloadAndInstall}
                  className="flex flex-1 items-center justify-center gap-[0.43em] rounded-[0.71em] bg-[var(--brand-green)] px-[1em] py-[0.64em] text-[0.86em] font-semibold text-white transition-colors hover:bg-[var(--brand-green-hover)]"
                >
                  <Download className="h-[0.86em] w-[0.86em]" />
                  {t('confirm')}
                </button>
              </div>
            </>
          )}

          {/* Downloading */}
          {status === 'downloading' && (
            <div className="py-[1em]">
              <div className="flex items-start gap-[0.86em]">
                <div className="flex h-[2.57em] w-[2.57em] shrink-0 items-center justify-center rounded-[0.71em] bg-[var(--brand-green-10)]">
                  <RefreshCw className="h-[1.29em] w-[1.29em] animate-spin text-[var(--brand-green)]" />
                </div>
                <div className="min-w-0 flex-1">
                  <h2 className="text-[1em] font-semibold text-[var(--text-primary)]">{t('downloadingUpdate')}</h2>
                  <p className="mt-[0.14em] text-[0.86em] text-[var(--text-muted)]">{t('doNotCloseApp')}</p>
                </div>
              </div>

              {/* Progress bar: solid fill, no decorative gradient */}
              <div className="mt-[1.14em] h-[0.43em] w-full overflow-hidden rounded-full bg-[var(--bg-main)]">
                <div
                  className="h-full rounded-full bg-[var(--brand-green)] transition-all duration-300"
                  style={{ width: `${downloadProgress}%` }}
                />
              </div>
              <p className="mt-[0.57em] text-right text-[0.86em] font-medium text-[var(--brand-green)]">
                {downloadProgress.toFixed(0)}%
              </p>
            </div>
          )}

          {/* Downloaded, ready to install */}
          {status === 'ready' && (
            <>
              <div className="flex items-start gap-[0.86em]">
                <div className="flex h-[2.57em] w-[2.57em] shrink-0 items-center justify-center rounded-[0.71em] bg-[var(--brand-green-10)]">
                  <CheckCircle className="h-[1.29em] w-[1.29em] text-[var(--brand-green)]" />
                </div>
                <div className="min-w-0 flex-1">
                  <h2 className="text-[1em] font-semibold text-[var(--text-primary)]">{t('updateReady')}</h2>
                  <p className="mt-[0.14em] text-[0.86em] leading-relaxed text-[var(--text-muted)]">{t('restartToUpdate')}</p>
                </div>
              </div>

              <div className="mt-[1.43em] flex gap-[0.71em]">
                <button
                  onClick={handleClose}
                  className="flex-1 rounded-[0.71em] border border-[var(--border-default)] bg-[var(--bg-card)] px-[1em] py-[0.64em] text-[0.86em] font-medium text-[var(--text-secondary)] transition-colors hover:bg-[var(--bg-hover)]"
                >
                  {t('cancel')}
                </button>
                <button
                  onClick={handleRelaunch}
                  className="flex flex-1 items-center justify-center gap-[0.43em] rounded-[0.71em] bg-[var(--brand-green)] px-[1em] py-[0.64em] text-[0.86em] font-semibold text-white transition-colors hover:bg-[var(--brand-green-hover)]"
                >
                  <RefreshCw className="h-[0.86em] w-[0.86em]" />
                  {t('confirm')}
                </button>
              </div>
            </>
          )}

          {/* Error state */}
          {status === 'error' && (
            <>
              <div className="flex items-start gap-[0.86em]">
                <div className="flex h-[2.57em] w-[2.57em] shrink-0 items-center justify-center rounded-[0.71em] bg-[var(--color-danger)]/10">
                  <AlertCircle className="h-[1.29em] w-[1.29em] text-[var(--color-danger)]" />
                </div>
                <div className="min-w-0 flex-1">
                  <h2 className="text-[1em] font-semibold text-[var(--text-primary)]">{t('updateFailed')}</h2>
                  <p className="mt-[0.14em] break-words text-[0.86em] leading-relaxed text-[var(--color-danger)]">
                    {errorMessage}
                  </p>
                </div>
              </div>

              {/* Notice bar: light background + 1px border, dropping the old large warning color block */}
              <div className="mt-[1.14em] flex items-start gap-[0.57em] rounded-[0.71em] border border-[var(--border-color)] bg-[var(--bg-main)] px-[0.86em] py-[0.64em]">
                <AlertTriangle className="mt-[0.14em] h-[0.86em] w-[0.86em] shrink-0 text-[var(--color-warning)]" />
                <p className="text-[0.79em] leading-relaxed text-[var(--text-muted)]">
                  {t('updateFailed')}
                </p>
              </div>

              <div className="mt-[1.43em] flex gap-[0.71em]">
                <button
                  onClick={handleClose}
                  className="flex-1 rounded-[0.71em] border border-[var(--border-default)] bg-[var(--bg-card)] px-[1em] py-[0.64em] text-[0.86em] font-medium text-[var(--text-secondary)] transition-colors hover:bg-[var(--bg-hover)]"
                >
                  {t('close')}
                </button>
                <button
                  onClick={handleRetry}
                  className="flex flex-1 items-center justify-center gap-[0.43em] rounded-[0.71em] bg-[var(--brand-green)] px-[1em] py-[0.64em] text-[0.86em] font-semibold text-white transition-colors hover:bg-[var(--brand-green-hover)]"
                >
                  <RefreshCw className="h-[0.86em] w-[0.86em]" />
                  {t('retry')}
                </button>
              </div>
            </>
          )}

          {/* Checking */}
          {status === 'checking' && (
            <div className="flex items-center gap-[0.86em] py-[1.43em]">
              <div className="flex h-[2.29em] w-[2.29em] shrink-0 items-center justify-center rounded-[0.71em] bg-[var(--brand-green-10)]">
                <RefreshCw className="h-[1.14em] w-[1.14em] animate-spin text-[var(--brand-green)]" />
              </div>
              <p className="text-[0.86em] text-[var(--text-muted)]">{t('checkingUpdates')}</p>
            </div>
          )}
        </div>
      </div>
    </div>,
    document.body
      )}

      <PortableUpdateDialog
        isOpen={portableDialogOpen}
        latestVersion={portableLatestVersion}
        currentVersion={currentVersion || '...'}
        isChecking={isCheckingPortableVersion}
        checkFailed={portableCheckFailed}
        netDiskUrl={downloadConfig.netDiskUrl}
        officialWebsiteUrl={LIGHTC_OFFICIAL_WEBSITE_URL}
        onOpenChannel={handleOpenPortableChannel}
        onClose={() => setPortableDialogOpen(false)}
      />
    </>
  );
}
