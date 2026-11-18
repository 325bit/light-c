// ============================================================================
// Module card component
// A reusable expandable cleanup module card for the dashboard layout
// ============================================================================

import { ReactNode, useRef, useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, Search, CheckCircle2, AlertCircle } from 'lucide-react';
import { formatSize } from '../utils/format';
import type { ModuleStatus } from '../contexts/DashboardContext';

// ============================================================================
// Type definitions
// ============================================================================

export interface ModuleCardProps {
  /** Unique module identifier */
  id: string;
  /** Module title */
  title: string;
  /** Module description */
  description: string;
  /** Module icon */
  icon: ReactNode;
  /** Icon background color class name */
  iconBgClass?: string;
  /** Module status */
  status: ModuleStatus;
  /** Number of files found */
  fileCount: number;
  /** Total cleanable size (bytes) */
  totalSize: number;
  /** Status text when finished with results; analysis-type modules should not default to showing "cleanable". */
  doneBadgeText?: string;
  /** Status text when finished with no results, to avoid non-cleanup modules showing "cleaned". */
  emptyDoneBadgeText?: string;
  /** Hide the completion-status badge, suitable for detection-type modules to avoid showing a meaningless "detected". */
  hideDoneBadge?: boolean;
  /** Unit label for fileCount, defaults to "files". Large-directory modules may pass "large directories". */
  countLabel?: string;
  /** Show only the count, not the inapplicable space statistics. */
  hideTotalSize?: boolean;
  /** Scan button click callback */
  onScan: () => void;
  /** Scan button text */
  scanButtonText?: string;
  /** Whether the scan button is disabled */
  scanDisabled?: boolean;
  /** Expanded content */
  children: ReactNode;
  /** Extra content beside the title, suitable for a lightweight filter or mode switch. */
  titleExtra?: ReactNode;
  /** Extra content on the right side of the header */
  headerExtra?: ReactNode;
  /** Error message */
  error?: string | null;
  /** Allow sticky elements inside the module content to stay visible while the card content scrolls. */
  allowStickyContent?: boolean;
}

// ============================================================================
// Component implementation
// ============================================================================

export function ModuleCard({
  // id is reserved for future extensions (e.g. analytics)
  id: _id,
  title,
  description,
  icon,
  // Icon background color defaults to WeChat green at 10% opacity
  iconBgClass = 'bg-[var(--brand-green-10)]',
  status,
  fileCount,
  totalSize,
  doneBadgeText,
  emptyDoneBadgeText,
  hideDoneBadge = false,
  countLabel,
  hideTotalSize = false,
  onScan,
  scanButtonText,
  scanDisabled = false,
  children,
  titleExtra,
  headerExtra,
  error,
  allowStickyContent = false,
}: ModuleCardProps) {
  const { t } = useTranslation('common');
  const resolvedDoneBadgeText = doneBadgeText ?? t('cleanable');
  const resolvedEmptyDoneBadgeText = emptyDoneBadgeText ?? t('cleaned');
  const resolvedCountLabel = countLabel ?? t('files');
  const isScanning = status === 'scanning';
  const isDone = status === 'done';
  const hasError = status === 'error' || !!error;

  // Get the status label - uses the WeChat green palette
  const getStatusBadge = () => {
    if (isScanning) {
      return (
        <span className="flex items-center gap-1 px-2.5 py-1 rounded-full text-[0.79rem] font-medium bg-[var(--brand-green-10)] text-[var(--brand-green)]">
          <Loader2 className="w-3 h-3 animate-spin" />
          {t('scanningShort')}
        </span>
      );
    }
    if (hasError) {
      return (
        <span className="flex items-center gap-1 px-2.5 py-1 rounded-full text-[0.79rem] font-medium bg-[var(--color-danger)]/10 text-[var(--color-danger)]">
          <AlertCircle className="w-3 h-3" />
          {t('errorState')}
        </span>
      );
    }
    if (isDone && fileCount > 0) {
      if (hideDoneBadge) return null;
      return (
        <span className="px-2.5 py-1 rounded-full text-[0.79rem] font-medium bg-[var(--brand-green-10)] text-[var(--brand-green)]">
          {resolvedDoneBadgeText}
        </span>
      );
    }
    if (isDone && fileCount === 0) {
      if (hideDoneBadge) return null;
      return (
        <span className="flex items-center gap-1 px-2.5 py-1 rounded-full text-[0.79rem] font-medium bg-[var(--bg-hover)] text-[var(--text-muted)]">
          <CheckCircle2 className="w-3 h-3" />
          {resolvedEmptyDoneBadgeText}
        </span>
      );
    }
    return null;
  };

  // Get the scan button text
  const getButtonText = () => {
    if (scanButtonText) return scanButtonText;
    if (isScanning) return t('scanningShort');
    if (isDone) return t('rescan');
    return t('startScan');
  };

  return (
    <div 
      className={`
        /* WeChat-style card: pure white background + very subtle shadow + large rounded corners */
        glass-surface bg-[var(--bg-card)] rounded-2xl ${allowStickyContent ? 'overflow-visible' : 'overflow-hidden'}
        /* flex-1 lets the card fill the remaining height; min-height: auto keeps long results from being compressed. */
        module-card--page flex flex-1 flex-col
        shadow-sm ring-1 ring-[var(--border-color)]
        transition-all duration-300 ease-out
      `}
    >
      {/* Card header - extra padding for breathing room */}
      <div className="shrink-0 p-6">
        <div className="flex flex-wrap items-center gap-4">
          {/* Module icon - WeChat green 10% opacity rounded container */}
          <div className={`w-14 h-14 rounded-2xl ${iconBgClass} flex items-center justify-center shrink-0`}>
            {icon}
          </div>

          {/* Module info - clear text hierarchy */}
          <div className="min-w-0 flex-1 basis-[15.71rem]">
            <div className="flex min-w-0 flex-wrap items-center gap-2.5">
              <h3 className="min-w-0 text-[1.07rem] font-bold text-[var(--text-primary)]">{title}</h3>
              {titleExtra}
              {getStatusBadge()}
            </div>
            <p className="text-[0.93rem] text-[var(--text-muted)] mt-1 truncate">{description}</p>
          </div>

          {/* Statistics - tabular-nums keeps digits from jittering */}
          {isDone && fileCount > 0 && (
            <div className="text-right shrink-0 mr-3">
              {!hideTotalSize && <p className="text-xl font-bold text-[var(--brand-green)] tabular-nums">{formatSize(totalSize)}</p>}
              <p className="text-[0.93rem] text-[var(--text-muted)] tabular-nums">{fileCount.toLocaleString()} {resolvedCountLabel}</p>
            </div>
          )}

          {/* Extra content */}
          {headerExtra}

          {/* Scan button: keep the button boundary even in the done state, so "Rescan/Detect" does not look like plain text in the list. */}
          <button
            onClick={(e) => {
              e.stopPropagation();
              onScan();
            }}
            disabled={isScanning || scanDisabled}
            className={`
              flex items-center gap-2 px-5 py-2.5 rounded-xl text-[0.93rem] font-semibold transition-all duration-200 shrink-0
              border shadow-sm active:scale-[0.98]
              ${isScanning || scanDisabled
                ? 'border-[var(--border-color)] bg-[var(--bg-hover)] text-[var(--text-faint)] cursor-not-allowed shadow-none'
                : isDone
                  ? 'border-[var(--brand-green-20)] bg-[var(--brand-green-10)] text-[var(--brand-green)] hover:bg-[var(--brand-green)] hover:text-white hover:border-[var(--brand-green)] hover:shadow-[0_6px_18px_rgba(7,193,96,0.18)]'
                  : 'border-[var(--brand-green)] bg-[var(--brand-green)] text-white hover:bg-[var(--brand-green-hover)] hover:border-[var(--brand-green-hover)] hover:shadow-[0_6px_18px_rgba(7,193,96,0.22)]'
              }
            `}
          >
            {isScanning ? (
              <Loader2 className="w-4 h-4 animate-spin" />
            ) : (
              <Search className="w-4 h-4" />
            )}
            {getButtonText()}
          </button>
        </div>

        {/* Scan progress bar - WeChat green flowing-light animation */}
        {isScanning && (
          <div className="mt-5 pt-5 border-t border-[var(--border-muted)]">
            <div className="h-1.5 bg-[var(--bg-hover)] rounded-full overflow-hidden">
              <div 
                className="h-full rounded-full"
                style={{ 
                  width: '100%',
                  background: `linear-gradient(90deg, transparent, var(--brand-green-20), transparent)`,
                  backgroundSize: '200% 100%',
                  animation: 'shimmer 1.5s ease-in-out infinite'
                }} 
              />
            </div>
          </div>
        )}

        {/* Error message */}
        {hasError && error && (
          <div className="mt-4 px-4 py-3 bg-[var(--color-danger)]/10 rounded-xl">
            <p className="text-[0.93rem] text-[var(--color-danger)]">{error}</p>
          </div>
        )}
      </div>

      {/* Content area always shown: in sidebar mode the outer layer controls the current module, so the card no longer does accordion folding. */}
      <div className="flex flex-1 min-h-0 flex-col">
        <div className="border-t border-[var(--border-color)] pb-2 module-card__body flex flex-1 flex-col">
          {children}
        </div>
      </div>
    </div>
  );
}

// ============================================================================
// Accordion animation component
// ============================================================================

interface AccordionContentProps {
  expanded: boolean;
  children: ReactNode;
  animated?: boolean;
  className?: string;
}

// Exported for reuse by modules that also need accordion transitions (e.g. the tree expansion of big-directory analysis), avoiding repeated height-animation implementations everywhere.
export function AccordionContent({ expanded, children, animated = true, className = '' }: AccordionContentProps) {
  const contentRef = useRef<HTMLDivElement>(null);
  const [height, setHeight] = useState<number | 'auto'>(expanded ? 'auto' : 0);
  const [isAnimating, setIsAnimating] = useState(false);
  const prevExpandedRef = useRef(expanded);

  useEffect(() => {
    const content = contentRef.current;
    if (!content) return;

    const wasExpanded = prevExpandedRef.current;
    prevExpandedRef.current = expanded;

    if (expanded && !wasExpanded) {
      // Expanding: set the actual height first, then set to auto after the animation ends
      const scrollHeight = content.scrollHeight;
      setHeight(scrollHeight);
      setIsAnimating(true);
      
      const timer = setTimeout(() => {
        setHeight('auto');
        setIsAnimating(false);
      }, 300);
      
      return () => clearTimeout(timer);
    } else if (!expanded && wasExpanded) {
      // Collapsing: first capture the current height, then animate to 0
      const scrollHeight = content.scrollHeight;
      // Set a concrete height first (converting from auto)
      setHeight(scrollHeight);
      setIsAnimating(true);
      
      // Use requestAnimationFrame to ensure the browser renders the concrete height first
      requestAnimationFrame(() => {
        requestAnimationFrame(() => {
          setHeight(0);
        });
      });
      
      const timer = setTimeout(() => {
        setIsAnimating(false);
      }, 300);
      
      return () => clearTimeout(timer);
    }
  }, [expanded]);

  const shouldRender = expanded || height !== 0 || isAnimating;

  if (!shouldRender) return null;

  if (!animated) {
    // Page mode and floating-action modules skip the height animation, to avoid overflow rules clipping sticky children.
    return expanded ? <div ref={contentRef} className={className}>{children}</div> : null;
  }

  return (
    <div
      ref={contentRef}
      className={className}
      style={{
        height: typeof height === 'number' ? `${height}px` : height,
        overflow: 'hidden',
        transition: 'height 300ms cubic-bezier(0.4, 0, 0.2, 1)',
      }}
    >
      {children}
    </div>
  );
}

export default ModuleCard;
