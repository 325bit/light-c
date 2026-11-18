import type {
  DiskGrowthAnalyzeEntry,
  DiskGrowthEntry,
  DiskGrowthExportNode,
  DiskGrowthReport,
  DiskGrowthScanResponse,
} from '../api/commands';
import { formatSize } from './format';

export interface DiskGrowthHtmlLabels {
  title: string;
  generatedAt: string;
  drive: string;
  scanMode: string;
  changeMode: string;
  baselineMode: string;
  currentSize: string;
  netChange: string;
  noHistory: string;
  previousScan: string;
  currentScan: string;
  scannedFiles: string;
  resultCount: string;
  truncatedNote: string;
  path: string;
  changeTime: string;
  level: string;
  size: string;
  difference: string;
  previousSize: string;
  children: string;
  explanation: string;
  suggestion: string;
  noResult: string;
  depthNote: string;
  scopeTitle: string;
  changeScopeNote: string;
  baselineScopeNote: string;
  levels: Record<DiskGrowthEntry['level'], string>;
}

interface DiskGrowthHtmlOptions {
  labels: DiskGrowthHtmlLabels;
  locale: string;
  exportTotalNodes?: number;
  exportTruncated?: boolean;
}

function escapeHtml(value: string): string {
  // Scan paths and backend descriptions may contain special characters; escape them before embedding in HTML so the report structure isn't broken.
  const entities: Record<string, string> = {
    '&': '&amp;',
    '<': '&lt;',
    '>': '&gt;',
    "'": '&#39;',
    '"': '&quot;',
  };
  return value.replace(/[&<>'"]/g, (character) => entities[character]);
}

function formatDateTime(timestamp: number, locale: string): string {
  if (!timestamp) return '-';
  const normalizedTimestamp = timestamp < 10_000_000_000 ? timestamp * 1000 : timestamp;
  const date = new Date(normalizedTimestamp);
  return Number.isNaN(date.getTime())
    ? '-'
    : new Intl.DateTimeFormat(locale || undefined, { dateStyle: 'medium', timeStyle: 'short' }).format(date);
}

function formatSignedSize(diff: number): string {
  if (diff === 0) return '-';
  return `${diff > 0 ? '+' : '-'}${formatSize(Math.abs(diff))}`;
}

function interpolateHtmlLabel(template: string, values: Record<string, string | number>): string {
  // Report descriptions need actual item counts inserted; escape interpolated content uniformly so paths or external text can't break the HTML.
  return template.replace(/\{\{(\w+)\}\}/g, (_match, key: string) => escapeHtml(String(values[key] ?? '')));
}

function renderNode(
  node: DiskGrowthExportNode,
  labels: DiskGrowthHtmlLabels,
  locale: string,
  parts: string[],
): void {
  const hasChildren = node.children.length > 0;
  const diffClass = node.diff >= 0 ? 'increase' : 'decrease';
  // Only the root node is expanded by default: there can be thousands of nodes, and expanding all would severely stall first-paint layout in the browser.
  parts.push(`<details class="node"${hasChildren ? ' open' : ''}><summary><span>`);
  parts.push(escapeHtml(node.name));
  parts.push(`</span><strong class="${diffClass}">`);
  parts.push(escapeHtml(formatSignedSize(node.diff)));
  // Details use dl/dt/dd, one less wrapper than div + span: 5 fewer DOM elements per node,
  // which directly affects browser parsing and layout cost at thousands of nodes, while carrying exactly the same information.
  parts.push('</strong></summary><div class="node-content"><dl class="node-details">');
  parts.push(detailRow(labels.path, `<code>${escapeHtml(node.path)}</code>`));
  parts.push(detailRow(labels.previousSize, `<strong>${escapeHtml(formatSize(node.old_size))}</strong>`));
  parts.push(detailRow(labels.currentSize, `<strong>${escapeHtml(formatSize(node.new_size))}</strong>`));
  parts.push(detailRow(labels.difference, `<strong class="${diffClass}">${escapeHtml(formatSignedSize(node.diff))}</strong>`));
  parts.push(detailRow(labels.changeTime, escapeHtml(formatDateTime(node.modified, locale))));
  parts.push(detailRow(labels.level, `<span class="badge">${escapeHtml(labels.levels[node.level] || node.level)}</span>`));
  parts.push('</dl>');

  if (hasChildren) {
    parts.push('<div class="children">');
    for (const child of node.children) {
      renderNode(child, labels, locale, parts);
    }
    parts.push('</div>');
  }

  parts.push('</div></details>');
}

/// Generate one "label + value" detail row; valueHtml must already be an escaped HTML fragment.
function detailRow(label: string, valueHtml: string): string {
  return `<dt>${escapeHtml(label)}</dt><dd>${valueHtml}</dd>`;
}

function renderBaselineEntry(entry: DiskGrowthAnalyzeEntry, labels: DiskGrowthHtmlLabels, locale: string): string {
  return `<details class="node" open>
    <summary><span>${escapeHtml(entry.path)}</span><strong>${escapeHtml(formatSize(entry.size))}</strong></summary>
    <div class="node-content"><dl class="node-details">
      <dt>${escapeHtml(labels.path)}</dt><dd><code>${escapeHtml(entry.path)}</code></dd>
      <dt>${escapeHtml(labels.currentSize)}</dt><dd><strong>${escapeHtml(formatSize(entry.size))}</strong></dd>
      <dt>${escapeHtml(labels.changeTime)}</dt><dd>${escapeHtml(formatDateTime(entry.modified, locale))}</dd>
      <dt>${escapeHtml(labels.explanation)}</dt><dd>${escapeHtml(entry.reason)}</dd>
      <dt>${escapeHtml(labels.suggestion)}</dt><dd>${escapeHtml(entry.suggestion)}</dd>
    </dl></div>
  </details>`;
}

export function buildDiskGrowthHtml(
  scanSummary: DiskGrowthScanResponse,
  growthReport: DiskGrowthReport,
  exportNodes: DiskGrowthExportNode[],
  options: DiskGrowthHtmlOptions,
): string {
  const { labels, locale, exportTotalNodes, exportTruncated = false } = options;
  // Whether a historical snapshot exists decides the report mode; even a second scan with no changes must not fall back to the first-scan baseline.
  const isBaselineReport = !scanSummary.previous_scan_time;
  // Accumulate in an array and join once: recursive template strings copy each subtree's lower results again for every level,
  // producing noticeable intermediate-string overhead at thousands of nodes.
  const parts: string[] = [];
  if (!isBaselineReport) {
    for (const node of exportNodes) {
      renderNode(node, labels, locale, parts);
    }
  } else {
    for (const entry of scanSummary.analyze.entries) {
      parts.push(renderBaselineEntry(entry, labels, locale));
    }
  }
  const content = parts.length > 0 ? parts.join('') : `<p class="empty">${escapeHtml(labels.noResult)}</p>`;
  const previousScan = scanSummary.previous_scan_time || labels.noHistory;
  // The change report uses the node count actually generated by the backend, to avoid mistaking the deduped root-directory count for the export count.
  const resultCount = !isBaselineReport
    ? (exportTotalNodes ?? exportNodes.length)
    : scanSummary.analyze.entries.length;
  const footerNote = exportTruncated
    ? `${labels.depthNote} ${labels.truncatedNote}`
    : labels.depthNote;
  const scopeNote = interpolateHtmlLabel(
    isBaselineReport ? labels.baselineScopeNote : labels.changeScopeNote,
    {
      pageCount: isBaselineReport ? scanSummary.analyze.entries.length : growthReport.entries.length,
      exportCount: resultCount,
    },
  );

  return `<!doctype html>
<html lang="${escapeHtml(locale)}">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>${escapeHtml(labels.title)}</title>
  <style>
    :root { color-scheme: light dark; --bg: #f4faf7; --card: #fff; --text: #1f2937; --muted: #5f7168; --border: #cfe6d9; --accent: #07c160; --accent-hover: #06ad56; --soft: rgba(7, 193, 96, .10); --increase: #ef4444; --decrease: #07c160; }
    @media (prefers-color-scheme: dark) { :root { --bg: #111a15; --card: #1b2820; --text: #edf5f1; --muted: #a7b8ae; --border: #365443; --soft: rgba(7, 193, 96, .18); } }
    * { box-sizing: border-box; } ::selection { background: var(--accent); color: #fff; } body { margin: 0; padding: 32px 20px 48px; background: var(--bg); color: var(--text); font: 14px/1.5 "Segoe UI", "Microsoft YaHei", sans-serif; }
    main { max-width: 1160px; margin: 0 auto; } h1 { margin: 0 0 20px; color: var(--accent-hover); font-size: 26px; }
    .meta { display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 10px; margin-bottom: 24px; }
    .meta-item { padding: 13px 15px; background: var(--card); border: 1px solid var(--border); border-radius: 10px; min-width: 0; }
    .meta-item span { display: block; color: var(--muted); font-size: 12px; } .meta-item strong { display: block; margin-top: 3px; overflow-wrap: anywhere; }
    .node { margin: 8px 0; background: var(--card); border: 1px solid var(--border); border-radius: 10px; overflow: hidden; }
    .node summary { display: flex; justify-content: space-between; gap: 16px; padding: 13px 16px; cursor: pointer; list-style-position: inside; } .node summary::marker { color: var(--accent); } .node summary:hover { background: var(--soft); } .node summary:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
    .node summary span { overflow-wrap: anywhere; } .node summary strong { flex: 0 0 auto; }
    /* The collapse animation is entirely CSS-driven: interpolate-size lets height:auto interpolate with 0,
       so there's no need to bind a toggle listener to every node (which would stall the browser main thread for a long time at thousands of nodes).
       Older browsers without that property just have no transition; expand/collapse itself still works. */
    .node-content { height: auto; overflow: hidden; opacity: 1; interpolate-size: allow-keywords; transition: height 220ms ease, opacity 180ms ease; }
    .node:not([open]) > .node-content { height: 0; opacity: 0; }
    .node-details { display: grid; grid-template-columns: repeat(auto-fit, minmax(210px, 1fr)); gap: 10px 16px; margin: 0; padding: 0 16px 14px 34px; }
    /* dl uses a two-column grid: dt fixed in the left column, dd in the right and auto-wrapping, each pair on one row, no field wrapper needed. */
    .node-details { display: grid; grid-template-columns: minmax(96px, max-content) minmax(0, 1fr); column-gap: 14px; row-gap: 6px; margin: 0; padding: 0 16px 14px 34px; }
    .node-details dt { grid-column: 1; color: var(--muted); font-size: 12px; min-width: 0; }
    .node-details dd { grid-column: 2; margin: 0; min-width: 0; overflow-wrap: anywhere; }
    .node-details dd code { font: 12px/1.45 Consolas, monospace; }
    .children { margin: 0 12px 12px 28px; padding-left: 12px; border-left: 2px solid var(--border); }
    .badge { display: inline-block; padding: 2px 8px; border-radius: 999px; color: var(--accent); background: var(--soft); }
    .increase { color: var(--increase); } .decrease { color: var(--decrease); } .empty { padding: 24px; color: var(--muted); background: var(--card); border: 1px solid var(--border); border-radius: 10px; } footer { margin-top: 24px; color: var(--muted); font-size: 12px; }
    .scope-note { margin: 0 0 24px; padding: 14px 16px; background: var(--soft); border: 1px solid var(--border); border-radius: 10px; } .scope-note h2 { margin: 0 0 5px; font-size: 14px; } .scope-note p { margin: 0; color: var(--muted); }
  </style>
</head>
<body><main>
  <h1>${escapeHtml(labels.title)}</h1>
  <section class="meta">
    <div class="meta-item"><span>${escapeHtml(labels.drive)}</span><strong>${escapeHtml(scanSummary.drive_letter)}</strong></div>
    <div class="meta-item"><span>${escapeHtml(labels.scanMode)}</span><strong>${escapeHtml(isBaselineReport ? labels.baselineMode : labels.changeMode)}</strong></div>
    <div class="meta-item"><span>${escapeHtml(labels.generatedAt)}</span><strong>${escapeHtml(formatDateTime(Date.now(), locale))}</strong></div>
    <div class="meta-item"><span>${escapeHtml(labels.currentSize)}</span><strong>${escapeHtml(formatSize(scanSummary.total_size))}</strong></div>
    <div class="meta-item"><span>${escapeHtml(labels.netChange)}</span><strong class="${growthReport.total_growth >= 0 ? 'increase' : 'decrease'}">${escapeHtml(formatSignedSize(growthReport.total_growth))}</strong></div>
    <div class="meta-item"><span>${escapeHtml(labels.previousScan)}</span><strong>${escapeHtml(previousScan)}</strong></div>
    <div class="meta-item"><span>${escapeHtml(labels.currentScan)}</span><strong>${escapeHtml(scanSummary.current_scan_time)}</strong></div>
    <div class="meta-item"><span>${escapeHtml(labels.scannedFiles)}</span><strong>${scanSummary.total_files_scanned.toLocaleString(locale)}</strong></div>
    <div class="meta-item"><span>${escapeHtml(labels.resultCount)}</span><strong>${resultCount.toLocaleString(locale)}</strong></div>
  </section>
  <section class="scope-note"><h2>${escapeHtml(labels.scopeTitle)}</h2><p>${scopeNote}</p></section>
  <section>${content}</section>
  <footer>${escapeHtml(footerNote)}</footer>
</main>
</body></html>`;
}
