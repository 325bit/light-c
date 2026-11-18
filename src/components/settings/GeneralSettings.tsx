// ============================================================================
// General settings page
// ============================================================================

import { useEffect, useState } from 'react';
import { AnimatePresence, motion } from 'framer-motion';
import { useTranslation } from 'react-i18next';
import { CheckCircle2, ClipboardList, ChevronRight, FolderOpen, HardDrive, History, Image as ImageIcon, Info, MonitorCog, RefreshCw, Rocket, Search, ShieldCheck, Trash2, Type } from 'lucide-react';
import { Select } from '../ui/Select';
import { Switch } from '../ui/Switch';
import { useFontSize, CUSTOM_FONT_SIZE_MIN, CUSTOM_FONT_SIZE_MAX, useSettings, useTheme, useWallpaper, type Language, type ThemeMode, type WallpaperFit } from '../../contexts';
import { useToast } from '../Toast';
import { clearSelectedLocalData, getStorageLocationInfo, getStorageWriteDiagnostic, listClearableDataItems, migrateLegacyPortableData, openInFolder, openLogsFolder, openStartupManager, openStorageSettings, pickFolderDialog, setDataDirectory, type ClearableDataItem, type StorageLocationInfo, type StorageWriteDiagnostic } from '../../api/commands';
import { formatSize } from '../../utils/format';
import { getStoredSearchEngine, SEARCH_ENGINE_CHANGED_EVENT, SEARCH_ENGINE_OPTIONS, setStoredSearchEngine, type SearchEngine } from '../../utils/searchEngine';
import { ClearLocalDataDialog } from './ClearLocalDataDialog';
import { FONT_SIZE_CONFIGS, FONT_SIZE_OPTIONS, THEME_OPTIONS } from './constants';

const LANGUAGE_OPTIONS: { value: Language; labelKey: string }[] = [
  { value: 'zh', labelKey: 'language.zh' },
  { value: 'zh-TW', labelKey: 'language.zhTW' },
  { value: 'en', labelKey: 'language.en' },
  { value: 'ja', labelKey: 'language.ja' },
];

export function GeneralSettings({ mode, setMode }: { mode: ThemeMode; setMode: (mode: ThemeMode) => void }) {
  const { t } = useTranslation('settings');
  // Get the glass toggle directly from context to avoid passing another level of props into SettingsModal
  const { glass, setGlass, theme: appliedTheme } = useTheme();
  const wallpaper = useWallpaper();
  const { t: commonT } = useTranslation('common');
  const { level: fontSizeLevel, setLevel: setFontSizeLevel, customFontSize, setCustomFontSize } = useFontSize();
  const { settings, updateSettings } = useSettings();
  const { showToast } = useToast();
  const [dataDir, setDataDir] = useState('');
  const [storageInfo, setStorageInfo] = useState<StorageLocationInfo | null>(null);
  const [writeDiagnostic, setWriteDiagnostic] = useState<StorageWriteDiagnostic | null>(null);
  /** UI data directory: only set for the portable build; extract to a string up front so optional chaining does not lose narrowing in callbacks. */
  const webviewDataDir = storageInfo?.webview_data_directory ?? '';
  const [isChangingDir, setIsChangingDir] = useState(false);
  const [isMigratingLegacyData, setIsMigratingLegacyData] = useState(false);
  const [isClearing, setIsClearing] = useState(false);
  const [clearDialogOpen, setClearDialogOpen] = useState(false);
  const [clearableItems, setClearableItems] = useState<ClearableDataItem[]>([]);
  const [selectedClearItemIds, setSelectedClearItemIds] = useState<string[]>([]);
  const [customFontSizeDraft, setCustomFontSizeDraft] = useState(String(customFontSize));

  /** All wallpaper operations give feedback here: choosing, installing, and removing can all fail (except canceling the picker). */
  const handleChooseWallpaper = async () => {
    try {
      await wallpaper.choose();
    } catch (error) {
      showToast({ type: 'error', title: t('wallpaper.pickFailed'), description: String(error) });
    }
  };

  const handleRemoveWallpaper = async () => {
    try {
      await wallpaper.remove();
    } catch (error) {
      showToast({ type: 'error', title: t('wallpaper.removeFailed'), description: String(error) });
    }
  };

  /**
   * Expose "wallpaper opacity" in the UI: a higher value makes the image clearer, which matches intuition.
   * Internally it stores dim intensity (higher is blurrier); the two are complementary, so both read and write need a 100 - x conversion.
   * The storage format is unchanged, so old configs remain compatible.
   */
  const activeDim = appliedTheme === 'dark' ? wallpaper.dimDark : wallpaper.dimLight;
  const wallpaperOpacity = 100 - activeDim;
  const setWallpaperOpacity = (opacity: number) => {
    const dim = 100 - opacity;
    if (appliedTheme === 'dark') wallpaper.setDimDark(dim);
    else wallpaper.setDimLight(dim);
  };

  useEffect(() => {
    setCustomFontSizeDraft(String(customFontSize));
  }, [customFontSize]);

  // The backend returns the distribution mode and paths uniformly, avoiding frontend re-inference of portable directories.
  useEffect(() => {
    getStorageLocationInfo()
      .then((info) => {
        setStorageInfo(info);
        setDataDir(info.current_data_directory);
      })
      .catch(() => setDataDir(commonT('unknown')));

    // When the portable package is extracted into a non-writable directory, tell the user explicitly that data temporarily goes to AppData,
    // otherwise the user would think the portable build is writing to the C: drive again.
    getStorageWriteDiagnostic()
      .then(setWriteDiagnostic)
      .catch((error) => console.error('读取便携目录写入诊断失败:', error));
  }, []);

  const handleOpenLogsFolder = async () => {
    try {
      await openLogsFolder();
    } catch (error) {
      console.error('打开日志文件夹失败:', error);
    }
  };

  const handleOpenStoragePath = async (path: string, label: string) => {
    try {
      await openInFolder(path);
    } catch (error) {
      showToast({
        type: 'error',
        title: t('dataDir.openFailed', { label }),
        description: String(error),
      });
    }
  };

  // Change the data directory
  const handleChangeDataDir = async () => {
    try {
      setIsChangingDir(true);
      const folder = await pickFolderDialog();
      if (!folder) { setIsChangingDir(false); return; }
      await setDataDirectory(folder);
      try {
        const info = await getStorageLocationInfo();
        setStorageInfo(info);
        setDataDir(info.current_data_directory);
      } catch {
        // When the set command has already succeeded, a state-refresh failure must not be misreported as a migration failure.
        setDataDir(folder);
      }
      showToast({
        type: 'success',
        title: t('dataDir.changeDirSuccess'),
        description: folder,
      });
    } catch (error) {
      console.error('更改数据目录失败:', error);
      showToast({
        type: 'error',
        title: t('dataDir.changeDirFailed'),
        description: String(error),
      });
    } finally {
      setIsChangingDir(false);
    }
  };

  const handleMigrateLegacyData = async () => {
    try {
      setIsMigratingLegacyData(true);
      const info = await migrateLegacyPortableData();
      setStorageInfo(info);
      setDataDir(info.current_data_directory);
      showToast({
        type: 'success',
        title: t('dataDir.migrateSuccess'),
        description: t('dataDir.migrateSuccessDesc'),
      });
    } catch (error) {
      showToast({
        type: 'error',
        title: t('dataDir.migrateFailed'),
        description: String(error),
      });
    } finally {
      setIsMigratingLegacyData(false);
    }
  };

  // Clear local data
  const handleClearData = async () => {
    try {
      setIsClearing(true);
      const items = await listClearableDataItems();
      setClearableItems(items);
      // Driver backup files are usually large and serve manual recovery after accidental deletion; cleaning them requires separate user confirmation.
      setSelectedClearItemIds(items
        .filter(item => item.id !== 'driver_backups' && item.exists && item.file_count > 0)
        .map(item => item.id));
      setClearDialogOpen(true);
    } catch (error) {
      showToast({
        type: 'error',
        title: t('dataDir.readItemsFailed'),
        description: String(error),
      });
    } finally {
      setIsClearing(false);
    }
  };

  const executeClearData = async () => {
    if (selectedClearItemIds.length === 0) {
      showToast({ type: 'info', title: t('dataDir.noItemsSelected') });
      return;
    }

    try {
      setIsClearing(true);
      const result = await clearSelectedLocalData(selectedClearItemIds);
      setClearDialogOpen(false);

      // A single-item failure no longer aborts the whole batch, so report the result in three cases:
      // all success: report only the freed amount; partial failure: add "which item was not deleted and why"; total failure: report an error directly.
      const failedCount = result.failed_items.length;
      const firstFailureReason = result.failed_items[0]?.reason ?? '';

      if (failedCount === 0) {
        showToast({
          type: 'success',
          title: t('dataDir.clearSuccess'),
          description: t('dataDir.clearSuccessDesc', { files: result.deleted_files, size: formatSize(result.freed_bytes) }),
        });
      } else if (result.cleared_items === 0) {
        showToast({
          type: 'error',
          title: t('dataDir.clearFailed'),
          description: t('dataDir.clearAllFailedDesc', { count: failedCount, detail: firstFailureReason }),
        });
      } else {
        showToast({
          type: 'warning',
          title: t('dataDir.clearPartial'),
          description: t('dataDir.clearPartialDesc', {
            cleared: result.cleared_items,
            failed: failedCount,
            size: formatSize(result.freed_bytes),
            detail: firstFailureReason,
          }),
        });
      }
    } catch (error) {
      showToast({
        type: 'error',
        title: t('dataDir.clearFailed'),
        description: String(error),
      });
    } finally {
      setIsClearing(false);
    }
  };

  const toggleClearItem = (itemId: string) => {
    setSelectedClearItemIds(prev =>
      prev.includes(itemId) ? prev.filter(id => id !== itemId) : [...prev, itemId]
    );
  };

  return (
    <div className="space-y-6">
      {/* General settings */}
      <div className="space-y-3">
        <h4 className="text-xs font-medium text-[var(--text-muted)] uppercase tracking-wider flex items-center gap-2">
          <MonitorCog className="w-3.5 h-3.5" />
          {t('sections.general')}
        </h4>
        <div className="glass-block bg-[var(--bg-main)] rounded-2xl p-5 space-y-5">
          {/* Theme mode */}
          <div className="flex items-center justify-between">
            <div>
              <p className="text-sm font-medium text-[var(--text-primary)]">{t('theme.label')}</p>
              <p className="text-xs text-[var(--text-muted)] mt-1">{t('theme.desc')}</p>
            </div>
            {/* Segmented control - icons only */}
            <div className="flex items-center gap-1 p-1 bg-[var(--bg-card)] rounded-xl border border-[var(--border-color)]">
              {THEME_OPTIONS.map(({ mode: m, label, icon: Icon }) => (
                <button
                  key={m}
                  onClick={() => setMode(m)}
                  title={t(label)}
                  className={`flex items-center justify-center p-2 rounded-lg transition-all duration-200 ${mode === m
                      ? 'bg-[var(--brand-green)] text-white'
                      : 'text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]'
                    }`}
                >
                  <Icon className="w-4 h-4" aria-label={t(label)} />
                </button>
              ))}
            </div>
          </div>

          {/* Liquid glass: off by default. When enabled, the top surface continuously consumes GPU compositing resources,
              so state the cost directly in the UI rather than waiting for the user to notice lag */}
          <div className="flex items-center justify-between gap-4 border-t border-[var(--border-color)] pt-4">
            <div className="min-w-0">
              {/* Use ASCII parentheses for the Beta marker: this is the only hardcoded suffix,
                  using fullwidth brackets would look like Chinese punctuation mixed into the Western UI */}
              <p className="text-sm font-medium text-[var(--text-primary)]">{t('theme.glassLabel')} (Beta)</p>
              <p className="mt-1 text-xs text-[var(--text-muted)]">{t('theme.glassDesc')}</p>
              <p className="mt-1 text-xs text-[var(--color-warning)]">{t('theme.glassWarning')}</p>
            </div>
            <Switch
              checked={glass}
              onChange={setGlass}
              aria-label={t('theme.glassLabel')}
            />
          </div>

          {/* Background wallpaper: the image is stored in the data directory; only the toggle and fit options live here. */}
          <div className="border-t border-[var(--border-color)] pt-4 space-y-3">
            <div className="flex items-center justify-between gap-3">
              <div className="min-w-0">
                <p className="text-sm font-medium text-[var(--text-primary)] flex items-center gap-1.5">
                  <ImageIcon className="w-4 h-4 text-[var(--text-muted)]" />
                  {t('wallpaper.label')}
                </p>
                <p className="text-xs text-[var(--text-muted)] mt-1">{t('wallpaper.desc')}</p>
              </div>
              <div className="flex items-center gap-2 shrink-0">
                <button
                  onClick={() => void handleChooseWallpaper()}
                  className="px-3 py-1.5 text-xs rounded-lg border border-[var(--brand-green)] bg-[var(--brand-green)] text-white hover:bg-[var(--brand-green-hover)] transition-colors"
                >
                  {t(wallpaper.active ? 'wallpaper.replace' : 'wallpaper.choose')}
                </button>
                {wallpaper.active && (
                  <button
                    onClick={() => void handleRemoveWallpaper()}
                    className="px-3 py-1.5 text-xs rounded-lg border border-[var(--border-color)] text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] transition-colors"
                  >
                    {t('wallpaper.remove')}
                  </button>
                )}
              </div>
            </div>

            {/* Hide the detailed options when no wallpaper is set, to avoid a screen full of unusable controls. */}
            {wallpaper.active && (
              <div className="space-y-3 pl-5 border-l-2 border-[var(--border-muted)]">
                <div className="flex items-center justify-between gap-3">
                  <p className="text-xs text-[var(--text-secondary)]">{t('wallpaper.fitLabel')}</p>
                  <Select<WallpaperFit>
                    value={wallpaper.fit}
                    options={[
                      { value: 'cover', label: t('wallpaper.fitCover') },
                      { value: 'contain', label: t('wallpaper.fitContain') },
                      { value: 'center', label: t('wallpaper.fitCenter') },
                    ]}
                    onChange={wallpaper.setFit}
                    widthClass="w-32"
                  />
                </div>

                {/* Show only the opacity for the current theme; light and dark are stored separately so switching themes needs no re-adjustment. */}
                <div className="flex items-center justify-between gap-4">
                  <p className="text-xs text-[var(--text-secondary)]">{t('wallpaper.opacityLabel')}</p>
                  <div className="flex items-center gap-2">
                    <input
                      type="range"
                      min={0}
                      max={100}
                      step={5}
                      value={wallpaperOpacity}
                      onChange={(event) => setWallpaperOpacity(Number(event.target.value))}
                      className="w-28 accent-[var(--brand-green)]"
                      aria-label={t('wallpaper.opacityLabel')}
                    />
                    <span className="w-8 text-xs tabular-nums text-[var(--text-muted)]">{wallpaperOpacity}</span>
                  </div>
                </div>

              </div>
            )}
          </div>

          {/* Use the common dropdown so varying language-name lengths do not stretch the settings page layout. */}
          <div className="flex items-center justify-between border-t border-[var(--border-color)] pt-4">
            <div>
              <p className="text-sm font-medium text-[var(--text-primary)]">{t('language.label')}</p>
              <p className="mt-1 text-xs text-[var(--text-muted)]">{t('language.desc')}</p>
            </div>
            <Select<Language>
              value={settings.language}
              options={LANGUAGE_OPTIONS.map(({ value, labelKey }) => ({
                value,
                label: t(labelKey),
              }))}
              onChange={(language) => updateSettings({ language })}
              widthClass="w-32"
            />
          </div>

          {/* Font size */}
          <div className="pt-4 border-t border-[var(--border-color)]">
            <div className="flex flex-wrap items-center gap-3">
            <div className="min-w-[10.0rem] flex-1">
              <p className="text-sm font-medium text-[var(--text-primary)] flex items-center gap-1.5">
                <Type className="w-4 h-4 text-[var(--text-muted)]" />
                {t('fontSize.label')}
              </p>
              <p className="text-xs text-[var(--text-muted)] mt-1">{t('fontSize.desc')}</p>
            </div>
            {/* Font size segmented control */}
            <div className="flex max-w-full shrink-0 flex-wrap items-center gap-1 p-1 bg-[var(--bg-card)] rounded-xl border border-[var(--border-color)]">
              {FONT_SIZE_OPTIONS.map(({ level, label }) => (
                <button
                  key={level}
                  onClick={() => setFontSizeLevel(level)}
                  title={level === 'custom'
                    ? `${t(label)} (${customFontSize}px)`
                    : `${t(label)} (+${FONT_SIZE_CONFIGS[level].offset}px)`}
                  className={`whitespace-nowrap px-3 py-2 rounded-lg text-xs font-medium transition-all duration-200 ${fontSizeLevel === level
                      ? 'bg-[var(--brand-green)] text-white'
                      : 'text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]'
                    }`}
                >
                  {t(label)}
                </button>
              ))}
            </div>
            </div>

            {/* Expand custom font size separately so it does not take up space when unselected. */}
            <AnimatePresence initial={false}>
              {fontSizeLevel === 'custom' && (
                <motion.div
                  initial={{ opacity: 0, height: 0, y: -6 }}
                  animate={{ opacity: 1, height: 'auto', y: 0 }}
                  exit={{ opacity: 0, height: 0, y: -6 }}
                  transition={{ duration: 0.2, ease: 'easeOut' }}
                  className="overflow-hidden"
                >
                  <div className="glass-block mt-3 flex items-center justify-between gap-3 rounded-xl border border-[var(--border-color)] bg-[var(--bg-main)] px-3 py-2.5">
                    <div className="min-w-0">
                      <p className="text-xs font-medium text-[var(--text-secondary)]">{t('fontSize.customLabel')}</p>
                      <p className="mt-0.5 text-[0.79rem] text-[var(--text-muted)]">{t('fontSize.customRange', { min: CUSTOM_FONT_SIZE_MIN, max: CUSTOM_FONT_SIZE_MAX })}</p>
                    </div>
                    <div className="flex shrink-0 items-center gap-2">
                      <input
                        type="number"
                        min={CUSTOM_FONT_SIZE_MIN}
                        max={CUSTOM_FONT_SIZE_MAX}
                        step={1}
                        value={customFontSizeDraft}
                        onChange={(event) => setCustomFontSizeDraft(event.target.value)}
                        onBlur={() => {
                          const parsedValue = Number(customFontSizeDraft);
                          const nextValue = Number.isFinite(parsedValue)
                            ? Math.min(CUSTOM_FONT_SIZE_MAX, Math.max(CUSTOM_FONT_SIZE_MIN, Math.floor(parsedValue)))
                            : customFontSize;
                          setCustomFontSize(nextValue);
                          setCustomFontSizeDraft(String(nextValue));
                        }}
                        className="h-9 w-20 rounded-xl border border-[var(--border-color)] bg-[var(--bg-card)] px-3 text-right text-sm text-[var(--text-primary)] outline-none transition focus:border-[var(--brand-green)]"
                        title={t('fontSize.customTitle', { min: CUSTOM_FONT_SIZE_MIN, max: CUSTOM_FONT_SIZE_MAX })}
                      />
                      <span className="text-xs text-[var(--text-muted)]">px</span>
                    </div>
                  </div>
                </motion.div>
              )}
            </AnimatePresence>
          </div>

          <SearchEngineSettings />

          {/* Cleanup log retention */}
          <div className="flex items-center justify-between pt-4 border-t border-[var(--border-color)]">
            <div>
              <p className="text-sm font-medium text-[var(--text-primary)] flex items-center gap-1.5">
                <ClipboardList className="w-4 h-4 text-[var(--text-muted)]" />
                {t('logRetention.label')}
              </p>
              <p className="text-xs text-[var(--text-muted)] mt-1">{t('logRetention.desc')}</p>
            </div>
            <div className="flex items-center gap-2">
              <input
                type="number"
                min={1}
                max={100}
                step={1}
                value={settings.cleanupLogRetention}
                onChange={(event) => {
                  const nextValue = Math.min(100, Math.max(1, Math.floor(Number(event.target.value) || 10)));
                  updateSettings({ cleanupLogRetention: nextValue });
                }}
                className="h-9 w-20 rounded-xl border border-[var(--border-color)] bg-[var(--bg-card)] px-3 text-right text-sm text-[var(--text-primary)] outline-none transition focus:border-[var(--brand-green)]"
                title={t('logRetention.title')}
              />
              <span className="text-xs text-[var(--text-muted)]">{t('logRetention.unit')}</span>
            </div>
          </div>

          {/* Startup elevation: users who always want the fast path (MFT read, protected paths)
              should not have to click the in-module hint bar on every launch. Off by default —
              a system prompt at each start is intrusive for people who only clean caches. */}
          <div className="flex items-center justify-between gap-4 border-t border-[var(--border-color)] pt-4">
            <div className="min-w-0">
              <p className="text-sm font-medium text-[var(--text-primary)] flex items-center gap-1.5">
                <ShieldCheck className="w-4 h-4 text-[var(--text-muted)]" />
                {t('adminOnStartup.label')}
              </p>
              <p className="text-xs text-[var(--text-muted)] mt-1">{t('adminOnStartup.desc')}</p>
            </div>
            <Switch
              checked={settings.requestAdminOnStartup}
              onChange={(checked) => updateSettings({ requestAdminOnStartup: checked })}
              aria-label={t('adminOnStartup.label')}
            />
          </div>
        </div>
      </div>

      {/* Data management */}
      <div className="space-y-3 pt-2 border-t border-[var(--border-color)]">
        <h4 className="text-xs font-medium text-[var(--text-muted)] uppercase tracking-wider flex items-center gap-2">
          <History className="w-3.5 h-3.5" />
          {t('sections.dataManagement')}
        </h4>
        <div className="glass-block bg-[var(--bg-main)] rounded-2xl divide-y divide-[var(--border-color)]">
          {/* Current storage location */}
          <div className="space-y-3 p-4">
            <div className="flex items-center justify-between gap-3">
              <span className="shrink-0 text-xs text-[var(--text-muted)]">{t('dataDir.configLocation')}</span>
              <div className="flex min-w-0 items-center gap-2">
                <span className="max-w-[16.43rem] truncate text-right text-[0.71rem] text-[var(--text-faint)]" title={storageInfo?.config_file}>
                  {storageInfo?.config_file ? shortenPathMiddle(storageInfo.config_file) : t('dataDir.loadingPath')}
                </span>
                <button
                  onClick={() => storageInfo?.config_file && handleOpenStoragePath(storageInfo.config_file, t('dataDir.configLocation'))}
                  disabled={!storageInfo?.config_file}
                  className="shrink-0 text-[0.71rem] text-[var(--brand-green)] transition hover:opacity-80 disabled:opacity-40"
                >
                  {commonT('go')}
                </button>
              </div>
            </div>
            {webviewDataDir && (
              <div className="flex items-center justify-between gap-3">
                <span className="shrink-0 text-xs text-[var(--text-muted)]">{t('dataDir.webviewData')}</span>
                {/* Consistent with "config location" and "data location": a truncatable path + a Go button.
                    This row appears only for the portable build (the installed build uses the system default directory), but if it can be opened it should be clickable. */}
                <div className="flex min-w-0 items-center gap-2">
                  <span
                    className="max-w-[16.43rem] truncate text-right text-[0.71rem] text-[var(--text-faint)]"
                    title={webviewDataDir}
                  >
                    {shortenPathMiddle(webviewDataDir)}
                  </span>
                  <button
                    onClick={() => handleOpenStoragePath(webviewDataDir, t('dataDir.webviewData'))}
                    className="shrink-0 text-[0.71rem] text-[var(--brand-green)] transition hover:opacity-80"
                  >
                    {commonT('go')}
                  </button>
                </div>
              </div>
            )}
            <div className="flex items-center justify-between gap-3">
              <span className="shrink-0 text-xs text-[var(--text-muted)]">{t('dataDir.dataLocation')}</span>
              <div className="flex min-w-0 items-center gap-2">
                <span className="max-w-[16.43rem] truncate text-[0.71rem] text-[var(--text-faint)]" title={dataDir}>
                  {dataDir || t('dataDir.loadingPath')}
                </span>
                <button
                  onClick={() => handleOpenStoragePath(dataDir, t('dataDir.dataLocation'))}
                  className="shrink-0 text-[0.71rem] text-[var(--brand-green)] transition hover:opacity-80"
                >
                  {commonT('go')}
                </button>
              </div>
            </div>
            {writeDiagnostic?.portable_root && writeDiagnostic.preferred_writable === false && (
              // The portable directory is not writable: data can only be temporarily stored in AppData; tell the user clearly and provide a solution.
              <div className="space-y-1 rounded-xl bg-[var(--color-warning)]/10 px-3 py-2">
                <p className="flex items-start gap-1.5 text-[0.79rem] text-[var(--color-warning)]">
                  <Info className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                  {writeDiagnostic.active_writable
                    ? t('dataDir.portableFallbackWarning', { path: writeDiagnostic.preferred_data_directory })
                    : t('dataDir.portableReadOnlyBlocked')}
                </p>
                <p className="break-all pl-5 text-[0.71rem] text-[var(--text-faint)]">
                  {t('dataDir.portableRootLabel')}: {writeDiagnostic.portable_root}
                </p>
              </div>
            )}
            {storageInfo && !storageInfo.can_write && (
              <p className="flex items-start gap-1.5 text-[0.79rem] text-[var(--color-danger)]">
                <Info className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                {t('dataDir.cannotWrite')}
              </p>
            )}
            {storageInfo?.migration_completed && storageInfo.distribution_channel === 'portable' && (
              <p className="flex items-center gap-1.5 text-[0.79rem] text-[var(--brand-green)]">
                <CheckCircle2 className="h-3.5 w-3.5 shrink-0" />
                {t('dataDir.migrationDone')}
              </p>
            )}
            {storageInfo?.migration_available && (
              <button
                onClick={handleMigrateLegacyData}
                disabled={isMigratingLegacyData}
                className="flex w-full items-center justify-center gap-2 rounded-xl border border-[var(--brand-green)] px-3 py-2 text-xs text-[var(--brand-green)] transition hover:bg-[var(--brand-green-10)] disabled:opacity-50"
              >
                <RefreshCw className={`h-3.5 w-3.5 ${isMigratingLegacyData ? 'animate-spin' : ''}`} />
                {isMigratingLegacyData ? t('dataDir.migratingBtn') : t('dataDir.migrateBtn')}
              </button>
            )}
          </div>
          {/* Change data directory */}
          <button
            onClick={handleChangeDataDir}
            disabled={isChangingDir}
            className="w-full flex items-center justify-between p-4 hover:bg-[var(--bg-hover)] transition-colors group disabled:opacity-50"
          >
            <div className="flex items-center gap-3">
              <div className="w-9 h-9 rounded-xl bg-[var(--brand-green-10)] flex items-center justify-center">
                {isChangingDir ? (
                  <RefreshCw className="w-4.5 h-4.5 text-[var(--brand-green)] animate-spin" />
                ) : (
                  <FolderOpen className="w-4.5 h-4.5 text-[var(--brand-green)]" />
                )}
              </div>
              <div className="text-left">
                <p className="text-sm font-medium text-[var(--text-primary)]">{t('dataDir.changeDir')}</p>
                <p className="text-xs text-[var(--text-muted)] mt-0.5">{t('dataDir.changeDirDesc')}</p>
              </div>
            </div>
            <ChevronRight className="w-4 h-4 text-[var(--text-muted)] group-hover:text-[var(--text-secondary)] transition-colors" />
          </button>
          {/* Open logs folder */}
          <button
            onClick={handleOpenLogsFolder}
            className="w-full flex items-center justify-between p-4 hover:bg-[var(--bg-hover)] transition-colors group"
          >
            <div className="flex items-center gap-3">
              <div className="w-9 h-9 rounded-xl bg-[var(--brand-green-10)] flex items-center justify-center">
                <History className="w-4.5 h-4.5 text-[var(--brand-green)]" />
              </div>
              <div className="text-left">
                <p className="text-sm font-medium text-[var(--text-primary)]">{t('dataDir.viewLogs')}</p>
                <p className="text-xs text-[var(--text-muted)] mt-0.5">{t('dataDir.viewLogsDesc')}</p>
              </div>
            </div>
            <ChevronRight className="w-4 h-4 text-[var(--text-muted)] group-hover:text-[var(--text-secondary)] transition-colors" />
          </button>
          {/* Clear local data */}
          <button
            onClick={handleClearData}
            disabled={isClearing}
            className="w-full flex items-center justify-between p-4 hover:bg-[var(--bg-hover)] rounded-b-2xl transition-colors group disabled:opacity-50"
          >
            <div className="flex items-center gap-3">
              <div className="w-9 h-9 rounded-xl bg-[var(--color-danger)]/10 flex items-center justify-center">
                {isClearing ? (
                  <RefreshCw className="w-4.5 h-4.5 text-[var(--color-danger)] animate-spin" />
                ) : (
                  <Trash2 className="w-4.5 h-4.5 text-[var(--color-danger)]" />
                )}
              </div>
              <div className="text-left">
                <p className="text-sm font-medium text-[var(--text-primary)]">{t('dataDir.clearData')}</p>
                <p className="text-xs text-[var(--text-muted)] mt-0.5">{t('dataDir.clearDataDesc')}</p>
              </div>
            </div>
            <ChevronRight className="w-4 h-4 text-[var(--text-muted)] group-hover:text-[var(--text-secondary)] transition-colors" />
          </button>
        </div>
      </div>

      <ClearLocalDataDialog
        isOpen={clearDialogOpen}
        items={clearableItems}
        selectedIds={selectedClearItemIds}
        isClearing={isClearing}
        onToggleItem={toggleClearItem}
        onCancel={() => setClearDialogOpen(false)}
        onConfirm={executeClearData}
      />

      {/* System quick tools */}
      <div className="space-y-3 pt-2 border-t border-[var(--border-color)]">
        <h4 className="text-xs font-medium text-[var(--text-muted)] uppercase tracking-wider flex items-center gap-2">
          <Rocket className="w-3.5 h-3.5" />
          {t('sections.systemTools')}
        </h4>
        <div className="glass-block bg-[var(--bg-main)] rounded-2xl divide-y divide-[var(--border-color)]">
          {/* Startup management */}
          <button
            onClick={() => openStartupManager().catch(console.error)}
            className="w-full flex items-center justify-between p-4 hover:bg-[var(--bg-hover)] first:rounded-t-2xl transition-colors group"
          >
            <div className="flex items-center gap-3">
              <div className="w-9 h-9 rounded-xl bg-[var(--brand-green-10)] flex items-center justify-center">
                <Rocket className="w-4.5 h-4.5 text-[var(--brand-green)]" />
              </div>
              <div className="text-left">
                <p className="text-sm font-medium text-[var(--text-primary)]">{t('systemTools.startup')}</p>
                <p className="text-xs text-[var(--text-muted)] mt-0.5">{t('systemTools.startupDesc')}</p>
              </div>
            </div>
            <ChevronRight className="w-4 h-4 text-[var(--text-muted)] group-hover:text-[var(--text-secondary)] transition-colors" />
          </button>
          {/* Storage sense */}
          <button
            onClick={() => openStorageSettings().catch(console.error)}
            className="w-full flex items-center justify-between p-4 hover:bg-[var(--bg-hover)] last:rounded-b-2xl transition-colors group"
          >
            <div className="flex items-center gap-3">
              <div className="w-9 h-9 rounded-xl bg-[var(--brand-green-10)] flex items-center justify-center">
                <HardDrive className="w-4.5 h-4.5 text-[var(--brand-green)]" />
              </div>
              <div className="text-left">
                <p className="text-sm font-medium text-[var(--text-primary)]">{t('systemTools.storage')}</p>
                <p className="text-xs text-[var(--text-muted)] mt-0.5">{t('systemTools.storageDesc')}</p>
              </div>
            </div>
            <ChevronRight className="w-4 h-4 text-[var(--text-muted)] group-hover:text-[var(--text-secondary)] transition-colors" />
          </button>
        </div>
      </div>
    </div>
  );
}

// Middle elision preserves the drive letter, directory levels, and config filename; the user can still see the full path via the title.
function shortenPathMiddle(path: string, maxLength = 54): string {
  if (path.length <= maxLength) {
    return path;
  }

  const visibleLength = maxLength - 3;
  const leftLength = Math.ceil(visibleLength * 0.45);
  const rightLength = visibleLength - leftLength;
  return `${path.slice(0, leftLength)}...${path.slice(-rightLength)}`;
}

function SearchEngineSettings() {
  const { t } = useTranslation('settings');
  const [searchEngine, setSearchEngine] = useState<SearchEngine>(() => getStoredSearchEngine());

  useEffect(() => {
    const handleSearchEngineChange = (event: Event) => {
      const nextEngine = (event as CustomEvent<SearchEngine>).detail;
      setSearchEngine(nextEngine);
    };

    window.addEventListener(SEARCH_ENGINE_CHANGED_EVENT, handleSearchEngineChange);
    return () => window.removeEventListener(SEARCH_ENGINE_CHANGED_EVENT, handleSearchEngineChange);
  }, []);

  const handleChange = (engine: SearchEngine) => {
    setSearchEngine(engine);
    setStoredSearchEngine(engine);
  };

  return (
    <div className="flex items-center justify-between pt-4 border-t border-[var(--border-color)]">
      <div>
        <p className="text-sm font-medium text-[var(--text-primary)] flex items-center gap-1.5">
          <Search className="w-4 h-4 text-[var(--text-muted)]" />
          {t('searchEngine.label')}
        </p>
        <p className="text-xs text-[var(--text-muted)] mt-1">
          {t('searchEngine.desc')}
        </p>
      </div>
      <Select<SearchEngine>
        value={searchEngine}
        options={SEARCH_ENGINE_OPTIONS}
        onChange={handleChange}
        widthClass="w-32"
      />
    </div>
  );
}
