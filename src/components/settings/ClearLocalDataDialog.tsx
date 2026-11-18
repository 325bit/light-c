// ============================================================================
// Local data cleanup confirmation dialog
// ============================================================================

import { createPortal } from 'react-dom';
import { AnimatePresence, motion } from 'framer-motion';
import { CheckCircle, RefreshCw, Trash2, X } from 'lucide-react';
import type { ClearableDataItem } from '../../api/commands';
import { formatSize } from '../../utils/format';
import { MODAL_BACKDROP_MOTION, MODAL_CARD_MOTION } from '../../utils/modalMotion';
import { useTranslation } from 'react-i18next';

const DATA_ITEM_TRANSLATION_KEYS: Record<string, string> = {
  install_history: 'clearDataItems.installHistory',
  logs: 'clearDataItems.logs',
  reg_backups: 'clearDataItems.registryBackups',
  registry_redundancy_backups: 'clearDataItems.registryRedundancyBackups',
  shell_icon_logs: 'clearDataItems.shellIconLogs',
  shell_icon_backups: 'clearDataItems.shellIconBackups',
  driver_backups: 'clearDataItems.driverBackups',
};

// Map translations by stable backend IDs, avoiding passing the current language state into data-dir and cleanup logic.
function localizeDataItem(item: ClearableDataItem, translate: (key: string, options?: Record<string, unknown>) => string) {
  const fixedKey = DATA_ITEM_TRANSLATION_KEYS[item.id];
  const snapshotPrefix = 'disk_growth_snapshots_';
  const drive = item.id.startsWith(snapshotPrefix)
    ? item.id.slice(snapshotPrefix.length).toUpperCase()
    : undefined;
  const translationKey = fixedKey ?? (drive ? 'clearDataItems.diskGrowthSnapshot' : undefined);

  if (!translationKey) {
    return { label: item.id, description: '', warning: undefined };
  }

  const options = drive ? { drive } : undefined;
  return {
    label: translate(`${translationKey}.label`, options),
    description: translate(`${translationKey}.description`, options),
    warning: translate(`${translationKey}.warning`, options),
  };
}

export function ClearLocalDataDialog({
  isOpen,
  items,
  selectedIds,
  isClearing,
  onToggleItem,
  onCancel,
  onConfirm,
}: {
  isOpen: boolean;
  items: ClearableDataItem[];
  selectedIds: string[];
  isClearing: boolean;
  onToggleItem: (itemId: string) => void;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const { t } = useTranslation('settings');
  const selectedItems = items.filter(item => selectedIds.includes(item.id));
  const selectedSize = selectedItems.reduce((sum, item) => sum + item.size, 0);
  const selectedFileCount = selectedItems.reduce((sum, item) => sum + item.file_count, 0);

  return createPortal(
    <AnimatePresence>
      {isOpen && (
        // Cleanup confirmation interrupts the user's flow; enter/exit animations reduce the jarring feel of sudden appearance/disappearance.
        <motion.div
          className="fixed inset-0 z-[10050] flex items-center justify-center"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.18 }}
        >
          <motion.div
            className="absolute inset-0 bg-black/50 backdrop-blur-sm glass-backdrop"
            onClick={onCancel}
            {...MODAL_BACKDROP_MOTION}
          />
          {/* Sizes all use em: body inherits the :root font size, so the dialog scales with the global font setting, 37.14em ≈ 520px.
              Height must also be clamped by the viewport: previously only the list's own 41.43em cap existed, and in a short window the whole dialog overflowed top and bottom.
              Same approach as the social-cleanup file-list dialog: capped shell + list with flex-1 scrolling itself. */}
          <motion.div
            className="relative flex w-[37.14em] max-w-[calc(100vw-2em)] max-h-[80vh] flex-col overflow-hidden rounded-2xl border border-[var(--border-color)] bg-[var(--bg-card)] glass-dialog shadow-2xl"
            {...MODAL_CARD_MOTION}
          >
            <div className="flex items-center justify-between border-b border-[var(--border-color)] px-[1.43em] py-[1.14em] shrink-0">
              <div className="flex items-center gap-[0.86em]">
                <div className="flex h-[2.86em] w-[2.86em] items-center justify-center rounded-full bg-[var(--color-danger)]/10">
                  <Trash2 className="h-[1.43em] w-[1.43em] text-[var(--color-danger)]" />
                </div>
                <div>
                  <h3 className="text-[1.14em] font-semibold text-[var(--text-primary)]">{t('clearData.title')}</h3>
                  <p className="text-[0.86em] text-[var(--text-muted)]">{t('clearData.subtitle')}</p>
                </div>
              </div>
              <button
                onClick={onCancel}
                className="rounded-lg p-[0.43em] text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] transition"
              >
                <X className="h-[1.14em] w-[1.14em]" />
              </button>
            </div>

            {/* flex-1: shrinks within the shell's max-h; 41.43em is the list-height cap on wide screens, preventing the dialog from growing indefinitely */}
            <div className="max-h-[41.43em] flex-1 overflow-y-auto px-[1.43em] py-[1.14em]">
              <div className="mb-[0.86em] rounded-xl border border-amber-500/20 bg-amber-500/10 p-[0.86em]">
                <p className="text-[0.86em] leading-relaxed text-amber-700 dark:text-amber-300">
                  {t('clearData.warning')}
                </p>
              </div>

              <div className="space-y-[0.57em]">
                {items.map(item => {
                  const selected = selectedIds.includes(item.id);
                  const disabled = !item.exists || item.file_count === 0;
                  const localizedItem = localizeDataItem(item, t);

                  return (
                    <button
                      key={item.id}
                      type="button"
                      disabled={disabled}
                      onClick={() => onToggleItem(item.id)}
                      className={`w-full rounded-xl border p-[0.86em] text-left transition ${
                        selected
                          ? 'border-[var(--brand-green)] bg-[var(--brand-green)]/10'
                          : 'border-[var(--border-color)] bg-[var(--bg-main)] hover:border-[var(--brand-green)]/30'
                      } ${disabled ? 'cursor-not-allowed opacity-55' : ''}`}
                    >
                      <div className="flex items-start gap-[0.86em]">
                        {/* The check mark uses em to follow font size, consistent with the look of ui/Checkbox */}
                        <span className={`mt-[0.14em] flex h-[1.14em] w-[1.14em] shrink-0 items-center justify-center rounded border ${
                          selected ? 'border-[var(--brand-green)] bg-[var(--brand-green)]' : 'border-[var(--border-color)]'
                        }`}>
                          {selected && <CheckCircle className="h-[0.86em] w-[0.86em] text-white" />}
                        </span>
                        <div className="min-w-0 flex-1">
                          <div className="flex items-center justify-between gap-[0.86em]">
                            <p className="truncate text-[1em] font-semibold text-[var(--text-primary)]">{localizedItem.label}</p>
                            <span className="shrink-0 text-[0.86em] font-semibold tabular-nums text-[var(--brand-green)]">
                              {formatSize(item.size)}
                            </span>
                          </div>
                          <p className="mt-[0.29em] text-[0.86em] leading-relaxed text-[var(--text-muted)]">{localizedItem.description}</p>
                          <p className="mt-[0.29em] truncate text-[0.79em] text-[var(--text-faint)]" title={item.path}>
                            {item.item_type === 'directory' ? t('clearData.directory') : t('clearData.file')} · {t('clearData.fileCount', { count: item.file_count.toLocaleString() })} · {item.path}
                          </p>
                          {localizedItem.warning && (
                            <p className="mt-[0.57em] text-[0.79em] leading-relaxed text-amber-600 dark:text-amber-400">
                              {localizedItem.warning}
                            </p>
                          )}
                        </div>
                      </div>
                    </button>
                  );
                })}
              </div>
            </div>

            <div className="glass-bar flex items-center justify-between gap-[0.86em] border-t border-[var(--border-color)] bg-[var(--bg-main)] px-[1.43em] py-[1.14em] shrink-0">
              <p className="text-[0.86em] text-[var(--text-muted)]">
                {t('clearData.summary', { count: selectedFileCount.toLocaleString(), size: formatSize(selectedSize) })}
              </p>
              <div className="flex items-center gap-[0.57em]">
                <button
                  onClick={onCancel}
                  className="rounded-lg px-[1.14em] py-[0.57em] text-[1em] font-medium text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] transition"
                >
                  {t('clearData.cancel')}
                </button>
                <button
                  onClick={onConfirm}
                  disabled={isClearing || selectedIds.length === 0}
                  className="inline-flex items-center gap-[0.57em] rounded-lg bg-[var(--color-danger)] px-[1.14em] py-[0.57em] text-[1em] font-semibold text-white transition hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-50"
                >
                  {isClearing && <RefreshCw className="h-[1.14em] w-[1.14em] animate-spin" />}
                  {t('clearData.confirm')}
                </button>
              </div>
            </div>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>,
    document.body
  );
}
