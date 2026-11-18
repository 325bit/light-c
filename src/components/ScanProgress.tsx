// ============================================================================
// Scan progress component - centered modal style
// Features: fixed centered display, shows results when scanning completes and disappears after a delay
// ============================================================================

import { useEffect, useState, useRef, memo } from 'react';
import { createPortal } from 'react-dom';
import { CheckCircle, FolderSearch, FileText, HardDrive } from 'lucide-react';
import { useTranslation } from 'react-i18next';

// ============================================================================
// Constant configuration
// ============================================================================

/** Duration to show the completed state (milliseconds) */
const COMPLETION_DISPLAY_DURATION = 2000;
/** Exit animation duration (milliseconds) */
const EXIT_ANIMATION_DURATION = 300;

// ============================================================================
// Type definitions
// ============================================================================

/** Component internal state */
type ProgressState = 'idle' | 'scanning' | 'completing' | 'exiting';

interface ScanProgressProps {
  /** Whether a scan is in progress */
  isScanning: boolean;
  /** Name of the category currently being scanned */
  currentCategory?: string;
  /** Number of completed categories */
  completedCategories?: number;
  /** Total number of categories */
  totalCategories?: number;
  /** Number of files scanned so far */
  scannedFileCount?: number;
  /** Size of files scanned so far */
  scannedSize?: number;
}

// ============================================================================
// Utility functions
// ============================================================================

/**
 * Formats a file size
 */
function formatSize(bytes: number): string {
  if (bytes === 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  const k = 1024;
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return `${(bytes / Math.pow(k, i)).toFixed(2)} ${units[i]}`;
}

// ============================================================================
// Ring progress bar component
// ============================================================================

interface CircularProgressProps {
  progress: number;
  size?: number;
  strokeWidth?: number;
  isCompleted?: boolean;
  isIndeterminate?: boolean;
}

const CircularProgress = memo(function CircularProgress({ 
  progress, 
  size = 80, 
  strokeWidth = 6,
  isCompleted = false,
  isIndeterminate = false,
}: CircularProgressProps) {
  const radius = (size - strokeWidth) / 2;
  const circumference = radius * 2 * Math.PI;
  const offset = isIndeterminate 
    ? circumference * 0.75  // show 25% of the arc for indeterminate progress
    : circumference - (progress / 100) * circumference;

  return (
    <div className="relative" style={{ width: size, height: size }}>
      {/* Background ring */}
      <svg 
        className={`transform -rotate-90 ${isIndeterminate ? 'animate-spin' : ''}`}
        style={isIndeterminate ? { animationDuration: '1.5s' } : undefined}
        width={size} 
        height={size}
      >
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius}
          fill="none"
          stroke="var(--bg-hover)"
          strokeWidth={strokeWidth}
        />
        {/* Progress ring */}
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius}
          fill="none"
          stroke={isCompleted ? '#10b981' : 'url(#progressGradient)'}
          strokeWidth={strokeWidth}
          strokeLinecap="round"
          strokeDasharray={circumference}
          strokeDashoffset={offset}
          className={isIndeterminate ? '' : 'transition-all duration-500 ease-out'}
        />
        <defs>
          <linearGradient id="progressGradient" x1="0%" y1="0%" x2="100%" y2="0%">
            <stop offset="0%" stopColor="#10b981" />
            <stop offset="100%" stopColor="#14b8a6" />
          </linearGradient>
        </defs>
      </svg>
      {/* Center content */}
      <div className="absolute inset-0 flex items-center justify-center">
        {isCompleted ? (
          <CheckCircle className="w-8 h-8 text-emerald-500" />
        ) : isIndeterminate ? (
          <FolderSearch className="w-6 h-6 text-emerald-500 animate-pulse" />
        ) : (
          <span className="text-lg font-bold text-emerald-500">{Math.round(progress)}%</span>
        )}
      </div>
    </div>
  );
});

// ============================================================================
// Main component
// ============================================================================

export function ScanProgress({
  isScanning,
  currentCategory,
  completedCategories = 0,
  totalCategories = 10,
  scannedFileCount = 0,
  scannedSize = 0,
}: ScanProgressProps) {
  const { t } = useTranslation('common');
  const resolvedCategory = currentCategory ?? t('preparing');
  // Component internal state management
  const [state, setState] = useState<ProgressState>('idle');
  const [isVisible, setIsVisible] = useState(false);
  
  // Save the scan data at completion
  const completedDataRef = useRef({
    scannedFileCount: 0,
    scannedSize: 0,
    totalCategories: 10,
  });
  
  // Record the previous scan state
  const prevIsScanningRef = useRef(false);

  // State machine: handles scan state changes
  useEffect(() => {
    const wasScanning = prevIsScanningRef.current;
    prevIsScanningRef.current = isScanning;
    
    if (isScanning && !wasScanning) {
      // Scan just started
      setState('scanning');
      requestAnimationFrame(() => {
        requestAnimationFrame(() => {
          setIsVisible(true);
        });
      });
    } else if (!isScanning && wasScanning) {
      // Scan just ended
      completedDataRef.current = {
        scannedFileCount,
        scannedSize,
        totalCategories,
      };
      setState('completing');
      
      // Start exiting after the completed state is shown
      const completionTimer = setTimeout(() => {
        setIsVisible(false);
        setState('exiting');
        
        setTimeout(() => {
          setState('idle');
        }, EXIT_ANIMATION_DURATION);
      }, COMPLETION_DISPLAY_DURATION);
      
      return () => clearTimeout(completionTimer);
    }
  }, [isScanning, scannedFileCount, scannedSize, totalCategories]);

  // Return null when not rendering
  if (state === 'idle') return null;

  const isCompleted = state === 'completing' || state === 'exiting';
  const displayFileCount = isCompleted ? completedDataRef.current.scannedFileCount : scannedFileCount;
  const displaySize = isCompleted ? completedDataRef.current.scannedSize : scannedSize;
  const displayTotal = isCompleted ? completedDataRef.current.totalCategories : totalCategories;
  // -1 means indeterminate progress (scanning), use a simulated animated progress
  const isIndeterminate = completedCategories < 0 && !isCompleted;
  const progress = isCompleted ? 100 : (isIndeterminate ? 0 : (totalCategories > 0 ? (completedCategories / totalCategories) * 100 : 0));

  return createPortal(
    <div 
      className={`
        fixed inset-0 z-[9999] flex items-center justify-center
        transition-all duration-300 ease-out
        ${isVisible ? 'opacity-100' : 'opacity-0 pointer-events-none'}
      `}
    >
      {/* Semi-transparent overlay */}
      <div className="absolute inset-0 bg-black/30 backdrop-blur-sm glass-backdrop" />
      
      {/* Modal content */}
      <div 
        className={`
          glass-overlay relative bg-[var(--bg-elevated)] rounded-2xl glass-dialog shadow-2xl 
          border border-[var(--border-default)] p-6 w-80
          transition-all duration-300 ease-out
          ${isVisible ? 'scale-100 translate-y-0' : 'scale-95 translate-y-4'}
          ${isCompleted ? 'border-emerald-500/30' : ''}
        `}
      >
        {/* Top: progress ring */}
        <div className="flex flex-col items-center mb-5">
          <CircularProgress progress={progress} isCompleted={isCompleted} isIndeterminate={isIndeterminate} />
          
          {/* Status text */}
          <h3 className="mt-4 text-base font-semibold text-[var(--fg-primary)]">
            {isCompleted ? t('scanCompleted') : t('scanning')}
          </h3>
          <p className="text-xs text-[var(--fg-muted)] mt-1">
            {isCompleted ? t('allCategoriesCompleted') : resolvedCategory}
          </p>
        </div>

        {/* Statistics cards */}
        <div className="grid grid-cols-3 gap-2">
          <div className="glass-block bg-[var(--bg-card)] rounded-lg p-3 text-center">
            <FileText className="w-4 h-4 text-emerald-500 mx-auto mb-1" />
            <p className="text-sm font-semibold text-[var(--fg-primary)]">
              {displayFileCount.toLocaleString()}
            </p>
            <p className="text-[0.71rem] text-[var(--fg-muted)]">{t('fileCountLabel')}</p>
          </div>
          
          <div className="glass-block bg-[var(--bg-card)] rounded-lg p-3 text-center">
            <HardDrive className="w-4 h-4 text-teal-500 mx-auto mb-1" />
            <p className="text-sm font-semibold text-[var(--fg-primary)]">
              {formatSize(displaySize)}
            </p>
            <p className="text-[0.71rem] text-[var(--fg-muted)]">{t('cleanable')}</p>
          </div>
          
          <div className="glass-block bg-[var(--bg-card)] rounded-lg p-3 text-center">
            <FolderSearch className="w-4 h-4 text-amber-500 mx-auto mb-1" />
            <p className="text-sm font-semibold text-[var(--fg-primary)]">
              {isCompleted ? displayTotal : completedCategories}/{displayTotal}
            </p>
            <p className="text-[0.71rem] text-[var(--fg-muted)]">{t('categoryCountLabel')}</p>
          </div>
        </div>

        {/* Animated indicator while scanning */}
        {!isCompleted && (
          <div className="mt-4 flex justify-center">
            <div className="flex gap-1">
              <span className="w-2 h-2 bg-emerald-500 rounded-full animate-bounce" style={{ animationDelay: '0ms' }} />
              <span className="w-2 h-2 bg-emerald-400 rounded-full animate-bounce" style={{ animationDelay: '150ms' }} />
              <span className="w-2 h-2 bg-teal-400 rounded-full animate-bounce" style={{ animationDelay: '300ms' }} />
            </div>
          </div>
        )}
      </div>
    </div>,
    document.body
  );
}

export default ScanProgress;
