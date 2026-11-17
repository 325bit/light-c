/**
 * 「这条路径其实是个链接」的徽标 + 真实落点。
 *
 * 背景：用 FolderMove 之类把目录搬到别的盘之后，原位置留下的是 junction / symlink。
 * 扫描器沿链接读到的是真实文件，但显示的仍是原来的盘符 —— 用户会以为 C 盘被占了那么多，
 * 也看不出这个目录早就搬走了。两个模块（AI 工具清理 / 开发空间清理）共用这一份显示逻辑，
 * 免得一处改了另一处忘了。
 */
import { useTranslation } from 'react-i18next';
import { CornerUpRight } from 'lucide-react';
import type { LinkInfo } from '../types/devCache';

export function LinkBadge({ link }: { link?: LinkInfo }) {
  const { t } = useTranslation('modules');
  if (!link) return null;

  const label = link.kind === 'junction' ? t('link.junction') : t('link.symlink');

  return (
    <span
      className="mr-1.5 inline-flex shrink-0 items-center gap-0.5 rounded bg-[var(--brand-green-10)] px-1.5 py-0.5 align-middle text-[0.72rem] font-medium text-[var(--brand-green)]"
      /* 悬停给出完整链条：链接本身 → 目标 → 最终落点，排查时一眼能看清 */
      title={t('link.tooltip', { from: link.linkPath, to: link.target, resolved: link.resolved })}
    >
      <CornerUpRight className="h-3 w-3" />
      {label}
    </span>
  );
}

/**
 * 把「显示路径」换成「真实落点」——只在有链接时才换。
 *
 * 没链接时返回原始路径，调用方不用自己判空。
 */
export function displayPathOf(path: string, link?: LinkInfo): string {
  return link?.resolved || path;
}
