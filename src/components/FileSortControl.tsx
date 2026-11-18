// ============================================================================
// 文件列表排序控件（名称 / 大小 / 修改时间）
//
// 中文说明：垃圾清理的「查看全部文件」弹窗先做了这套排序，社交专清、AI 工具清理的
// 「全部文件」也需要同样的操作。与其每个弹窗各写一遍按钮、各写一遍升降序切换，
// 不如收成一个组件 —— 三个入口的视觉、语义（点同一键切方向、点别的键用该项默认方向）
// 从此只有一份实现。
// ============================================================================

import { ArrowDown, ArrowUp } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { DEFAULT_SORT_DESC, type TreeSort, type TreeSortKey } from '../utils/fileTree';

/** 排序键的默认展示顺序：名称 / 大小 / 修改时间 */
export const FILE_SORT_KEYS: TreeSortKey[] = ['name', 'size', 'time'];

/**
 * 点同一个键切换升/降序，点别的键换成那个键的默认方向
 * （大小和时间默认从大到小/从新到旧，名称默认从小到大）。
 */
export function nextSort(previous: TreeSort, key: TreeSortKey): TreeSort {
  return previous.key === key
    ? { key, desc: !previous.desc }
    : { key, desc: DEFAULT_SORT_DESC[key] };
}

export interface FileSortControlProps {
  sort: TreeSort;
  /** 结算后的目标排序；组件内部已经算好方向，调用方只管接收 */
  onSortChange: (next: TreeSort) => void;
  /**
   * 这批数据真正能用的排序键。
   *
   * 中文说明：数据源没有修改时间时不要摆一个「修改时间」按钮 —— 点了整列都是
   * 「—」，用户只会以为排序坏了。默认三个都给。
   */
  keys?: TreeSortKey[];
  className?: string;
}

export function FileSortControl({
  sort,
  onSortChange,
  keys = FILE_SORT_KEYS,
  className = '',
}: FileSortControlProps) {
  const { t } = useTranslation('common');

  const labelOf = (key: TreeSortKey) => {
    if (key === 'size') return t('sortBySize');
    if (key === 'time') return t('sortByTime');
    return t('sortByName');
  };

  return (
    <div className={`flex shrink-0 items-center gap-0.5 rounded-lg bg-[var(--bg-hover)] p-0.5 ${className}`}>
      {keys.map((key) => {
        const active = sort.key === key;
        return (
          <button
            key={key}
            type="button"
            onClick={() => onSortChange(nextSort(sort, key))}
            title={active
              ? (sort.desc ? t('sortDescendingHint') : t('sortAscendingHint'))
              : t('sortSwitchHint')}
            className={`flex items-center gap-0.5 rounded-md px-1.5 py-0.5 text-[0.79rem] transition
              ${active
                ? 'bg-[var(--bg-card)] font-medium text-[var(--brand-green)] shadow-sm'
                : 'text-[var(--text-muted)] hover:text-[var(--brand-green)]'}`}
          >
            {labelOf(key)}
            {active && (sort.desc
              ? <ArrowDown className="h-3 w-3" />
              : <ArrowUp className="h-3 w-3" />)}
          </button>
        );
      })}
    </div>
  );
}
