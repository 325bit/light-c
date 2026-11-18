/**
 * Badge for "this path is actually a link" + the real landing point.
 *
 * Background: after moving a directory to another drive with a tool like FolderMove, a junction / symlink is left at the original location.
 * The scanner reads the real files through the link, but still displays the original drive letter - users would think drive C is occupied by that much,
 * and cannot tell the directory was moved away long ago. Two modules (AI tool cleanup / dev-space cleanup) share this display logic,
 * so a change in one place is not forgotten in the other.
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
      /* Hover shows the full chain: the link itself → target → final landing point, so troubleshooting is clear at a glance */
      title={t('link.tooltip', { from: link.linkPath, to: link.target, resolved: link.resolved })}
    >
      <CornerUpRight className="h-3 w-3" />
      {label}
    </span>
  );
}

/**
 * Replace the "display path" with the "real landing point" - only when there is a link.
 *
 * Returns the original path when there is no link, so the caller does not need to check for null.
 */
export function displayPathOf(path: string, link?: LinkInfo): string {
  return link?.resolved || path;
}
