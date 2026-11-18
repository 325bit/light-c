import { useAiModuleBridge } from '../ai/useAiModuleBridge';
import { AiAskButton } from '../ai/AiAskButton';
import { buildItemQuestion } from '../ai/aiQuestions';
// ============================================================================
// Big file cleanup module component
// Shows big-file scanning and cleanup in the dashboard
// ============================================================================

import { useState, useCallback, useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { FileBox, Trash2, FileWarning, FolderOpen, Copy, StopCircle, Search } from 'lucide-react';
import { ScanTimingPopover } from '../ScanTimingPopover';
import { AdminRequiredHint } from '../AdminRequiredHint';
import { listen } from '@tauri-apps/api/event';
import { ModuleCard } from '../ModuleCard';
import { ModuleDeleteProgress } from '../ModuleDeleteProgress';
import { ModuleOperationToolbar } from '../ModuleOperationToolbar';
import { ModuleScanProgress } from '../ModuleScanProgress';
import { ConfirmDialog } from '../ConfirmDialog';
import { EmptyState } from '../EmptyState';
import { useToast } from '../Toast';
import {
  defaultDriveLetter,
  DriveSelect,
  driveDisplayName,
  normalizeDriveLetter,
  useLocalDrives,
} from '../ui/DriveSelect';
import { useModuleDashboard } from '../../contexts/DashboardContext';
import { useSettings } from '../../contexts';
import { scanLargeFiles, cancelLargeFileScan, deleteFiles, openInFolder, recordCleanupAction, type CleanupLogEntryInput } from '../../api/commands';
import { formatSize, formatDate, getRiskLevelColor, getRiskLevelBgColor, getRiskLevelKey } from '../../utils/format';
import { openSearchUrl } from '../../utils/searchEngine';
import type { LargeFileEntry, LargeFileScanProgress } from '../../types';
import type { ModuleRenderProps } from './moduleProps';

// The backend currently returns fixed source labels; map only known enums and do not display unknown values, to avoid mistranslating real file info.
function getLocalizedSourceLabel(sourceLabel: string, translate: (key: string) => string): string {
  const sourceKey: Record<string, string> = {
    '虚拟机磁盘': 'bigFiles.source.virtualDisk',
    '内存转储': 'bigFiles.source.memoryDump',
    '光盘镜像': 'bigFiles.source.diskImage',
    '数据库文件': 'bigFiles.source.database',
    '压缩包': 'bigFiles.source.archive',
    '日志文件': 'bigFiles.source.log',
    '系统临时文件': 'bigFiles.source.systemTemp',
    'Windows 更新缓存': 'bigFiles.source.windowsUpdate',
    'Steam 游戏文件': 'bigFiles.source.steam',
    '微信文件': 'bigFiles.source.wechat',
    'Chrome 浏览器': 'bigFiles.source.chrome',
    'Edge 浏览器': 'bigFiles.source.edge',
    'Firefox 浏览器': 'bigFiles.source.firefox',
    '下载文件': 'bigFiles.source.downloads',
    '桌面文件': 'bigFiles.source.desktop',
    '回收站': 'bigFiles.source.recycleBin',
    '视频文件': 'bigFiles.source.video',
    '音频文件': 'bigFiles.source.audio',
    '图片文件': 'bigFiles.source.image',
  };
  const key = sourceKey[sourceLabel];
  return key ? translate(key) : '';
}

// ============================================================================
// Component implementation
// ============================================================================

export function BigFilesModule({ isPageActive = true }: ModuleRenderProps) {
  const { t: navT } = useTranslation('nav');
  const { t } = useTranslation('common');
  const { moduleState, updateModuleState, triggerHealthRefresh, oneClickScanTrigger } = useModuleDashboard('bigFiles');
  const { showToast } = useToast();
  const { settings } = useSettings();
  const { drives } = useLocalDrives();

  // Prevent duplicate scans
  const scanningRef = useRef(false);
  // Scan start time
  const scanStartRef = useRef(0);
  // Tracks whether the current one-click scan trigger has been handled
  const lastScanTriggerRef = useRef(0);

  // Local state
  const [files, setFiles] = useState<LargeFileEntry[]>([]);
  // Hook up the AI assistant (read-only): provide a summary only, no action tools.
  useAiModuleBridge('big-files', {
    // Read-only: single-item details. itemId is the full path at the start of the read_items line.
    readItemDetail: (itemId) => {
      const wanted = itemId.trim();
      const file = files.find((item) => item.path === wanted || wanted.startsWith(item.path));
      if (!file) return null;
      return [
        `路径：${file.path}`,
        `大小：${formatSize(file.size)}`,
        `来源：${file.source_label || '未知来源'}`,
        `风险等级：${file.risk_level}`,
      ].join('\n');
    },
    // Read-only: lets the assistant "look further" at what entries exist (one page at a time; the model decides whether to continue).
    readItems: (_targetId, offset, limit) => {
      if (files.length === 0) return null;
      return {
        total: files.length,
        lines: files.slice(offset, offset + limit).map(
          (file) => `${file.path}  ${formatSize(file.size)}  （${file.source_label || '未知来源'}）`,
        ),
      };
    },
    summary: () => {
      if (files.length === 0) return null;
      const totalSize = files.reduce((sum, file) => sum + file.size, 0);
      const topThree = [...files]
        .sort((a, b) => b.size - a.size)
        .slice(0, 3)
        .map((file) => `${file.source_label || '未知来源'} ${formatSize(file.size)}`)
        .join('、');
      return [
        `共 ${files.length} 个大文件，合计 ${formatSize(totalSize)}；`,
        `当前已勾选 ${selectedFiles.size} 个。`,
        `最大的三个：${topThree}。`,
      ].join('');
    },
  });

  const [selectedFiles, setSelectedFiles] = useState<Set<string>>(new Set());
  const [currentPath, setCurrentPath] = useState('');
  const [scanBackend, setScanBackend] = useState(''); // "mft" | "walkdir"

  // Copying a path suits a cleanup list better than opening the file: users can first confirm the file's purpose in another tool, then decide whether to delete.
  const copyFilePath = useCallback(async (path: string) => {
    try {
      await navigator.clipboard.writeText(path);
      showToast({ type: 'success', title: t('copyFilePathSuccess') });
    } catch (error) {
      showToast({ type: 'error', title: t('copyFilePathFailed'), description: String(error) });
    }
  }, [showToast, t]);
  const [scanStage, setScanStage] = useState('');
  const [scanDetail, setScanDetail] = useState('');
  const [backendElapsedMs, setBackendElapsedMs] = useState(0);
  const [scannedCount, setScannedCount] = useState(0);
  const [scanElapsed, setScanElapsed] = useState(0);
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);
  const [selectedDriveLetter, setSelectedDriveLetter] = useState('C:');
  const selectedDriveLabel = driveDisplayName(selectedDriveLetter);

  useEffect(() => {
    if (drives.length > 0) {
      setSelectedDriveLetter((current) => {
        const normalized = normalizeDriveLetter(current);
        return drives.some((drive) => drive.drive_letter === normalized)
          ? normalized
          : defaultDriveLetter(drives);
      });
    }
  }, [drives]);

  const resetBigFilesResult = useCallback(() => {
    // After switching disks, old results no longer match the current target disk; clear them to prevent deleting files on another disk by mistake.
    setFiles([]);
    setSelectedFiles(new Set());
    setCurrentPath('');
    setScanBackend('');
    setScanStage('');
    setBackendElapsedMs(0);
    setScannedCount(0);
    updateModuleState('bigFiles', { status: 'idle', error: null, fileCount: 0, totalSize: 0, progress: 0 });
  }, [updateModuleState]);

  const handleDriveChange = useCallback((driveLetter: string) => {
    if (scanningRef.current) return;
    setSelectedDriveLetter(normalizeDriveLetter(driveLetter));
    resetBigFilesResult();
  }, [resetBigFilesResult]);

  // Listen for scan progress events
  useEffect(() => {
    let unlisten: (() => void) | null = null;

    const setupListener = async () => {
      unlisten = await listen<LargeFileScanProgress>('large-file-scan:progress', (event) => {
        const { current_path, scanned_count, backend, stage, elapsed_ms, detail } = event.payload;
        setCurrentPath(current_path);
        setScannedCount(scanned_count);
        setScanStage(stage || '');
        setBackendElapsedMs(elapsed_ms || 0);
        setScanDetail(detail || '');
        if (backend) {
          setScanBackend(backend);
        }
      });
    };

    setupListener();

    return () => {
      if (unlisten) {
        unlisten();
      }
    };
  }, []);

  // Scan timer
  useEffect(() => {
    if (moduleState.status !== 'scanning') { setScanElapsed(0); return; }
    const interval = setInterval(() => {
      if (scanStartRef.current > 0) {
        setScanElapsed(Math.floor((performance.now() - scanStartRef.current) / 1000));
      }
    }, 200);
    return () => clearInterval(interval);
  }, [moduleState.status]);

  // Start scan (with debounce — scanningRef prevents duplicate triggers)
  const handleScan = useCallback(async () => {
    if (scanningRef.current) return;
    scanningRef.current = true;

    updateModuleState('bigFiles', { status: 'scanning', error: null });
    setFiles([]);
    setCurrentPath('');
    setScanBackend('');
    setScanStage('');
    setBackendElapsedMs(0);
    setScannedCount(0);
    setScanElapsed(0);
    scanStartRef.current = performance.now();
    setSelectedFiles(new Set());

    try {
      const results = await scanLargeFiles(settings.bigFilesScanLimit, selectedDriveLetter);
      setFiles(results);

      const totalSize = results.reduce((sum, f) => sum + f.size, 0);
      updateModuleState('bigFiles', {
        status: 'done',
        fileCount: results.length,
        totalSize,
      });
    } catch (err) {
      console.error('扫描大文件失败:', err);
      updateModuleState('bigFiles', { status: 'error', error: String(err) });
    } finally {
      scanningRef.current = false;
    }
  }, [updateModuleState, settings.bigFilesScanLimit, selectedDriveLetter]);

  // Listen for the one-click scan trigger
  useEffect(() => {
    if (oneClickScanTrigger > 0 && oneClickScanTrigger !== lastScanTriggerRef.current) {
      lastScanTriggerRef.current = oneClickScanTrigger;
      handleScan();
    }
  }, [oneClickScanTrigger, handleScan]);

  // Stop scan
  const handleStopScan = useCallback(async () => {
    try {
      await cancelLargeFileScan();
      showToast({ type: 'info', title: t('bigFiles.scanStopped'), description: t('bigFiles.scanStoppedDesc') });
    } catch (err) {
      console.error('停止扫描失败:', err);
    }
  }, [showToast, t]);

  // Toggle file selection (backend risk level >= 4 is locked and unselectable)
  const handleSearchFile = useCallback(async (path: string) => {
    try {
      // Include the full path when searching, to help users confirm the file's origin and risk before deleting.
      await openSearchUrl(t('bigFiles.searchQuery', { path }));
    } catch (err) {
      console.error('搜索文件用途失败:', err);
      showToast({
        type: 'error',
        title: t('openSearchFailed'),
        description: String(err),
      });
    }
  }, [showToast, t]);

  const toggleFileSelection = useCallback((path: string, riskLevel: number) => {
    if (riskLevel >= 4) return;

    setSelectedFiles((prev) => {
      const next = new Set(prev);
      if (next.has(path)) {
        next.delete(path);
      } else {
        next.add(path);
      }
      return next;
    });
  }, []);

  // Select all / deselect all (backend risk level >= 4 is locked and unselectable)
  const toggleSelectAll = useCallback(() => {
    const selectable = files.filter((f) => f.risk_level <= 3);
    if (selectedFiles.size === selectable.length) {
      setSelectedFiles(new Set());
    } else {
      setSelectedFiles(new Set(selectable.map((f) => f.path)));
    }
  }, [selectedFiles.size, files]);

  // Perform deletion
  const handleDelete = useCallback(async () => {
    const paths = Array.from(selectedFiles);
    if (paths.length === 0) return;

    setIsDeleting(true);

    try {
      const result = await deleteFiles(paths);

      // Record cleanup log (all operations are recorded)
      const failedPathSet = new Set(result.failed_files?.map((f) => f.path) || []);
      const logEntries: CleanupLogEntryInput[] = paths.map((path) => {
        const file = files.find((f) => f.path === path);
        const failedFile = result.failed_files?.find((f) => f.path === path);
        return {
          category: '大文件清理',
          path,
          size: file?.size || 0,
          success: !failedPathSet.has(path),
          error_message: failedFile?.reason,
        };
      });
      recordCleanupAction(logEntries).catch((err) => {
        console.warn('记录清理日志失败:', err);
      });

      if (result.failed_count === 0) {
        showToast({
          type: 'success',
          title: t('bigFiles.deleteSuccess', { count: result.success_count }),
          description: t('bigFiles.deleteSuccessDesc', { size: formatSize(result.freed_size) }),
        });
      } else if (result.success_count === 0) {
        showToast({
          type: 'error',
          title: t('bigFiles.deleteFailed'),
          description: t('bigFiles.deleteFailedDesc', { count: result.failed_count }),
        });
      } else {
        showToast({
          type: 'warning',
          title: t('bigFiles.deletePartial'),
          description: t('bigFiles.deletePartialDesc', { success: result.success_count, failed: result.failed_count }),
        });
      }

      // Remove successfully deleted files from the list; rebuild state based on the returned result
      if (result.success_count > 0) {
        const failedPaths = new Set(result.failed_files?.map((f) => f.path) ?? []);

        // Remove the successfully deleted ones from the file list (selected and not in the failure list)
        const newFiles = files.filter(
          (file) => !selectedFiles.has(file.path) || failedPaths.has(file.path)
        );
        setFiles(newFiles);

        // Keep only actually failed files in the selection state
        setSelectedFiles(
          new Set([...failedPaths].filter((p) => selectedFiles.has(p)))
        );

        const newTotalSize = newFiles.reduce((sum, f) => sum + f.size, 0);
        updateModuleState('bigFiles', {
          fileCount: newFiles.length,
          totalSize: newTotalSize,
        });

        triggerHealthRefresh();
      }
    } catch (err) {
      console.error('删除大文件失败:', err);
      showToast({
        type: 'error',
        title: t('bigFiles.deleteFailed'),
        description: String(err),
      });
    } finally {
      setIsDeleting(false);
    }
  }, [selectedFiles, files, updateModuleState, triggerHealthRefresh, showToast, t]);

  // Calculate the total size of selected files
  const selectedSize = files
    .filter((f) => selectedFiles.has(f.path))
    .reduce((sum, f) => sum + f.size, 0);

  // Number of selectable files (risk_level >= 4 is locked and unselectable)
  const selectableCount = files.filter((f) => f.risk_level <= 3).length;
  // Visibility is controlled by the current module, ensuring the operation area follows its corresponding result.
  const shouldShowOperationToolbar = isPageActive;
  const isScanning = moduleState.status === 'scanning';
  const displayElapsedSeconds = isScanning
    ? scanElapsed
    : Math.round(backendElapsedMs / 1000);
  // All backend progress branches now provide a stable stage; unknown stages fall back to a localized hint, avoiding display of legacy Chinese messages.
  const localizedScanStage = scanStage
    ? t(`bigFilesStages.${scanStage}`, { defaultValue: t('bigFiles.detecting') })
    : t('bigFiles.detecting');
  const displayScanPath = /^[A-Za-z]:[\\/]/.test(currentPath) ? currentPath : '';
  const driveSelector = (
    <div className="flex items-center gap-2 shrink-0" onClick={(event) => event.stopPropagation()}>
      <DriveSelect
        value={selectedDriveLetter}
        drives={drives}
        onChange={handleDriveChange}
        disabled={isScanning}
      />
    </div>
  );

  if (!isPageActive && !isDeleting && !showDeleteConfirm) {
    return null;
  }

  return (
    <>
      {/* Deletion confirmation modal */}
      <ConfirmDialog
        isOpen={showDeleteConfirm}
        title={t('confirmDeleteLargeFiles')}
        description={t('bigFiles.confirmDesc', { count: selectedFiles.size.toLocaleString(), size: formatSize(selectedSize) })}
        warning={t('largeFileDeleteWarning')}
        confirmText={t('confirmDelete')}
        cancelText={t('cancel')}
        onConfirm={() => {
          setShowDeleteConfirm(false);
          handleDelete();
        }}
        onCancel={() => setShowDeleteConfirm(false)}
        isDanger
      />

      <ModuleCard
        id="bigFiles"
        title={navT('bigFiles')}
        description={`${navT('bigFilesDesc')} (${selectedDriveLabel})`}
        icon={<FileBox className="w-6 h-6 text-[var(--brand-green)]" />}
        status={moduleState.status}
        fileCount={moduleState.fileCount}
        totalSize={moduleState.totalSize}
        onScan={handleScan}
        error={moduleState.error}
        titleExtra={driveSelector}
        headerExtra={
          <>
            {isScanning && (
              <button
                onClick={handleStopScan}
                className="flex items-center gap-1.5 px-3 py-1.5 bg-amber-500/10 hover:bg-amber-500/20 rounded-lg text-xs font-medium text-amber-600 transition"
              >
                <StopCircle className="w-3.5 h-3.5" />
                {t('stop')}
              </button>
            )}
          </>
        }
        allowStickyContent
      >
            {/* Deletion progress is now attached to the floating toolbar (see busyContent above):
                it's a position: fixed layer outside the document flow, so the list is neither stretched nor shifted —
                big files need to preserve the reference of "which ones I am deleting". */}
            {/* Expanded content */}
            {/* pb-20: the bottom toolbar is a fixed floating layer; bottom padding keeps the last file row from being covered */}
            <div className="pb-20">
              {shouldShowOperationToolbar && files.length > 0 && !isScanning && (
            // The shared toolbar handles bottom positioning; button content is still maintained by the big-files module.
            <ModuleOperationToolbar
              moduleId="bigFiles"
              busy={isDeleting}
              busyContent={
                <ModuleDeleteProgress
                  variant="floating"
                  phaseLabel={t('bigFiles.deleting')}
                  progressLabel={t('bigFiles.deletingDesc', { count: selectedFiles.size.toLocaleString() })}
                />
              }
            >
              <button
                onClick={toggleSelectAll}
                className="module-operation-toolbar__button module-operation-toolbar__button--muted"
              >
                {selectedFiles.size === selectableCount && selectableCount > 0 ? t('deselectAll') : t('selectAll')}
              </button>
              <button
                onClick={() => setShowDeleteConfirm(true)}
                disabled={selectedFiles.size === 0}
                className="module-operation-toolbar__button module-operation-toolbar__button--danger"
              >
                <Trash2 className="w-3.5 h-3.5" />
                {t('cleanSelected', { count: selectedFiles.size })}
              </button>
            </ModuleOperationToolbar>
          )}

          {/* Non-admin falls back to normal traversal (tens of seconds vs a few). Say only "can be accelerated" — scanning still completes without elevation. */}
          <AdminRequiredHint className="mx-4 mt-3" variant="performance" />

          {/* Scan progress + engine + duration (shown both during and after scanning) */}
          {(isScanning || scanBackend) && (displayScanPath || localizedScanStage) && (
            <div className={`px-4 py-2 border-b border-[var(--border-default)] text-xs flex items-center gap-2 ${
              scanBackend === 'mft' ? 'bg-[var(--brand-green-10)]' : 'bg-emerald-500/5'
            }`}>
              {/* This left part needs its own wrapper: truncate has overflow-hidden, which would clip the popover */}
              <div className="truncate flex items-center gap-3 min-w-0 flex-1">
                <span className="truncate text-[var(--fg-muted)]">{isScanning ? t('bigFiles.scanningPrefix') : t('bigFiles.scanCompletedPrefix')} {localizedScanStage}{displayScanPath ? ` · ${displayScanPath}` : ''}</span>
                {scanBackend && (
                  <span className={`shrink-0 text-[0.71rem] px-1.5 py-0.5 rounded font-medium ${
                    scanBackend === 'mft'
                      ? 'bg-[var(--brand-green)] text-white'
                      : 'bg-[var(--bg-hover)] text-[var(--text-muted)]'
                  }`}>
                    {scanBackend === 'mft' ? t('bigFiles.mftFullScan') : t('bigFiles.normalScan')}
                  </span>
                )}
                <span className="shrink-0 text-[var(--fg-faint)]">{t('fileCount', { count: scannedCount.toLocaleString() })}</span>
                {scanStage && scanBackend === 'mft' && (
                  <span className="shrink-0 text-[var(--fg-faint)]">{localizedScanStage}</span>
                )}
                {displayElapsedSeconds > 0 && (
                  <span className="shrink-0 text-[var(--fg-faint)]">{displayElapsedSeconds}s</span>
                )}
              </div>
              {/* Key phase numbers (raw from backend): collapsed by default, click the stopwatch to view — after release only an icon remains here */}
              <ScanTimingPopover
                groups={scanDetail ? [{ title: '', rows: [], note: scanDetail }] : []}
              />
            </div>
          )}

          {/* Empty state */}
          {moduleState.status === 'idle' && files.length === 0 && (
            <div className="p-4">
              <EmptyState
                icon={FileBox}
                  title={t('notScannedLargeFiles')}
                description={t('bigFiles.emptyDesc')}
              />
            </div>
          )}

          {/* Scanning state: uses the shared component.
              engine name (MFT / normal traversal), current phase, and the path being scanned are implementation details,
              not needed when viewing results; phase timings are available via ScanTimingPopover.
              keep only "what is being scanned" and "roughly what the result looks like" here. */}
          {isScanning && files.length === 0 && (
            <div className="p-4">
              <ModuleScanProgress
                title={
                  scanBackend === 'mft'
                    ? t('bigFiles.mftScanning', { drive: selectedDriveLabel })
                    : scanBackend === 'walkdir'
                      ? t('bigFiles.walkdirScanning', { drive: selectedDriveLabel })
                      : t('scanningShort')
                }
                description={t('bigFiles.scanningDesc')}
              />
            </div>
          )}

          {/* File list */}
          {files.length > 0 && (
            <div className="divide-y divide-[var(--border-default)]">
              {files.map((file, index) => {
                const riskLevel = file.risk_level;
                const isSelected = selectedFiles.has(file.path);
                const isLocked = riskLevel >= 4; // backend risk level >= 4, locked and undeletable
                const localizedSourceLabel = getLocalizedSourceLabel(file.source_label, t);

                return (
                  <div
                    key={file.path}
                    onClick={() => toggleFileSelection(file.path, riskLevel)}
                    className={`
                      px-4 py-3 flex items-center gap-3 cursor-pointer transition-all
                      ${isLocked ? 'bg-rose-500/5 cursor-not-allowed' :
                        isSelected ? 'bg-[var(--brand-green-10)] hover:bg-[var(--brand-green-10)]' : 'hover:bg-[var(--bg-hover)]'}
                    `}
                  >
                    {/* Row number + checkbox */}
                    <div className="flex items-center gap-2 shrink-0">
                      <span className="w-5 text-center text-xs font-medium text-[var(--fg-faint)]">
                        {index + 1}
                      </span>
                      <div className={`
                        w-5 h-5 rounded border-2 flex items-center justify-center
                        ${isLocked
                          ? 'soft-danger'
                          : isSelected
                            ? 'bg-[var(--brand-green)] border-[var(--brand-green)] cursor-pointer'
                            : 'border-[var(--text-faint)] cursor-pointer'
                        }
                      `}>
                        {isLocked ? (
                          <svg className="w-3 h-3 text-rose-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={3}>
                            <path strokeLinecap="round" strokeLinejoin="round" d="M6 18L18 6M6 6l12 12" />
                          </svg>
                        ) : isSelected ? (
                          <svg className="w-3 h-3 text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={3}>
                            <path strokeLinecap="round" strokeLinejoin="round" d="M5 13l4 4L19 7" />
                          </svg>
                        ) : null}
                      </div>
                    </div>

                    {/* File icon */}
                    <div className={`w-9 h-9 rounded-lg flex items-center justify-center shrink-0 ${getRiskLevelBgColor(riskLevel)}`}>
                      <FileWarning className={`w-4 h-4 ${getRiskLevelColor(riskLevel)}`} />
                    </div>

                    {/* File info */}
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <p className="text-sm text-[var(--fg-primary)] truncate font-medium" title={file.path}>
                          {file.path.split('\\').pop() || file.path}
                        </p>
                        {localizedSourceLabel && (
                          <span className="shrink-0 px-1.5 py-0.5 rounded text-[0.64rem] font-medium bg-[var(--bg-hover)] text-[var(--fg-muted)]">
                            {localizedSourceLabel}
                          </span>
                        )}
                      </div>
                      <p className="text-xs text-[var(--fg-muted)] truncate mt-0.5" title={file.path}>
                        {file.path}
                      </p>
                    </div>

                    {/* Right-side info */}
                    <div className="text-right shrink-0">
                      <p className="text-sm font-bold text-emerald-600">{formatSize(file.size)}</p>
                      <div className="flex items-center justify-end gap-2 mt-0.5">
                        <span className="text-[0.71rem] text-[var(--fg-muted)]">{formatDate(file.modified)}</span>
                        <span className={`px-1.5 py-0.5 rounded-full text-[0.64rem] font-semibold ${getRiskLevelColor(riskLevel)} ${getRiskLevelBgColor(riskLevel)}`}>
                          {isLocked ? '🔒 ' : ''}{t(getRiskLevelKey(riskLevel))}
                        </span>
                      </div>
                    </div>

                    {/* Action buttons */}
                    <div className="flex items-center gap-0.5 shrink-0">
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          handleSearchFile(file.path);
                        }}
                        className="p-1.5 hover:bg-[var(--bg-hover)] rounded-lg transition text-[var(--fg-muted)] hover:text-emerald-600"
                  title={t('searchDeletionAdvice')}
                      >
                        <Search className="w-4 h-4" />
                      </button>
                      {/* Quick question: asks "what is this file and can it be deleted",
                          so use a complete question rather than search-engine keywords (see the aiQuestions note). */}
                      <AiAskButton
                        question={buildItemQuestion(file.path, 'file')}
                        className="inline-flex h-7 w-7 items-center justify-center"
                      />
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          openInFolder(file.path);
                        }}
                        className="p-1.5 hover:bg-[var(--bg-hover)] rounded-lg transition text-[var(--fg-muted)] hover:text-emerald-600"
                  title={t('openInFolder')}
                      >
                        <FolderOpen className="w-4 h-4" />
                      </button>
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          void copyFilePath(file.path);
                        }}
                        className="p-1.5 hover:bg-[var(--bg-hover)] rounded-lg transition text-[var(--fg-muted)] hover:text-emerald-600"
                  title={t('copyFilePath')}
                      >
                        <Copy className="w-4 h-4" />
                      </button>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </ModuleCard>
    </>
  );
}

export default BigFilesModule;
