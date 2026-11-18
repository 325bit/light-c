import { ScanTimingPopover, type TimingGroup } from '../ScanTimingPopover';
import { AdminRequiredHint, useAdminPrivilege } from '../AdminRequiredHint';
import { useAiModuleBridge } from '../ai/useAiModuleBridge';
// ============================================================================
// Multi-drive full-disk change analysis module
//
// Full-disk change analysis only locates the sources of space increase/decrease and provides no deletion capability, to avoid mistaking "changed directories" for "cleanable directories".
// ============================================================================

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import i18n from '../../i18n';
import { AnimatePresence } from 'framer-motion';
import { AlertTriangle, CheckCircle2, Download, HardDrive } from 'lucide-react';
import { ModuleScanProgress } from '../ModuleScanProgress';
import { StopScanButton } from '../ui/StopScanButton';
import { ModuleCard } from '../ModuleCard';
import { EmptyState } from '../EmptyState';
import {
  defaultDriveLetter,
  DriveSelect,
  driveDisplayName,
  driveOptionTitle,
  normalizeDriveLetter,
  useLocalDrives,
} from '../ui/DriveSelect';
import { useModuleDashboard } from '../../contexts/DashboardContext';
import { useSettings } from '../../contexts';
import { openSearchUrl } from '../../utils/searchEngine';
import {
  cancelDiskGrowthScan,
  getDiskGrowthExportTree,
  openInFolder,
  scanDiskGrowth,
  saveDiskGrowthHtml,
  type DiskGrowthEntry,
  type DiskGrowthReport,
  type DiskGrowthScanResponse,
} from '../../api/commands';
import { formatSize } from '../../utils/format';
import { useToast } from '../Toast';
import { buildDiskGrowthHtml } from '../../utils/diskGrowthHtml';
import { formatDiff, normalizeDiskPath } from '../../utils/growthFormat';
import { GrowthDetailModal } from './GrowthDetailModal';
import { GrowthResultList } from './GrowthResultList';
import type { ModuleRenderProps } from './moduleProps';

function getPhaseLabel(stage: string): string {
  return i18n.t(`scanStages.${stage}`, {
    ns: 'common',
    defaultValue: i18n.t('scanStages.scanning', { ns: 'common' }),
  });
}

function formatDuration(ms?: number): string {
  if (ms === undefined || ms < 0) return '-';
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

function formatPreviousScanTime(scanSummary: DiskGrowthScanResponse): string {
  return scanSummary.previous_scan_time || i18n.t('diskGrowth.noHistory', { ns: 'modules' });
}

/**
 * Select the top-level changed directories needed for the export report.
 *
 * Note: the backend's changed-directory list is flat, and a parent may appear after its child, so one cannot simply
 * "look backwards one by one"; here we sort by path first, then compare only with the most recently accepted root, reducing O(n²)
 * pairwise comparisons to one sort plus one linear scan. Return the original paths; return an empty array when no entry is found.
 */
function selectTopLevelGrowthPaths(entries: DiskGrowthEntry[]): string[] {
  const normalizedEntries = entries
    .map((entry) => ({ path: entry.path, normalized: normalizeDiskPath(entry.path) }))
    .sort((left, right) => left.normalized.localeCompare(right.normalized));

  const topLevelPaths: string[] = [];
  let lastAcceptedPrefix = '';
  for (const entry of normalizedEntries) {
    if (lastAcceptedPrefix && entry.normalized.startsWith(`${lastAcceptedPrefix}/`)) {
      continue;
    }
    lastAcceptedPrefix = entry.normalized;
    topLevelPaths.push(entry.path);
  }

  return topLevelPaths;
}

function SummaryCards({
  scanSummary,
  growthReport,
  driveLabel,
}: {
  scanSummary: DiskGrowthScanResponse;
  growthReport: DiskGrowthReport;
  driveLabel: string;
}) {
  const { t: moduleT } = useTranslation('modules');
  const totalGrowth = growthReport.total_growth;
  const indexedSizeText = formatSize(scanSummary.total_size);
  const totalGrowthText = totalGrowth === 0 ? moduleT('diskGrowth.noChange') : formatDiff(totalGrowth);
  const previousScanText = formatPreviousScanTime(scanSummary);
  const scannedFileCountText = scanSummary.total_files_scanned.toLocaleString();

  return (
    <div className="grid grid-cols-4 gap-3">
      <div className="glass-block min-w-0 bg-[var(--bg-main)] rounded-xl px-3 py-3">
        <p className="text-[0.79rem] text-[var(--text-muted)] mb-1 truncate" title={`${driveLabel} ${moduleT('diskGrowth.indexed')}`}>
          {driveLabel} {moduleT('diskGrowth.indexed')}
        </p>
        <p className="text-base font-bold text-[var(--text-primary)] tabular-nums truncate" title={indexedSizeText}>
          {indexedSizeText}
        </p>
      </div>
      <div className="glass-block min-w-0 bg-[var(--bg-main)] rounded-xl px-3 py-3">
        <p className="text-[0.79rem] text-[var(--text-muted)] mb-1 truncate" title={moduleT('diskGrowth.netChange')}>{moduleT('diskGrowth.netChange')}</p>
        <p
          className={`text-base font-bold tabular-nums truncate ${
            totalGrowth > 0
              ? 'text-red-500'
              : totalGrowth < 0
                ? 'text-green-500'
                : 'text-[var(--text-muted)]'
          }`}
          title={totalGrowthText}
        >
          {totalGrowthText}
        </p>
      </div>
      <div className="glass-block min-w-0 bg-[var(--bg-main)] rounded-xl px-3 py-3">
        <p className="text-[0.79rem] text-[var(--text-muted)] mb-1 truncate" title={moduleT('diskGrowth.previousScan')}>{moduleT('diskGrowth.previousScan')}</p>
        <p className="text-[0.93rem] font-semibold text-[var(--text-primary)] tabular-nums truncate" title={previousScanText}>
          {previousScanText}
        </p>
      </div>
      <div className="glass-block min-w-0 bg-[var(--bg-main)] rounded-xl px-3 py-3">
        <p className="text-[0.79rem] text-[var(--text-muted)] mb-1 truncate" title={moduleT('diskGrowth.fileCount')}>{moduleT('diskGrowth.fileCount')}</p>
        <p className="text-base font-bold text-[var(--brand-green)] tabular-nums truncate" title={scannedFileCountText}>
          {scannedFileCountText}
        </p>
      </div>
      {growthReport.entries.length > 0 && (
        <div className="col-span-4 flex items-center gap-3 text-[0.86rem] text-[var(--text-muted)] min-w-0 overflow-hidden whitespace-nowrap">
          <span className="text-red-500">{moduleT('diskGrowth.increased', { size: formatSize(scanSummary.analyze.increased_size ?? 0) })}</span>
          <span className="text-green-500">{moduleT('diskGrowth.decreased', { size: formatSize(scanSummary.analyze.decreased_size ?? 0) })}</span>
        </div>
      )}
    </div>
  );
}

function DiagnosticBanner({ report }: { report: DiskGrowthReport }) {
  const hasGrowth = report.significant_count > 0 || report.fast_count > 0;

  return (
    <div
      className={`flex items-start gap-3 px-4 py-3 rounded-xl text-[0.93rem] ${
        hasGrowth
          ? 'bg-amber-500/10 text-amber-700 dark:text-amber-400'
          : 'bg-[var(--brand-green-10)] text-[var(--brand-green)]'
      }`}
    >
      {hasGrowth ? (
        <AlertTriangle className="w-4 h-4 mt-0.5 shrink-0" />
      ) : (
        <CheckCircle2 className="w-4 h-4 mt-0.5 shrink-0" />
      )}
      <span>{report.summary}</span>
    </div>
  );
}

function DiskGrowthDiagnostics({
  scanSummary,
  resultMode,
  maxEntries,
}: {
  scanSummary: DiskGrowthScanResponse;
  resultMode: 'change' | 'usage';
  maxEntries: number;
}) {
  const { t: moduleT } = useTranslation('modules');
  // Phase durations, $MFT fragmentation, and size sources are troubleshooting numbers: put them in the popover, leaving only a small stopwatch on the page
  const phaseRows = scanSummary.phase_durations.map((phase) => ({
    label: getPhaseLabel(phase.stage),
    value: formatDuration(phase.duration_ms),
  }));
  const detailNotes = [
    moduleT('diskGrowth.engine', { name: scanSummary.backend === 'mft' ? 'MFT' : scanSummary.backend }),
    typeof scanSummary.mft_fragments === 'number' && scanSummary.mft_fragments > 0
      ? moduleT('diskGrowth.mftStats', {
          fragments: scanSummary.mft_fragments,
          io: ((scanSummary.mft_io_ms ?? 0) / 1000).toFixed(1),
          parse: ((scanSummary.mft_parse_ms ?? 0) / 1000).toFixed(1),
        })
      : '',
    moduleT('diskGrowth.sizeSource', {
      mft: scanSummary.mft_size_count.toLocaleString(),
      fallback: scanSummary.metadata_fallback_count.toLocaleString(),
    }),
  ].filter(Boolean);
  const timingGroups: TimingGroup[] = phaseRows.length > 0 || detailNotes.length > 0
    ? [{ title: '', rows: phaseRows, note: detailNotes.join(' · ') }]
    : [];
  // Reuse the same diagnostic layout as big-file analysis so users see consistent phase-timing info across MFT modules.
  const hasPhaseDurations = scanSummary.phase_durations.length > 0;
  const latestPhase = hasPhaseDurations
    ? scanSummary.phase_durations[scanSummary.phase_durations.length - 1]
    : null;

  return (
    <div className="glass-block rounded-xl bg-[var(--bg-main)] border border-[var(--border-color)] px-4 py-3 text-xs">
      <div className="grid grid-cols-1 md:grid-cols-[1fr_auto] gap-2 md:gap-4">
        <div className="flex items-center gap-2 min-w-0">
          <span className="shrink-0 px-2 py-0.5 rounded-md bg-[var(--brand-green)] text-white font-medium">
            {latestPhase ? getPhaseLabel(latestPhase.stage) : moduleT('diskGrowth.completed')}
          </span>
          <span className="truncate text-[var(--text-primary)]">
            {resultMode === 'change'
              ? moduleT('diskGrowthExtra.changeSummary', { count: maxEntries })
              : moduleT('diskGrowthExtra.baselineSummary')}
          </span>
        </div>
        <div className="flex items-center justify-start md:justify-end gap-4 text-[var(--text-muted)] tabular-nums">
          <span>{moduleT('diskGrowth.processed', { count: scanSummary.total_files_scanned.toLocaleString() })}</span>
          <span>{moduleT('diskGrowth.elapsed', { time: formatDuration(scanSummary.scan_duration_ms) })}</span>
          <ScanTimingPopover groups={timingGroups} />
        </div>
      </div>

    </div>
  );
}

export function DiskGrowthModule({ isPageActive = true }: ModuleRenderProps) {
  const { t: navT } = useTranslation('nav');
  const { t: moduleT } = useTranslation('modules');
  const { moduleState, updateModuleState, oneClickScanTrigger, stopScanTrigger } = useModuleDashboard('diskGrowth');
  const { settings } = useSettings();
  const { showToast } = useToast();
  const lastScanTriggerRef = useRef(0);
  const scanningRef = useRef(false);
  const cancelRequestedRef = useRef(false);
  const scanRunIdRef = useRef(0);

  const [scanSummary, setScanSummary] = useState<DiskGrowthScanResponse | null>(null);
  // Integrate with the AI assistant (**read-only**: disk change analysis only analyzes, no delete action).
  useAiModuleBridge('disk-growth', {
    summary: () => {
      if (!scanSummary) return null;
      return [
        `盘 ${scanSummary.drive_letter}（${scanSummary.root_path}）共 ${scanSummary.total_files_scanned} 个文件，`,
        `占用 ${formatSize(scanSummary.total_size)}，本次扫描耗时约 ${Math.round(scanSummary.scan_duration_ms / 1000)} 秒。`,
        scanSummary.previous_scan_time
          ? `已有一份历史快照（${scanSummary.previous_scan_time}），可以对比出这段时间里哪些目录长大了。`
          : '这是首次快照，还没有可对比的历史数据；下次扫描之后就能看出增减来自哪里。',
      ].join('');
    },
    // Read-only: the changed-directory list (the column in the UI). Give **all** entries with their change amounts -
    // the UI shows only the significant ones by default, but the user may ask about any of them.
    readItems: (_targetId, offset, limit) => {
      const list = growthReport?.entries ?? [];
      if (list.length === 0) return null;
      return {
        total: list.length,
        lines: list.slice(offset, offset + limit).map((item) => {
          const sign = item.diff >= 0 ? '+' : '-';
          const percent = `${item.diff_percent >= 0 ? '+' : ''}${item.diff_percent.toFixed(1)}%`;
          return `${item.path}  ${formatSize(item.new_size)}  （变化 ${sign}${formatSize(Math.abs(item.diff))}，${percent}）`;
        }),
      };
    },
    // Read-only: single-item detail. itemId is the directory path at the start of the line.
    // Report only this entry's own facts - which specific file drove the growth belongs to the UI drill-down; do not fabricate it here.
    readItemDetail: (itemId) => {
      const wanted = itemId.trim();
      if (!wanted) return null;
      const item = (growthReport?.entries ?? []).find((entry) => entry.path.trim() === wanted);
      if (!item) return null;
      const modified =
        item.modified > 0 ? new Date(item.modified * 1000).toLocaleString() : '（快照里没有记录）';
      return [
        `目录：${item.path}`,
        `本次快照：${formatSize(item.new_size)}`,
        `上次快照：${item.old_size > 0 ? formatSize(item.old_size) : '（上次快照里没有，属于新出现）'}`,
        `变化量：${item.diff >= 0 ? '+' : '-'}${formatSize(Math.abs(item.diff))}`
          + `（${item.diff_percent >= 0 ? '+' : ''}${item.diff_percent.toFixed(1)}%）`,
        `子树内最新文件修改时间：${modified}`,
        '说明：这里只有目录级的增减；具体是哪些文件造成的，要看界面上的下钻结果。',
      ].join('\n');
    },
  });

  const [growthReport, setGrowthReport] = useState<DiskGrowthReport | null>(null);
  // Admin status uses the shared hook: AdminRequiredHint has already probed once, reuse that cache here
  const isAdmin = useAdminPrivilege();
  const [error, setError] = useState<string | null>(null);
  const [detailEntry, setDetailEntry] = useState<DiskGrowthEntry | null>(null);
  const { drives, error: drivesError } = useLocalDrives();
  const [selectedDriveLetter, setSelectedDriveLetter] = useState('C:');
  const selectedDrive = drives.find((drive) => drive.drive_letter === selectedDriveLetter) ?? null;
  const selectedDriveLabel = driveDisplayName(selectedDriveLetter);

  const resetCurrentDriveResult = useCallback(() => {
    setScanSummary(null);
    setGrowthReport(null);
    setDetailEntry(null);
    setError(null);
    updateModuleState('diskGrowth', { status: 'idle', error: null, fileCount: 0, totalSize: 0, progress: 0 });
  }, [updateModuleState]);

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

  const handleScan = useCallback(async () => {
    if (scanningRef.current) return;
    if (isAdmin === false) {
      const message = moduleT('diskGrowth.adminScanError', { drive: selectedDriveLabel });
      setError(message);
      updateModuleState('diskGrowth', { status: 'error', error: message });
      return;
    }
    if (selectedDrive && !selectedDrive.is_ntfs) {
      const message = moduleT('diskGrowth.ntfsScanError', { drive: selectedDriveLabel, fileSystem: selectedDrive.file_system || moduleT('diskGrowth.unknownFileSystem') });
      setError(message);
      updateModuleState('diskGrowth', { status: 'error', error: message });
      return;
    }
    scanningRef.current = true;
    cancelRequestedRef.current = false;
    const scanRunId = ++scanRunIdRef.current;

    updateModuleState('diskGrowth', { status: 'scanning', error: null, fileCount: 0, totalSize: 0 });
    setError(null);
    setScanSummary(null);
    setGrowthReport(null);
    setDetailEntry(null);

    try {
      const result = await scanDiskGrowth(settings.diskGrowthMaxEntries, selectedDriveLetter);
      if (cancelRequestedRef.current || scanRunId !== scanRunIdRef.current) {
        // After user cancellation, do not accept old results that may already have returned, to avoid recording an interrupted scan as a normal completion.
        return;
      }
      setScanSummary(result);
      setGrowthReport(result.growth);
      updateModuleState('diskGrowth', {
        status: 'done',
        fileCount: result.growth.entries.length,
        totalSize: Math.abs(result.growth.total_growth),
      });
    } catch (err) {
      if (cancelRequestedRef.current || scanRunId !== scanRunIdRef.current) {
        updateModuleState('diskGrowth', { status: 'idle', progress: 0 });
        return;
      }
      const message = String(err);
      setError(message);
      updateModuleState('diskGrowth', { status: 'error', error: message });
    } finally {
      if (scanRunId === scanRunIdRef.current) {
        scanningRef.current = false;
      }
    }
  }, [isAdmin, selectedDrive, selectedDriveLabel, selectedDriveLetter, settings.diskGrowthMaxEntries, updateModuleState, moduleT]);

  const handleStopScan = useCallback(async () => {
    cancelRequestedRef.current = true;
    scanRunIdRef.current += 1;
    scanningRef.current = false;
    updateModuleState('diskGrowth', { status: 'idle', progress: 0 });
    try {
      await cancelDiskGrowthScan();
    } catch (err) {
      console.error('停止全盘分析失败:', err);
    }
  }, [updateModuleState]);

  const handleDriveChange = useCallback((driveLetter: string) => {
    if (scanningRef.current) return;
    setSelectedDriveLetter(normalizeDriveLetter(driveLetter));
    resetCurrentDriveResult();
  }, [resetCurrentDriveResult]);

  useEffect(() => {
    if (oneClickScanTrigger > 0 && oneClickScanTrigger !== lastScanTriggerRef.current) {
      lastScanTriggerRef.current = oneClickScanTrigger;
      handleScan();
    }
  }, [oneClickScanTrigger, handleScan]);

  useEffect(() => {
    if (stopScanTrigger > 0 && scanningRef.current) {
      // The global stop button has already sent the backend cancel signal; locally just mark cancellation to avoid old scan results writing back to the UI.
      cancelRequestedRef.current = true;
      scanRunIdRef.current += 1;
      scanningRef.current = false;
    }
  }, [stopScanTrigger]);

  const handleOpenFolder = useCallback(async (path: string) => {
    try {
      await openInFolder(path);
    } catch (err) {
      console.error('打开目录失败:', err);
    }
  }, []);

  const handleSearchPath = useCallback(async (path: string) => {
    try {
      // A changed directory is not necessarily a cleanable one; the search query first confirms purpose, then helps decide whether deletion is safe.
      await openSearchUrl(`${moduleT('diskGrowth.searchQueryPrefix')} ${path} ${moduleT('diskGrowth.searchQuerySuffix')}`);
    } catch (err) {
      console.error('搜索路径用途失败:', err);
    }
  }, []);

  const handleShowDetails = useCallback((entry: DiskGrowthEntry) => {
    setDetailEntry(entry);
  }, []);

  const handleCloseDetails = useCallback(() => {
    setDetailEntry(null);
  }, []);

  const handleExportHtml = useCallback(async () => {
    if (!scanSummary || !growthReport) return;

    try {
      // Fetch the multi-level directory tree once for the current result, avoiding unnecessary IPC and snapshot reads from per-entry requests.
      const exportTree = growthReport.entries.length > 0
        ? await getDiskGrowthExportTree(
            selectTopLevelGrowthPaths(growthReport.entries),
            3,
            scanSummary.drive_letter,
          )
        : { nodes: [], total_nodes: 0, truncated: false };
      const labels = {
        title: moduleT('diskGrowth.exportHtmlTitle'),
        generatedAt: moduleT('diskGrowth.exportGeneratedAt'),
        drive: moduleT('diskGrowth.exportDrive'),
        scanMode: moduleT('diskGrowth.exportScanMode'),
        changeMode: moduleT('diskGrowth.exportChangeMode'),
        baselineMode: moduleT('diskGrowth.exportBaselineMode'),
        currentSize: moduleT('diskGrowth.currentSize'),
        netChange: moduleT('diskGrowth.netChange'),
        noHistory: moduleT('diskGrowth.noHistory'),
        previousScan: moduleT('diskGrowth.previousScan'),
        currentScan: moduleT('diskGrowth.exportCurrentScan'),
        scannedFiles: moduleT('diskGrowth.fileCount'),
        resultCount: moduleT('diskGrowth.exportResultCount'),
        truncatedNote: moduleT('diskGrowth.exportTruncatedNote'),
        path: moduleT('diskGrowth.path'),
        changeTime: moduleT('diskGrowth.changeTime'),
        level: moduleT('diskGrowth.changeLevel'),
        size: moduleT('diskGrowth.currentSizeHeader'),
        difference: moduleT('diskGrowth.differenceHeader'),
        previousSize: moduleT('diskGrowth.previousSize'),
        children: moduleT('diskGrowth.subdirectories'),
        explanation: moduleT('diskGrowth.exportExplanation'),
        suggestion: moduleT('diskGrowth.exportSuggestion'),
        noResult: moduleT('diskGrowth.noChange'),
        depthNote: moduleT('diskGrowth.exportDepthNote'),
        scopeTitle: moduleT('diskGrowth.exportScopeTitle'),
        changeScopeNote: moduleT('diskGrowth.exportChangeScopeNote', {
          pageCount: growthReport.entries.length,
          exportCount: exportTree.total_nodes,
        }),
        baselineScopeNote: moduleT('diskGrowth.exportBaselineScopeNote'),
        levels: {
          significant: moduleT('diskGrowth.level.significant'),
          fast: moduleT('diskGrowth.level.fast'),
          minor: moduleT('diskGrowth.level.minor'),
          stable: moduleT('diskGrowth.level.stable'),
          decreased: moduleT('diskGrowth.level.decreased'),
          new: moduleT('diskGrowth.level.new'),
        },
      };
      const content = buildDiskGrowthHtml(scanSummary, growthReport, exportTree.nodes, {
        labels,
        locale: i18n.language,
        exportTotalNodes: exportTree.total_nodes,
        exportTruncated: exportTree.truncated,
      });
      const reportDate = new Date().toISOString().slice(0, 10);
      const savedPath = await saveDiskGrowthHtml(
        content,
        `LightC_disk_growth_${scanSummary.drive_letter.replace(':', '')}_${reportDate}.html`,
        labels.title,
      );
      if (savedPath) {
        showToast({
          type: 'success',
          title: moduleT('diskGrowth.exportHtmlSuccess'),
          description: savedPath,
        });
      }
    } catch (err) {
      showToast({
        type: 'error',
        title: moduleT('diskGrowth.exportHtmlFailed'),
        description: String(err),
      });
    }
  }, [growthReport, moduleT, scanSummary, showToast]);

  // Change mode feeds the tree list straight from the backend entries. A first scan
  // has no diffs at all; its usage baseline is converted into zero-diff entries so the
  // folder tree and "sort by current size" still work there.
  const listEntries = useMemo<DiskGrowthEntry[]>(() => {
    if (growthReport && growthReport.entries.length > 0) return growthReport.entries;
    return (scanSummary?.analyze.entries ?? []).map((entry) => ({
      path: entry.path,
      old_size: entry.size,
      new_size: entry.size,
      diff: 0,
      diff_percent: 0,
      modified: entry.modified,
      level: 'stable' as const,
      explanation: entry.reason,
      suggestion: entry.suggestion,
      details: [],
    }));
  }, [growthReport, scanSummary]);
  const resultMode = growthReport?.entries.length ? 'change' : 'usage';
  const driveSelector = (
    <div className="flex items-center gap-2 shrink-0" onClick={(event) => event.stopPropagation()}>
      <DriveSelect
        value={selectedDriveLetter}
        drives={drives}
        onChange={handleDriveChange}
        disabled={moduleState.status === 'scanning'}
      />
    </div>
  );

  if (!isPageActive && !detailEntry) {
    return null;
  }

  return (
    <ModuleCard
      id="disk-growth"
        title={navT('diskGrowth')}
        description={`${navT('diskGrowthDesc')} (${selectedDriveLabel})`}
      icon={<HardDrive className="w-5 h-5 text-[var(--brand-green)]" />}
      status={moduleState.status}
      fileCount={moduleState.fileCount}
      totalSize={moduleState.totalSize}
      countLabel={moduleT('diskGrowth.countLabel')}
      onScan={handleScan}
      scanButtonText={moduleT('diskGrowth.scan')}
      scanDisabled={isAdmin === false || Boolean(selectedDrive && !selectedDrive.is_ntfs)}
      titleExtra={driveSelector}
      error={error}
      headerExtra={
        // Put the stop button in the header: a scan may run tens of seconds; users should not hunt in the body to interrupt it
        moduleState.status === 'scanning' ? (
          <StopScanButton onClick={handleStopScan} label={moduleT('diskGrowth.stop')} />
        ) : null
      }
    >
      <div className="glass-block mx-4 mt-4 flex flex-col gap-2 rounded-xl bg-[var(--bg-main)] px-4 py-3 text-[0.86rem] text-[var(--text-muted)] sm:flex-row sm:items-center sm:justify-between">
        <span title={selectedDrive ? driveOptionTitle(selectedDrive) : selectedDriveLabel}>
          {moduleT('diskGrowth.analyzingDrive', { drive: selectedDriveLabel })}
          {selectedDrive?.volume_name ? ` · ${selectedDrive.volume_name}` : ''}
          {selectedDrive?.file_system ? ` · ${selectedDrive.file_system}` : ''}
        </span>
        {selectedDrive ? (
          <span className="tabular-nums">
            {moduleT('diskGrowth.driveSpace', { free: formatSize(selectedDrive.free_space), total: formatSize(selectedDrive.total_space) })}
          </span>
        ) : drivesError ? (
          <span className="text-amber-600 dark:text-amber-400">{moduleT('diskGrowth.drivesError')}</span>
        ) : (
          <span>{moduleT('diskGrowth.loadingDrives')}</span>
        )}
      </div>

      {isAdmin === false && (
        <AdminRequiredHint className="mx-4 mt-4" description={moduleT('diskGrowth.adminHint')} />
      )}

      {selectedDrive && !selectedDrive.is_ntfs && (
        <div className="mx-4 mt-4 flex items-start gap-3 rounded-xl bg-amber-500/10 px-4 py-3 text-[0.93rem] text-amber-700 dark:text-amber-400">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
          <span>{moduleT('diskGrowth.ntfsHint')}</span>
        </div>
      )}

      {moduleState.status === 'idle' && !scanSummary && !growthReport && (
        <div className="p-4">
          <EmptyState
            icon={HardDrive}
            title={moduleT('diskGrowth.idleTitle', { drive: selectedDriveLabel })}
            description={moduleT('diskGrowth.idleDesc', { drive: selectedDriveLabel })}
          />
        </div>
      )}

      {/* Scanning state: use the shared component uniformly.
          phase names and processed counts are implementation details (the timing overlay is for troubleshooting completed scans),
          during scanning, keep only "which drive is being analyzed" and a first-scan note. */}
      {moduleState.status === 'scanning' && (
        <div className="p-4">
          <ModuleScanProgress
            title={moduleT('diskGrowth.scanningTitle', { drive: selectedDriveLabel })}
            description={moduleT('diskGrowth.firstScanHint')}
          />
        </div>
      )}

      {moduleState.status === 'done' && scanSummary && growthReport && (
        <div className="p-4 space-y-4">
          <SummaryCards scanSummary={scanSummary} growthReport={growthReport} driveLabel={driveDisplayName(scanSummary.drive_letter)} />
          <DiagnosticBanner report={growthReport} />

          <div className="flex items-center justify-between">
            <p className="text-[0.93rem] text-[var(--text-muted)]">{moduleT('diskGrowth.resultCount', { count: listEntries.length })}</p>
            <div className="flex items-center gap-3">
              <button
                onClick={handleExportHtml}
                className="flex items-center gap-1.5 rounded-lg border border-[var(--border-color)] px-2.5 py-1.5 text-[0.86rem] text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
                title={moduleT('diskGrowth.exportHtml')}
              >
                <Download className="h-3.5 w-3.5" />
                <span>{moduleT('diskGrowth.exportHtml')}</span>
              </button>
              <span className="text-[0.86rem] text-[var(--text-faint)]">{isAdmin ? moduleT('diskGrowth.adminMode') : moduleT('diskGrowth.nonAdminMode')}</span>
            </div>
          </div>
          <DiskGrowthDiagnostics
            scanSummary={scanSummary}
            resultMode={resultMode}
            maxEntries={settings.diskGrowthMaxEntries}
          />

          {listEntries.length === 0 ? (
            <div className="glass-list bg-[var(--bg-main)] rounded-xl overflow-hidden p-4">
              <EmptyState
                icon={HardDrive}
                title={moduleT('diskGrowth.firstSnapshot')}
                description={moduleT('diskGrowth.firstSnapshotDesc')}
                tone="success"
                compact
              />
            </div>
          ) : (
            <GrowthResultList
              entries={listEntries}
              driveLetter={scanSummary.drive_letter}
              hasDiff={resultMode === 'change'}
              onOpenFolder={handleOpenFolder}
              onSearchPath={handleSearchPath}
              onShowDetails={handleShowDetails}
            />
          )}
        </div>
      )}
      <AnimatePresence>
        {detailEntry && (
          <GrowthDetailModal
            key={`${selectedDriveLetter}-${detailEntry.path}`}
            entry={detailEntry}
            driveLetter={selectedDriveLetter}
            onClose={handleCloseDetails}
            onOpenFolder={handleOpenFolder}
            onSearchPath={handleSearchPath}
          />
        )}
      </AnimatePresence>
    </ModuleCard>
  );
}

export default DiskGrowthModule;
