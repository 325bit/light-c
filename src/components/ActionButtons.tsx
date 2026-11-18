// ============================================================================
// Action buttons component - modern design style
// Features: gradient background, glow effect, smooth animations
// ============================================================================

import { memo } from 'react';
import { Trash2, Loader2, RefreshCw, CheckSquare, Square, Sparkles, HardDrive } from 'lucide-react';
import type { AppStatus } from '../types';
import { openDiskCleanup } from '../api/commands';
import { useTranslation } from 'react-i18next';

// ============================================================================
// Type definitions
// ============================================================================

interface ActionButtonsProps {
  /** Current app status */
  status: AppStatus;
  /** Whether a scan result exists */
  hasScanResult: boolean;
  /** Number of selected files */
  selectedCount: number;
  /** Total number of files */
  totalCount: number;
  /** Scan callback */
  onScan: () => void;
  /** Delete callback */
  onDelete: () => void;
  /** Select-all callback */
  onSelectAll: () => void;
  /** Deselect-all callback */
  onDeselectAll: () => void;
}

// ============================================================================
// Primary button component - with gradient and glow
// ============================================================================

interface PrimaryButtonProps {
  onClick: () => void;
  disabled: boolean;
  variant: 'scan' | 'delete';
  isLoading: boolean;
  children: React.ReactNode;
}

const PrimaryButton = memo(function PrimaryButton({
  onClick,
  disabled,
  variant,
  isLoading,
  children,
}: PrimaryButtonProps) {
  // Choose the color scheme based on the variant
  const colorScheme = {
    scan: {
      gradient: 'from-emerald-500 via-teal-500 to-emerald-600',
      glow: 'shadow-emerald-500/30',
      hoverGlow: 'hover:shadow-emerald-500/50',
      ring: 'focus:ring-emerald-500/50',
    },
    delete: {
      gradient: 'from-rose-500 via-red-500 to-rose-600',
      glow: 'shadow-rose-500/30',
      hoverGlow: 'hover:shadow-rose-500/50',
      ring: 'focus:ring-rose-500/50',
    },
  }[variant];

  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className={`
        relative group flex items-center gap-2 px-5 py-2.5 rounded-lg text-sm font-semibold
        transition-all duration-300 ease-out
        focus:outline-none focus:ring-2 focus:ring-offset-2 focus:ring-offset-[var(--bg-elevated)]
        ${disabled
          ? 'bg-[var(--bg-hover)] text-[var(--fg-faint)] cursor-not-allowed shadow-none'
          : `
            bg-gradient-to-r ${colorScheme.gradient}
            text-white
            shadow-lg ${colorScheme.glow}
            ${colorScheme.hoverGlow}
            hover:shadow-xl hover:-translate-y-0.5
            active:translate-y-0 active:shadow-md
            ${colorScheme.ring}
            ${isLoading ? '' : 'animate-gradient'}
          `
        }
      `}
    >
      {/* Button content */}
      <span className="relative z-10 flex items-center gap-2">
        {children}
      </span>
      
      {/* Glow effect on hover */}
      {!disabled && (
        <span className="absolute inset-0 rounded-lg bg-white/10 opacity-0 group-hover:opacity-100 transition-opacity duration-300" />
      )}
    </button>
  );
});

// ============================================================================
// Secondary button component - lightweight style
// ============================================================================

interface SecondaryButtonProps {
  onClick: () => void;
  disabled: boolean;
  icon: React.ReactNode;
  label: string;
}

const SecondaryButton = memo(function SecondaryButton({
  onClick,
  disabled,
  icon,
  label,
}: SecondaryButtonProps) {
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className={`
        flex items-center gap-1.5 px-3 py-2 rounded-lg text-xs font-medium
        transition-all duration-200
        ${disabled
          ? 'text-[var(--fg-faint)] cursor-not-allowed'
          : `
            text-[var(--fg-secondary)]
            hover:text-emerald-500 hover:bg-emerald-500/10
            active:bg-emerald-500/20
          `
        }
      `}
    >
      {icon}
      <span>{label}</span>
    </button>
  );
});

// ============================================================================
// Main component
// ============================================================================

export const ActionButtons = memo(function ActionButtons({
  status,
  hasScanResult,
  selectedCount,
  totalCount,
  onScan,
  onDelete,
  onSelectAll,
  onDeselectAll,
}: ActionButtonsProps) {
  const { t } = useTranslation('common');
  // Determine state
  const isScanning = status === 'scanning';
  const isDeleting = status === 'deleting';
  const isBusy = isScanning || isDeleting;
  const hasSelection = selectedCount > 0;
  const isAllSelected = selectedCount === totalCount && totalCount > 0;

  return (
    <div className="flex items-center gap-3">
      {/* ========== Scan button ========== */}
      <PrimaryButton
        onClick={onScan}
        disabled={isBusy}
        variant="scan"
        isLoading={isScanning}
      >
        {isScanning ? (
          <>
            <Loader2 className="w-4 h-4 animate-spin" />
            <span>{t('scanningShort')}</span>
          </>
        ) : (
          <>
            {hasScanResult ? (
              <RefreshCw className="w-4 h-4" />
            ) : (
              <Sparkles className="w-4 h-4" />
            )}
            <span>{hasScanResult ? t('rescan') : t('startScan')}</span>
          </>
        )}
      </PrimaryButton>

      {/* ========== Cleanup button ========== */}
      {hasScanResult && (
        <PrimaryButton
          onClick={onDelete}
          disabled={isBusy || !hasSelection}
          variant="delete"
          isLoading={isDeleting}
        >
          {isDeleting ? (
            <>
              <Loader2 className="w-4 h-4 animate-spin" />
              <span>{t('deleting')}</span>
            </>
          ) : (
            <>
              <Trash2 className="w-4 h-4" />
              <span>{t('cleanSelected', { count: selectedCount.toLocaleString() })}</span>
            </>
          )}
        </PrimaryButton>
      )}

      {/* ========== Divider ========== */}
      {hasScanResult && totalCount > 0 && (
        <div className="w-px h-7 bg-[var(--border-default)] mx-1" />
      )}

      {/* ========== Select all / deselect all ========== */}
      {hasScanResult && totalCount > 0 && (
        <div className="flex items-center gap-1">
          <SecondaryButton
            onClick={onSelectAll}
            disabled={isBusy || isAllSelected}
            icon={<CheckSquare className="w-3.5 h-3.5" />}
            label={t('selectAll')}
          />
          <SecondaryButton
            onClick={onDeselectAll}
            disabled={isBusy || !hasSelection}
            icon={<Square className="w-3.5 h-3.5" />}
            label={t('deselectAll')}
          />
        </div>
      )}

      {/* ========== Divider ========== */}
      <div className="w-px h-7 bg-[var(--border-default)] mx-1" />

      {/* ========== System disk cleanup ========== */}
      <SecondaryButton
        onClick={() => openDiskCleanup()}
        disabled={isBusy}
        icon={<HardDrive className="w-3.5 h-3.5" />}
        label={t('systemCleanup')}
      />
    </div>
  );
});
