// ============================================================================
// Dashboard top statistics bar component
// Displays the C drive health score, disk usage, and a one-click scan button
// ============================================================================

import { useCallback, useEffect, useMemo, useState, useRef } from 'react';
import { HardDrive, Moon, RefreshCw, ArrowLeftRight, Trash2, Zap, Square } from 'lucide-react';
import { useDashboardActions, useDashboardSummary } from '../contexts/DashboardContext';
import { formatSize } from '../utils/format';
import { useTranslation } from 'react-i18next';
import { defaultDriveLetter, useLocalDrives } from './ui/DriveSelect';
import { DiskSwitchModal } from './DiskSwitchModal';
import { useToast } from './Toast';

// ============================================================================
// Number-counting animation hook
// ============================================================================

function useAnimatedNumber(targetValue: number, duration: number = 800) {
  const [displayValue, setDisplayValue] = useState(0);
  const animationRef = useRef<number | null>(null);
  const startTimeRef = useRef<number | null>(null);
  const startValueRef = useRef<number>(0);

  useEffect(() => {
    startValueRef.current = displayValue;
    startTimeRef.current = null;

    const animate = (currentTime: number) => {
      if (!startTimeRef.current) {
        startTimeRef.current = currentTime;
      }

      const elapsed = currentTime - startTimeRef.current;
      const progress = Math.min(elapsed / duration, 1);
      
      // Uses the easeOutExpo easing function
      const easeProgress = progress === 1 ? 1 : 1 - Math.pow(2, -10 * progress);
      const currentValue = Math.round(startValueRef.current + (targetValue - startValueRef.current) * easeProgress);
      
      setDisplayValue(currentValue);

      if (progress < 1) {
        animationRef.current = requestAnimationFrame(animate);
      }
    };

    animationRef.current = requestAnimationFrame(animate);

    return () => {
      if (animationRef.current) {
        cancelAnimationFrame(animationRef.current);
      }
    };
  }, [targetValue, duration]);

  return displayValue;
}

// ============================================================================
// Gets the color configuration based on the score
// ============================================================================

// WeChat-style score color configuration
function getScoreColor(score: number) {
  if (score >= 80) {
    return {
      text: 'text-[#07C160]',      // WeChat green
      bg: 'bg-[#07C160]',
      bgLight: 'bg-[#07C160]/10',
      labelKey: 'excellent',
    };
  } else if (score >= 60) {
    return {
      text: 'text-[#FA9D3B]',      // soft orange
      bg: 'bg-[#FA9D3B]',
      bgLight: 'bg-[#FA9D3B]/10',
      labelKey: 'good',
    };
  } else {
    return {
      text: 'text-[#FA5151]',      // soft red
      bg: 'bg-[#FA5151]',
      bgLight: 'bg-[#FA5151]/10',
      labelKey: 'needsOptimization',
    };
  }
}

// ============================================================================
// Component props
// ============================================================================

interface DashboardHeaderProps {
  /** One-click scan callback */
  onOneClickScan: () => void;
  /** Callback to show the welcome modal (easter egg) */
  onShowWelcome?: () => void;
  /** In page mode that focuses a single module, hiding one-click scan avoids making users think only the current page is scanned. */
  hideOneClickScan?: boolean;
}

// ============================================================================
// Disk usage tiers
//
// The progress bar and the "Free" number must use the same set of thresholds: previously 90/75 was inlined only in the progress bar,
// now the text also changes color by usage; writing it in two places will sooner or later drift apart.
// ============================================================================

type DiskUsageTone = 'normal' | 'warning' | 'danger';

/** Usage rate → tier. Plenty uses the theme green, low uses orange, tight uses red. */
function getDiskUsageTone(usagePercent: number): DiskUsageTone {
  if (usagePercent > 90) return 'danger';
  if (usagePercent > 75) return 'warning';
  return 'normal';
}

const DISK_BAR_CLASS: Record<DiskUsageTone, string> = {
  normal: 'bg-[var(--brand-green)]',
  warning: 'bg-[var(--color-warning)]',
  danger: 'bg-[var(--color-danger)]',
};

const DISK_TEXT_CLASS: Record<DiskUsageTone, string> = {
  normal: 'text-[var(--brand-green)]',
  warning: 'text-[var(--color-warning)]',
  danger: 'text-[var(--color-danger)]',
};

/**
 * Cooldown duration after a manual refresh.
 * Disk capacity won't change twice within seconds; allowing rapid clicks just piles up "Refreshed" toasts (which auto-dismiss but still flood the screen).
 */
const REFRESH_COOLDOWN_MS = 2000;

// ============================================================================
// Component implementation
// ============================================================================

export function DashboardHeader({ onOneClickScan, onShowWelcome, hideOneClickScan = false }: DashboardHeaderProps) {
  const { diskInfo, healthData, isLoadingHealth, isAnyScanning } = useDashboardSummary();
  const { stopAllScans, refreshHealthScore } = useDashboardActions();
  const { t } = useTranslation('common');
  const { showToast } = useToast();

  /**
   * The top bar's disk bar supports switching drive letters and manual refresh.
   *
   * The data source uses `useLocalDrives` (it already returns each drive's capacity, free space, and usage rate),
   * instead of `diskInfo` in context — the latter has only one entry for the system drive and can't switch drives.
   * `diskInfo` is kept as a fallback: while the drive list hasn't loaded yet, use it to hold the display and avoid a blank first frame.
   */
  const { drives, loading: drivesLoading, refresh: refreshDrives } = useLocalDrives();
  const [selectedDrive, setSelectedDrive] = useState('');

  /** During cooldown the button stays disabled so users can see "just refreshed" rather than clicking with no response. */
  const [isRefreshCooling, setIsRefreshCooling] = useState(false);
  /** state updates are async: rapid clicks in the same frame must be blocked by a ref lock, not just by disabled. */
  const refreshInFlightRef = useRef(false);
  const cooldownTimerRef = useRef<number | null>(null);

  useEffect(() => () => {
    if (cooldownTimerRef.current !== null) window.clearTimeout(cooldownTimerRef.current);
  }, []);

  /** Manual refresh: don't re-fire while requesting or in cooldown; show a light toast when done, report both success and failure. */
  const handleRefreshDrives = useCallback(async () => {
    if (drivesLoading || isRefreshCooling || refreshInFlightRef.current) return;
    refreshInFlightRef.current = true;
    setIsRefreshCooling(true);
    try {
      const succeeded = await refreshDrives();
      showToast(
        succeeded
          ? { type: 'success', title: t('refreshDiskInfoDone') }
          : { type: 'error', title: t('refreshDiskInfoFailed') },
      );
    } finally {
      refreshInFlightRef.current = false;
      // Cooldown starts when the request ends: a slow request won't unlock early because the timer finished early
      cooldownTimerRef.current = window.setTimeout(() => {
        cooldownTimerRef.current = null;
        setIsRefreshCooling(false);
      }, REFRESH_COOLDOWN_MS);
    }
  }, [drivesLoading, isRefreshCooling, refreshDrives, showToast, t]);

  // On first receiving the list, default-select the system drive (the one flagged is_system; fall back to C: if not found).
  useEffect(() => {
    if (!selectedDrive && drives.length > 0) {
      setSelectedDrive(defaultDriveLetter(drives));
    }
  }, [drives, selectedDrive]);

  const activeDrive = useMemo(
    () => drives.find((drive) => drive.drive_letter === selectedDrive) ?? null,
    [drives, selectedDrive],
  );

  // If the selected drive is unplugged or disappears after refresh: the capacity bar falls back to the system drive, and the score must fall back too,
  // otherwise you get a mismatch of "showing C drive capacity but scoring by D drive".
  useEffect(() => {
    if (!selectedDrive || drives.length === 0 || activeDrive) return;
    const fallback = defaultDriveLetter(drives);
    setSelectedDrive(fallback);
    void refreshHealthScore(fallback);
  }, [selectedDrive, drives, activeDrive, refreshHealthScore]);
  /** One display item: the selected drive takes priority, otherwise fall back to the context's system drive. */
  const disk = activeDrive
    ? { drive_letter: activeDrive.drive_letter, free_space: activeDrive.free_space, total_space: activeDrive.total_space, usage_percent: activeDrive.usage_percent }
    : diskInfo;

  // Use a modal for switching partitions, don't stuff a dropdown into the top bar: the bar is only tens of pixels tall, and a dropdown's font size and width keep misaligning.
  const [isDiskSwitchOpen, setIsDiskSwitchOpen] = useState(false);

  /** The progress bar and "Free" number share this one tier, so color and thresholds always stay consistent. */
  const usageTone = getDiskUsageTone(disk?.usage_percent ?? 0);
  
  // Animated number
  const animatedScore = useAnimatedNumber(healthData?.score ?? 0);
  const scoreColor = getScoreColor(healthData?.score ?? 0);

  // Triple-click counter (easter egg)
  const [clickCount, setClickCount] = useState(0);
  const clickTimerRef = useRef<number | null>(null);

  const handleTripleClick = () => {
    setClickCount(prev => prev + 1);
    
    if (clickTimerRef.current) {
      clearTimeout(clickTimerRef.current);
    }
    
    clickTimerRef.current = window.setTimeout(() => {
      setClickCount(0);
    }, 500);

    if (clickCount >= 2) {
      setClickCount(0);
      onShowWelcome?.();
    }
  };

  return (
    <div className="glass-chrome glass-header-bottom bg-[var(--bg-card)] border-b border-[var(--border-color)] px-5 py-2.5 sticky top-0 z-10">
      <div className="dashboard-container flex items-center gap-5">
        {/* Health score - WeChat-style ring progress */}
        <div 
          className="flex items-center gap-3 cursor-pointer min-w-[16.43rem]"
          onClick={handleTripleClick}
        >
          <div className={`relative w-12 h-12 rounded-full ${scoreColor.bgLight} flex items-center justify-center shrink-0`}>
            <svg className="absolute inset-0 w-full h-full -rotate-90" viewBox="0 0 100 100">
              <circle cx="50" cy="50" r="42" fill="none" stroke="currentColor" strokeWidth="5" className="text-[var(--border-color)]" />
              <circle
                cx="50" cy="50" r="42" fill="none" stroke="currentColor" strokeWidth="5" strokeLinecap="round"
                className={scoreColor.text}
                strokeDasharray={`${(healthData?.score ?? 0) * 2.64} 264`}
                style={{ transition: 'stroke-dasharray 0.8s ease-out' }}
              />
            </svg>
            <span className={`text-base font-bold ${scoreColor.text} tabular-nums`}>
              {isLoadingHealth ? '--' : animatedScore}
            </span>
          </div>
          <div>
            <div className="flex items-center gap-2">
              <span className="text-[1rem] font-bold text-[var(--text-primary)]">{t('healthScore')}</span>
              {healthData && (
                <span className={`px-2 py-0.5 rounded text-[0.79rem] font-medium ${scoreColor.bg} text-white`}>
                  {t(scoreColor.labelKey)}
                </span>
              )}
            </div>
            {healthData && (
              <div className="flex items-center gap-3 mt-1 text-[0.79rem] text-[var(--text-muted)] tabular-nums">
                <span 
                  className="flex items-center gap-1 cursor-help"
                  title={t('scoreDetails.diskSpace', { drive: disk?.drive_letter ?? 'C:' })}
                >
                  <HardDrive className="w-3.5 h-3.5" />
                  {healthData.disk_score}/40
                </span>
                <span 
                  className="flex items-center gap-1 cursor-help"
                  title={t('scoreDetails.hibernation')}
                >
                  <Moon className="w-3.5 h-3.5" />
                  {healthData.hibernation_score}/30
                </span>
                <span 
                  className="flex items-center gap-1 cursor-help"
                  title={t('scoreDetails.junk')}
                >
                  <Trash2 className="w-3.5 h-3.5" />
                  {healthData.junk_score}/30
                </span>
              </div>
            )}
          </div>
        </div>

        {/* Divider */}
        <div className="w-px h-9 bg-[var(--border-color)]" />

        {/* Disk usage: the current drive label on the left, switch + refresh icons and capacity on the right.
            Switching partitions uses a modal rather than a dropdown — the top bar is only tens of pixels tall, and a dropdown's font size / width / option truncation
            keep misaligning; in a modal each drive can fully show its volume label, free space, and usage rate. */}
        <div className="flex-1 min-w-0">
          <div className="flex items-center justify-between gap-3 mb-1.5">
            <div className="flex min-w-0 items-center gap-1.5">
              {/* Don't set a custom font size: html's root font size is calc(14px + --font-size-offset),
                  inheriting it makes it follow "Settings - Font Size". Previously text-[0.9rem] was only 12.6px at the standard level,
                  one notch smaller than body text, and hard-coded the ratio. */}
              <span className="shrink-0 font-medium text-[var(--text-primary)]">
                {t('diskSpace', { drive: disk?.drive_letter ?? 'C:' })}
              </span>
            </div>
            <div className="flex min-w-0 items-center gap-2">
              {/* Switch partition: click for a modal, don't expand a dropdown on this line */}
              <button
                type="button"
                onClick={() => setIsDiskSwitchOpen(true)}
                title={t('switchDrive')}
                aria-label={t('switchDrive')}
                className="shrink-0 rounded-lg p-1.5 text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
              >
                <ArrowLeftRight className="h-4 w-4" />
              </button>
              {/* Manual refresh goes in the right action area: the left keeps only the drive label, two icons side by side are easier to scan */}
              <button
                type="button"
                onClick={handleRefreshDrives}
                disabled={drivesLoading || isRefreshCooling}
                title={t('refreshDiskInfo')}
                aria-label={t('refreshDiskInfo')}
                className="shrink-0 rounded-lg p-1.5 text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] disabled:cursor-not-allowed disabled:opacity-60"
              >
                {/* Show a brief loading spin, to make "the button worked" visible */}
                <RefreshCw className={`h-4 w-4 ${drivesLoading ? 'animate-spin' : ''}`} />
              </button>
              {/* The colored number is exactly the free space, no "Free" label needed: the drive label already says what this row is,
                  two extra characters just squeeze the number smaller. Total capacity is bolded. */}
              {disk && (
                <span className="truncate tabular-nums">
                  {/* Free and total share the same font weight: both numbers are primary info, bolding only the total makes the first half look faint */}
                  <span className={`font-semibold ${DISK_TEXT_CLASS[usageTone]}`}>
                    {formatSize(disk.free_space)}
                  </span>
                  <span className="text-[var(--text-muted)]"> / </span>
                  <span className="font-semibold text-[var(--text-primary)]">
                    {formatSize(disk.total_space)}
                  </span>
                </span>
              )}
            </div>
          </div>
          <div className="h-2 bg-[var(--bg-hover)] rounded-full overflow-hidden">
            {disk && (
              <div
                className={`h-full rounded-full transition-all duration-500 ${DISK_BAR_CLASS[usageTone]}`}
                style={{ width: `${disk.usage_percent}%` }}
              />
            )}
          </div>
        </div>

        <DiskSwitchModal
          isOpen={isDiskSwitchOpen}
          onClose={() => setIsDiskSwitchOpen(false)}
          drives={drives}
          selectedDrive={disk?.drive_letter ?? 'C:'}
          onSelect={(driveLetter) => {
            setSelectedDrive(driveLetter);
            // Disk space is the 40-point dimension of the score: switching drives must make the backend recalculate for the new drive
            void refreshHealthScore(driveLetter);
          }}
        />

        {!hideOneClickScan && (
          <>
            {/* Divider */}
            <div className="w-px h-9 bg-[var(--border-color)]" />

            {/* One-click scan button area */}
            <div className="flex items-center gap-2">
              {isAnyScanning ? (
                <button
                  onClick={stopAllScans}
                  className="flex items-center gap-2 px-4 py-2.5 rounded-lg text-[0.93rem] font-semibold transition-all duration-200 bg-[var(--color-danger)]/20 text-[var(--color-danger)] hover:bg-[var(--color-danger)]/30"
                  title={t('stopScan')}
                >
                  <Square className="w-4 h-4" />
                  {t('stopScan')}
                </button>
              ) : (
                <button
                  onClick={onOneClickScan}
                  className="flex items-center gap-2 px-5 py-2.5 rounded-lg text-[0.93rem] font-semibold transition-all duration-200 bg-[var(--brand-green)] text-white hover:bg-[var(--brand-green-hover)] active:scale-[0.98]"
                >
                  <Zap className="w-4 h-4" />
                  {t('oneClickScan')}
                </button>
              )}
            </div>
          </>
        )}
      </div>
    </div>
  );
}

export default DashboardHeader;
