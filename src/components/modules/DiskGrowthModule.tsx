import { ScanTimingPopover, type TimingGroup } from '../ScanTimingPopover';
import { AdminRequiredHint, useAdminPrivilege } from '../AdminRequiredHint';
import { useAiModuleBridge } from '../ai/useAiModuleBridge';
import { AiAskButton } from '../ai/AiAskButton';
import { buildItemQuestion } from '../ai/aiQuestions';
// ============================================================================
// Multi-drive full-disk change analysis module
//
// Full-disk change analysis only locates the sources of space increase/decrease and provides no deletion capability, to avoid mistaking "changed directories" for "cleanable directories".
// ============================================================================

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import i18n from '../../i18n';
import { createPortal } from 'react-dom';
import { useVirtualizer } from '@tanstack/react-virtual';
import { AnimatePresence, motion } from 'framer-motion';
import {
  AlertTriangle,
  CheckCircle2,
  FolderOpen,
  HardDrive,
  Loader2,
  Minus,
  Search,
  X,
  ChevronRight,
  Sparkles,
  TrendingDown,
  TrendingUp,
  Download,
} from 'lucide-react';
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
  getDiskGrowthDirectoryDetails,
  getDiskGrowthExportTree,
  getDiskGrowthFileDetails,
  openInFolder,
  scanDiskGrowth,
  saveDiskGrowthHtml,
  type DiskGrowthAnalyzeEntry,
  type DiskGrowthDetailEntry,
  type DiskGrowthDirectoryDetailsResponse,
  type DiskGrowthEntry,
  type DiskGrowthFileDetailEntry,
  type DiskGrowthFileDetailsResponse,
  type DiskGrowthReport,
  type DiskGrowthScanResponse,
} from '../../api/commands';
import { formatSize } from '../../utils/format';
import { useToast } from '../Toast';
import { buildDiskGrowthHtml } from '../../utils/diskGrowthHtml';
import type { ModuleRenderProps } from './moduleProps';

function simplifyPath(path: string): string {
  const normalized = path.replace(/\\/g, '/');
  const parts = normalized.split('/').filter(Boolean);
  if (parts.length <= 3) return normalized;
  return `.../${parts.slice(-3).join('/')}`;
}

function formatDiff(diff: number): string {
  const sign = diff >= 0 ? '+' : '-';
  return `${sign}${formatSize(Math.abs(diff))}`;
}


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

function formatModifiedTime(timestamp?: number | null, compact = false): string {
  if (!timestamp) return '-';
  const normalizedTimestamp = timestamp < 10_000_000_000 ? timestamp * 1000 : timestamp;
  const date = new Date(normalizedTimestamp);
  if (Number.isNaN(date.getTime())) return '-';
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, '0');
  const day = String(date.getDate()).padStart(2, '0');
  const hours = String(date.getHours()).padStart(2, '0');
  const minutes = String(date.getMinutes()).padStart(2, '0');
  return compact ? `${month}-${day} ${hours}:${minutes}` : `${year}-${month}-${day} ${hours}:${minutes}`;
}

function buildPathTitle(path: string, modified?: number | null, extraLine?: string): string {
  const lines = [path];
  if (extraLine) lines.push(extraLine);
  const modifiedText = formatModifiedTime(modified);
  if (modifiedText !== '-') {
    // title supports line breaks; when the path is very long, put the real modification time on its own line so key info is not drowned out by the path.
    lines.push(i18n.t('diskGrowth.modifiedTimeTitle', { ns: 'modules', time: modifiedText }));
  }
  return lines.join('\n');
}

function normalizeDiskPath(path: string): string {
  return path.replace(/\\/g, '/').replace(/\/+$/g, '').toLowerCase();
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

function buildChildGrowthEntry(parent: DiskGrowthEntry, path: string): DiskGrowthEntry | null {
  const detail = (parent.details ?? []).find((item) => normalizeDiskPath(item.path) === normalizeDiskPath(path));
  if (!detail) return null;
  const style = getGrowthStyle(detail.level);
  const direction = detail.diff > 0
    ? i18n.t('diskGrowth.increase', { ns: 'modules' })
    : i18n.t('diskGrowth.decrease', { ns: 'modules' });
  return {
    path: detail.path,
    old_size: detail.old_size,
    new_size: detail.new_size,
    diff: detail.diff,
    diff_percent: detail.old_size > 0 ? (detail.diff / detail.old_size) * 100 : 100,
    modified: detail.modified,
    level: detail.level,
    explanation: i18n.t('diskGrowth.growthExplanation', {
      ns: 'modules',
      level: style.label,
      direction,
      size: formatSize(Math.abs(detail.diff)),
    }),
    suggestion: i18n.t('diskGrowth.growthSuggestion', { ns: 'modules' }),
    details: [],
  };
}

function getGrowthStyle(level: DiskGrowthEntry['level']) {
  switch (level) {
    case 'significant':
      return { icon: TrendingUp, color: 'text-red-500', label: i18n.t('diskGrowth.level.significant', { ns: 'modules' }) };
    case 'fast':
      return { icon: TrendingUp, color: 'text-orange-500', label: i18n.t('diskGrowth.level.fast', { ns: 'modules' }) };
    case 'minor':
      return { icon: TrendingUp, color: 'text-amber-500', label: i18n.t('diskGrowth.level.minor', { ns: 'modules' }) };
    case 'decreased':
      return { icon: TrendingDown, color: 'text-green-500', label: i18n.t('diskGrowth.level.decreased', { ns: 'modules' }) };
    case 'new':
      return { icon: Sparkles, color: 'text-blue-500', label: i18n.t('diskGrowth.level.new', { ns: 'modules' }) };
    default:
      return { icon: Minus, color: 'text-[var(--text-faint)]', label: i18n.t('diskGrowth.level.stable', { ns: 'modules' }) };
  }
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
          <span className="truncate">{moduleT('diskGrowth.sorted')}</span>
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

function entryFromGrowth(growth: DiskGrowthEntry): DiskGrowthAnalyzeEntry {
  return {
    path: growth.path,
    size: growth.new_size,
    category: '变化目录',
    modified: growth.modified,
    risk: 'safe',
    action: 'ignore',
    reason: growth.explanation,
    suggestion: growth.suggestion,
    matched_rule_id: null,
    tags: [],
  };
}

function ChangeRow({
  entry,
  growth,
  onOpenFolder,
  onSearchPath,
  onShowDetails,
}: {
  entry: DiskGrowthAnalyzeEntry;
  growth: DiskGrowthEntry | null;
  onOpenFolder: (path: string) => void;
  onSearchPath: (path: string) => void;
  onShowDetails: (growth: DiskGrowthEntry) => void;
}) {
  const { t: moduleT } = useTranslation('modules');
  const style = growth ? getGrowthStyle(growth.level) : getGrowthStyle('stable');
  const Icon = style.icon;
  const diff = growth?.diff ?? 0;
  const modified = growth?.modified ?? entry.modified;
  const modifiedTimeLabel = formatModifiedTime(modified, true);
  const modifiedTimeTitle = formatModifiedTime(modified);
  const rowTitle = buildPathTitle(entry.path, modified, growth?.explanation ?? entry.reason);

  return (
    <div className="flex items-center gap-3 px-4 py-2.5 hover:bg-[var(--bg-hover)] transition-colors group">
      <div
        className={`w-1.5 h-8 rounded-full shrink-0 ${
          diff > 0 ? 'bg-red-400' : diff < 0 ? 'bg-green-400' : 'bg-gray-300'
        }`}
      />

      <div className="flex-1 min-w-0">
        <p
          className="text-[0.93rem] text-[var(--text-primary)] truncate cursor-pointer hover:text-[var(--brand-green)] transition-colors"
          title={rowTitle}
          onClick={() => onOpenFolder(entry.path)}
        >
          {simplifyPath(entry.path)}
        </p>
        <p className="text-[0.79rem] text-[var(--text-faint)] mt-0.5 truncate">
          {growth?.explanation ?? entry.reason}
        </p>
      </div>

      <span
        className="w-20 text-center text-[0.79rem] text-[var(--text-muted)] tabular-nums shrink-0"
        title={modifiedTimeTitle === '-'
          ? moduleT('diskGrowth.modifiedTimeMissing')
          : moduleT('diskGrowth.modifiedTimeTitle', { time: modifiedTimeTitle })}
      >
        {modifiedTimeLabel}
      </span>

      <span className={`flex items-center gap-1 w-24 justify-end text-[0.86rem] shrink-0 ${style.color}`}>
        <Icon className="w-3.5 h-3.5" />
        {style.label}
      </span>

      <span className="text-[0.93rem] font-medium text-[var(--text-primary)] tabular-nums w-20 text-right shrink-0">
        {formatSize(entry.size)}
      </span>

      <button
        onClick={() => growth && onShowDetails(growth)}
        disabled={!growth}
        className={`flex w-24 shrink-0 items-center justify-end gap-1 text-[0.93rem] font-medium tabular-nums ${style.color} ${
          growth ? 'hover:underline underline-offset-4 cursor-pointer' : 'cursor-default'
        }`}
        title={growth ? moduleT('diskGrowth.viewDetails') : undefined}
      >
        {diff === 0 ? '-' : formatDiff(diff)}
        {/* Previously the underline appeared only on hover, giving no hint that this column opens details,
            so add a persistent arrow hint. The animation is very light: nudge 3px right, play 3 times, then stop
            (see .hint-nudge in App.css) so it does not keep moving across a list of dozens of rows. */}
        {growth && <ChevronRight className="hint-nudge h-3 w-3 shrink-0" aria-hidden="true" />}
      </button>

      {/* Action buttons: **always visible** (previously shown only on hover, so users did not know they existed),
          and leave some breathing room from the "change + arrow" on the left - sitting together looks cramped. */}
      <div className="flex shrink-0 items-center justify-end gap-1 pl-2">
        <button
          onClick={() => onSearchPath(entry.path)}
          className="p-1.5 rounded-lg text-[var(--text-muted)] hover:bg-[var(--brand-green-10)] hover:text-[var(--brand-green)] transition"
          title={moduleT('diskGrowth.searchPath')}
        >
          <Search className="w-4 h-4" />
        </button>
        {/* Quick ask: a grown directory is only a "change", not necessarily deletable; the question makes clear it asks about purpose */}
        <AiAskButton
          question={buildItemQuestion(entry.path, 'folder')}
          className="inline-flex h-7 w-7 items-center justify-center"
        />
        <button
          onClick={() => onOpenFolder(entry.path)}
          className="p-1.5 rounded-lg text-[var(--text-muted)] hover:bg-[var(--brand-green-10)] hover:text-[var(--brand-green)] transition"
          title={moduleT('diskGrowth.openFolder')}
        >
          <FolderOpen className="w-4 h-4" />
        </button>
      </div>
    </div>
  );
}

function DiskGrowthDetailsModal({
  entry,
  driveLetter,
  onClose,
  onOpenFolder,
}: {
  entry: DiskGrowthEntry | null;
  driveLetter: string;
  onClose: () => void;
  onOpenFolder: (path: string) => void;
}) {
  const { t: moduleT } = useTranslation('modules');
  const [currentEntry, setCurrentEntry] = useState<DiskGrowthEntry | null>(entry);
  const [fileDetails, setFileDetails] = useState<DiskGrowthFileDetailsResponse | null>(null);
  const [directoryDetails, setDirectoryDetails] = useState<DiskGrowthDirectoryDetailsResponse | null>(null);
  const [fileRows, setFileRows] = useState<DiskGrowthFileDetailEntry[]>([]);
  const [directoryRows, setDirectoryRows] = useState<DiskGrowthDetailEntry[]>([]);
  const [fileLoading, setFileLoading] = useState(false);
  const [directoryLoading, setDirectoryLoading] = useState(false);
  const [fileError, setFileError] = useState<string | null>(null);
  const [directoryError, setDirectoryError] = useState<string | null>(null);
  const fileScrollRef = useRef<HTMLDivElement | null>(null);
  const directoryScrollRef = useRef<HTMLDivElement | null>(null);
  const rootPath = entry?.path ?? '';
  const currentPath = currentEntry?.path ?? rootPath;
  const detailPageSize = 200;

  useEffect(() => {
    setCurrentEntry(entry);
  }, [entry]);

  useEffect(() => {
    if (!currentEntry) return;

    let cancelled = false;
    setFileDetails(null);
    setDirectoryDetails(null);
    setFileRows([]);
    setDirectoryRows([]);
    setFileError(null);
    setDirectoryError(null);
    setFileLoading(true);
    setDirectoryLoading(true);

    // File-level details are lazy-loaded on demand to avoid carrying hundreds of thousands of file records in the main scan result.
    getDiskGrowthFileDetails(currentEntry.path, 0, detailPageSize, driveLetter)
      .then((result) => {
        if (!cancelled) {
          setFileDetails(result);
          setFileRows(result.entries);
        }
      })
      .catch((err) => {
        if (!cancelled) setFileError(String(err));
      })
      .finally(() => {
        if (!cancelled) setFileLoading(false);
      });

    // Directory details are also lazy-loaded per current directory, to avoid being unable to continue pagination when the main result carries only a few details.
    getDiskGrowthDirectoryDetails(currentEntry.path, 0, detailPageSize, driveLetter)
      .then((result) => {
        if (!cancelled) {
          setDirectoryDetails(result);
          setDirectoryRows(result.entries);
        }
      })
      .catch((err) => {
        if (!cancelled) setDirectoryError(String(err));
      })
      .finally(() => {
        if (!cancelled) setDirectoryLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [currentEntry, driveLetter]);

  const fileVirtualizer = useVirtualizer({
    count: fileRows.length,
    getScrollElement: () => fileScrollRef.current,
    estimateSize: () => 58,
    overscan: 8,
  });
  const directoryVirtualizer = useVirtualizer({
    count: directoryRows.length,
    getScrollElement: () => directoryScrollRef.current,
    estimateSize: () => 58,
    overscan: 8,
  });

  if (!entry || !currentEntry) return null;

  const style = getGrowthStyle(currentEntry.level);
  const rootNormalized = normalizeDiskPath(rootPath);
  const currentNormalized = normalizeDiskPath(currentPath);
  const relativeParts = currentNormalized.startsWith(rootNormalized)
    ? currentPath
        .slice(rootPath.length)
        .replace(/^\/+/, '')
        .split('/')
        .filter(Boolean)
    : [];
  const breadcrumbItems = [
    { label: rootPath, path: rootPath },
    ...relativeParts.map((part, index) => ({
      label: part,
      path: `${rootPath.replace(/\/+$/g, '')}/${relativeParts.slice(0, index + 1).join('/')}`,
    })),
  ];
  const handleEnterDirectory = (path: string) => {
    const childEntry = buildChildGrowthEntry(
      { ...currentEntry, details: directoryRows.length > 0 ? directoryRows : currentEntry.details },
      path
    );
    if (childEntry) setCurrentEntry(childEntry);
  };
  const loadMoreFiles = async () => {
    if (!currentEntry || fileLoading || !fileDetails?.has_more) return;
    setFileLoading(true);
    try {
      const result = await getDiskGrowthFileDetails(currentEntry.path, fileRows.length, detailPageSize, driveLetter);
      setFileDetails(result);
      setFileRows((rows) => [...rows, ...result.entries]);
    } catch (err) {
      setFileError(String(err));
    } finally {
      setFileLoading(false);
    }
  };
  const loadMoreDirectories = async () => {
    if (!currentEntry || directoryLoading || !directoryDetails?.has_more) return;
    setDirectoryLoading(true);
    try {
      const result = await getDiskGrowthDirectoryDetails(currentEntry.path, directoryRows.length, detailPageSize, driveLetter);
      setDirectoryDetails(result);
      setDirectoryRows((rows) => [...rows, ...result.entries]);
    } catch (err) {
      setDirectoryError(String(err));
    } finally {
      setDirectoryLoading(false);
    }
  };
  // The modal must be mounted under body to avoid the outer motion transform of the page mode changing the reference for fixed positioning.
  return createPortal(
    <motion.div
      className="fixed inset-0 z-[10000] flex items-center justify-center bg-black/40 backdrop-blur-sm glass-backdrop"
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      transition={{ duration: 0.18, ease: 'easeOut' }}
      onClick={onClose}
    >
      <motion.div
        className="w-[74.29rem] max-w-[calc(100vw-32px)] h-[90vh] max-h-[calc(100vh-32px)] rounded-2xl bg-[var(--bg-card)] glass-dialog shadow-2xl border border-[var(--border-color)] overflow-hidden flex flex-col"
        initial={{ opacity: 0, scale: 0.96, y: 12 }}
        animate={{ opacity: 1, scale: 1, y: 0 }}
        exit={{ opacity: 0, scale: 0.96, y: 12 }}
        transition={{ duration: 0.2, ease: [0.22, 1, 0.36, 1] }}
        onClick={(event) => event.stopPropagation()}
      >
        <div className="flex items-center justify-between px-5 py-4 border-b border-[var(--border-color)]">
          <div className="min-w-0">
            <h3 className="text-sm font-semibold text-[var(--text-primary)]">{moduleT('diskGrowth.details')}</h3>
            <div className="mt-1 flex items-center gap-1 text-xs text-[var(--text-muted)] min-w-0">
              {breadcrumbItems.map((item, index) => (
                <div key={item.path} className="flex items-center gap-1 min-w-0">
                  {index > 0 && <ChevronRight className="w-3 h-3 shrink-0 text-[var(--text-faint)]" />}
                  <button
                    onClick={() => {
                      if (index === 0) {
                        setCurrentEntry(entry);
                        return;
                      }
                      const childEntry = buildChildGrowthEntry(entry, item.path);
                      if (childEntry) setCurrentEntry(childEntry);
                    }}
                    className="truncate hover:text-[var(--brand-green)] transition-colors"
                    title={item.path}
                  >
                    {index === 0 ? item.label : item.label}
                  </button>
                </div>
              ))}
            </div>
          </div>
          <button
            onClick={onClose}
            className="p-1.5 rounded-lg text-[var(--text-muted)] hover:text-[var(--text-primary)] hover:bg-[var(--bg-hover)] transition-colors"
          >
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="p-5 space-y-4 overflow-hidden flex-1 min-h-0 flex flex-col">
          <div className="grid grid-cols-3 gap-3 shrink-0">
            <div className="glass-block rounded-xl bg-[var(--bg-main)] px-3 py-2">
              <p className="text-[0.79rem] text-[var(--text-muted)]">{moduleT('diskGrowth.previousSize')}</p>
              <p className="mt-1 text-sm font-semibold text-[var(--text-primary)] tabular-nums">
                {formatSize(currentEntry.old_size)}
              </p>
            </div>
            <div className="glass-block rounded-xl bg-[var(--bg-main)] px-3 py-2">
              <p className="text-[0.79rem] text-[var(--text-muted)]">{moduleT('diskGrowth.currentSize')}</p>
              <p className="mt-1 text-sm font-semibold text-[var(--text-primary)] tabular-nums">
                {formatSize(currentEntry.new_size)}
              </p>
            </div>
            <div className="glass-block rounded-xl bg-[var(--bg-main)] px-3 py-2">
              <p className="text-[0.79rem] text-[var(--text-muted)]">{moduleT('diskGrowth.difference')}</p>
              <p className={`mt-1 text-sm font-semibold tabular-nums ${style.color}`}>
                {formatDiff(currentEntry.diff)}
              </p>
            </div>
          </div>

          <div className="grid grid-cols-1 sm:grid-cols-2 gap-4 flex-1 min-h-0">
            <div className="glass-list rounded-xl bg-[var(--bg-main)] border border-[var(--border-color)] overflow-hidden min-w-0 min-h-0 flex flex-col">
              <div className="flex items-center gap-3 px-4 py-2 border-b border-[var(--border-color)] text-[0.79rem] text-[var(--text-faint)] shrink-0">
                <span className="flex-1">{moduleT('diskGrowth.subdirectories')}</span>
                <span className="w-20 text-right">{moduleT('diskGrowth.currentSize')}</span>
                <span className="w-24 text-right">{moduleT('diskGrowth.difference')}</span>
                <span className="w-24" />
              </div>
              <div ref={directoryScrollRef} className="flex-1 min-h-0 overflow-auto">
                {directoryError ? (
                  <div className="px-4 py-8 text-center text-sm text-[var(--text-muted)]">{directoryError}</div>
                ) : directoryRows.length > 0 ? (
                  <div style={{ height: `${directoryVirtualizer.getTotalSize()}px`, position: 'relative' }}>
                    {directoryVirtualizer.getVirtualItems().map((virtualItem) => {
                      const detail = directoryRows[virtualItem.index];
                      if (!detail) return null;
                      const detailStyle = getGrowthStyle(detail.level);
                      return (
                        <div
                          key={detail.path}
                          className="absolute left-0 top-0 w-full flex items-center gap-3 px-4 py-2.5 hover:bg-[var(--bg-hover)] transition-colors"
                          style={{ transform: `translateY(${virtualItem.start}px)`, height: `${virtualItem.size}px` }}
                        >
                          <div className="flex-1 min-w-0">
                            <p className="text-[0.93rem] text-[var(--text-primary)] truncate" title={buildPathTitle(detail.path, detail.modified)}>{detail.name}</p>
                            <p className="text-[0.79rem] text-[var(--text-faint)] truncate" title={buildPathTitle(detail.path, detail.modified)}>{detail.path}</p>
                          </div>
                          <span className="w-20 text-right text-[0.93rem] font-medium text-[var(--text-primary)] tabular-nums">{formatSize(detail.new_size)}</span>
                          <span className={`w-24 text-right text-[0.93rem] font-medium tabular-nums ${detailStyle.color}`}>{formatDiff(detail.diff)}</span>
                          <div className="w-24 flex items-center justify-end gap-0.5">
                            {/* Quick ask: this row is **what the user expanded to see**, so the path in the question is itself,
                                `skipLookup` -> do not let the assistant look it up (this file-level detail is outside its read capability,
                                looking it up would only waste a try). */}
                            <AiAskButton
                              question={buildItemQuestion(detail.path, 'folder')}
                              skipLookup
                              className="inline-flex h-7 w-7 items-center justify-center"
                            />
                            <button onClick={() => onOpenFolder(detail.path)} className="p-1.5 rounded-lg text-[var(--text-muted)] hover:bg-[var(--brand-green-10)] hover:text-[var(--brand-green)] transition" title={moduleT('diskGrowth.openFolder')}>
                              <FolderOpen className="w-4 h-4" />
                            </button>
                            <button onClick={() => handleEnterDirectory(detail.path)} className="p-1.5 rounded-lg text-[var(--text-muted)] hover:bg-[var(--brand-green-10)] hover:text-[var(--brand-green)] transition" title={moduleT('diskGrowth.enterDirectory')}>
                              <ChevronRight className="w-4 h-4" />
                            </button>
                          </div>
                        </div>
                      );
                    })}
                  </div>
                ) : directoryLoading ? (
                  <div className="flex items-center justify-center gap-2 px-4 py-8 text-sm text-[var(--text-muted)]">
                    <Loader2 className="w-4 h-4 animate-spin text-[var(--brand-green)]" />
                    {moduleT('diskGrowth.loadingDirectories')}
                  </div>
                ) : (
                  <div className="px-4 py-8 text-center text-sm text-[var(--text-muted)]">{moduleT('diskGrowth.noDirectories')}</div>
                )}
              </div>
              {directoryDetails && (
                <div className="flex items-center justify-between gap-3 border-t border-[var(--border-color)] px-4 py-2 text-xs text-[var(--text-faint)] shrink-0">
                  <span>{moduleT('diskGrowth.showing', { current: directoryRows.length, total: directoryDetails.total_changed_dirs })}</span>
                  {directoryDetails.has_more ? (
                    <button onClick={loadMoreDirectories} disabled={directoryLoading} className="text-[var(--brand-green)] hover:text-[var(--brand-green-hover)] disabled:opacity-60 transition-colors">
                      {directoryLoading ? moduleT('diskGrowth.loading') : moduleT('diskGrowth.loadMoreDirectories')}
                    </button>
                  ) : (
                    <span>{moduleT('diskGrowth.loadedAll')}</span>
                  )}
                </div>
              )}
            </div>

            <div className="glass-list rounded-xl bg-[var(--bg-main)] border border-[var(--border-color)] overflow-hidden min-w-0 min-h-0 flex flex-col">
              <div className="flex items-center gap-3 px-4 py-2 border-b border-[var(--border-color)] text-[0.79rem] text-[var(--text-faint)] shrink-0">
                <span className="flex-1">{moduleT('diskGrowth.files')}</span>
                <span className="w-20 text-right">{moduleT('diskGrowth.currentSize')}</span>
                <span className="w-24 text-right">{moduleT('diskGrowth.difference')}</span>
                <span className="w-16" />
              </div>
              <div ref={fileScrollRef} className="flex-1 min-h-0 overflow-auto">
                {fileError ? (
                  <div className="px-4 py-8 text-center">
                    <p className="text-sm text-[var(--text-muted)]">{moduleT('diskGrowth.fileDetailsUnavailable')}</p>
                    <p className="mt-1 text-xs text-[var(--text-faint)]">{fileError}</p>
                  </div>
                ) : fileRows.length > 0 ? (
                  <div style={{ height: `${fileVirtualizer.getTotalSize()}px`, position: 'relative' }}>
                    {fileVirtualizer.getVirtualItems().map((virtualItem) => {
                      const file = fileRows[virtualItem.index];
                      if (!file) return null;
                      const fileStyle = getGrowthStyle(file.level);
                      return (
                        <div
                          key={file.path}
                          className="absolute left-0 top-0 w-full flex items-center gap-3 px-4 py-2.5 hover:bg-[var(--bg-hover)] transition-colors"
                          style={{ transform: `translateY(${virtualItem.start}px)`, height: `${virtualItem.size}px` }}
                        >
                          <div className="flex-1 min-w-0">
                            <p className="text-[0.93rem] text-[var(--text-primary)] truncate" title={buildPathTitle(file.path, file.modified)}>{file.name}</p>
                            <p className="text-[0.79rem] text-[var(--text-faint)] truncate" title={buildPathTitle(file.path, file.modified)}>{file.path}</p>
                          </div>
                          <span className="w-20 text-right text-[0.93rem] font-medium text-[var(--text-primary)] tabular-nums">{formatSize(file.new_size)}</span>
                          <span className={`w-24 text-right text-[0.93rem] font-medium tabular-nums ${fileStyle.color}`}>{formatDiff(file.diff)}</span>
                          <div className="w-16 flex items-center justify-end gap-0.5">
                            {/* Quick ask: this row is a **specific file** (seen only after expanding details),
                                `skipLookup` -> do not let the assistant look it up; answer "what this is and whether it can be deleted" from the model's own knowledge */}
                            <AiAskButton
                              question={buildItemQuestion(file.path, 'file')}
                              skipLookup
                              className="inline-flex h-7 w-7 items-center justify-center"
                            />
                            <button onClick={() => onOpenFolder(file.path)} className="p-1.5 rounded-lg text-[var(--text-muted)] hover:bg-[var(--brand-green-10)] hover:text-[var(--brand-green)] transition" title={moduleT('diskGrowth.openLocation')}>
                              <FolderOpen className="w-4 h-4" />
                            </button>
                          </div>
                        </div>
                      );
                    })}
                  </div>
                ) : fileLoading ? (
                  <div className="flex items-center justify-center gap-2 px-4 py-8 text-sm text-[var(--text-muted)]">
                    <Loader2 className="w-4 h-4 animate-spin text-[var(--brand-green)]" />
                    {moduleT('diskGrowth.loadingFiles')}
                  </div>
                ) : (
                  <div className="px-4 py-8 text-center text-sm text-[var(--text-muted)]">{moduleT('diskGrowth.noFiles')}</div>
                )}
              </div>
              {fileDetails && (
                <div className="flex items-center justify-between gap-3 border-t border-[var(--border-color)] px-4 py-2 text-xs text-[var(--text-faint)] shrink-0">
                  <span>{moduleT('diskGrowth.showing', { current: fileRows.length, total: fileDetails.total_changed_files })}</span>
                  {fileDetails.has_more ? (
                    <button onClick={loadMoreFiles} disabled={fileLoading} className="text-[var(--brand-green)] hover:text-[var(--brand-green-hover)] disabled:opacity-60 transition-colors">
                      {fileLoading ? moduleT('diskGrowth.loading') : moduleT('diskGrowth.loadMoreFiles')}
                    </button>
                  ) : (
                    <span>{moduleT('diskGrowth.loadedAll')}</span>
                  )}
                </div>
              )}
            </div>
          </div>

          <p className="text-[0.79rem] text-[var(--text-faint)] shrink-0">
            {moduleT('diskGrowth.detailNote', { count: detailPageSize })}
          </p>
        </div>
      </motion.div>
    </motion.div>,
    document.body
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
  const [showAll, setShowAll] = useState(false);
  const [detailEntry, setDetailEntry] = useState<DiskGrowthEntry | null>(null);
  const { drives, error: drivesError } = useLocalDrives();
  const [selectedDriveLetter, setSelectedDriveLetter] = useState('C:');
  const selectedDrive = drives.find((drive) => drive.drive_letter === selectedDriveLetter) ?? null;
  const selectedDriveLabel = driveDisplayName(selectedDriveLetter);

  const resetCurrentDriveResult = useCallback(() => {
    setScanSummary(null);
    setGrowthReport(null);
    setShowAll(false);
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
    setShowAll(false);
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

  const growthMap = useMemo(() => {
    const map = new Map<string, DiskGrowthEntry>();
    for (const entry of growthReport?.entries ?? []) {
      map.set(entry.path.toLowerCase().replace(/\\/g, '/'), entry);
    }
    return map;
  }, [growthReport]);

  const entries = growthReport?.entries.length
    ? growthReport.entries.map(entryFromGrowth)
    : scanSummary?.analyze.entries ?? [];
  const resultMode = growthReport?.entries.length ? 'change' : 'usage';
  const displayedEntries = showAll ? entries : entries.slice(0, 20);
  const hasMore = entries.length > displayedEntries.length;
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
            <p className="text-[0.93rem] text-[var(--text-muted)]">{moduleT('diskGrowth.resultCount', { count: entries.length })}</p>
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

          <div className="glass-list bg-[var(--bg-main)] rounded-xl overflow-hidden">
            <div className="flex items-center gap-3 px-4 py-2 border-b border-[var(--border-color)] text-[0.79rem] text-[var(--text-faint)] uppercase tracking-wider">
              <div className="w-1.5 shrink-0" />
              <div className="flex-1">{moduleT('diskGrowth.path')}</div>
              <div className="w-20 shrink-0 text-center">{moduleT('diskGrowth.changeTime')}</div>
              <div className="w-24 shrink-0 text-right">{moduleT('diskGrowth.changeLevel')}</div>
              <div className="w-20 shrink-0 text-right">{moduleT('diskGrowth.currentSizeHeader')}</div>
              {/* Leave a spacer at the end as wide as the data-column arrow, otherwise numbers indent one extra step past the header's right edge and columns misalign */}
              <div className="flex w-24 shrink-0 items-center justify-end gap-1">
                {moduleT('diskGrowth.differenceHeader')}
                <span className="h-3 w-3 shrink-0" aria-hidden="true" />
              </div>
              <div className="w-16 shrink-0" />
            </div>

            {displayedEntries.map((entry) => {
              const normalizedPath = entry.path.toLowerCase().replace(/\\/g, '/');
              return (
                <ChangeRow
                  key={entry.path}
                  entry={entry}
                  growth={growthMap.get(normalizedPath) ?? null}
                  onOpenFolder={handleOpenFolder}
                  onSearchPath={handleSearchPath}
                  onShowDetails={handleShowDetails}
                />
              );
            })}

            {hasMore && (
              <button
                onClick={() => setShowAll(true)}
                className="w-full py-3 text-center text-[0.93rem] text-[var(--brand-green)] hover:bg-[var(--bg-hover)] transition-colors"
              >
                {moduleT('diskGrowth.showAll', { count: entries.length })}
              </button>
            )}

            {entries.length === 0 && (
              <div className="p-4">
                <EmptyState
                  icon={HardDrive}
                  title={moduleT('diskGrowth.firstSnapshot')}
                  description={moduleT('diskGrowth.firstSnapshotDesc')}
                  tone="success"
                  compact
                />
              </div>
            )}
          </div>
        </div>
      )}
      <AnimatePresence>
        {detailEntry && (
          <DiskGrowthDetailsModal
            key={`${selectedDriveLetter}-${detailEntry.path}`}
            entry={detailEntry}
            driveLetter={selectedDriveLetter}
            onClose={handleCloseDetails}
            onOpenFolder={handleOpenFolder}
          />
        )}
      </AnimatePresence>
    </ModuleCard>
  );
}

export default DiskGrowthModule;
