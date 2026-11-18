// ============================================================================
// Minimal Markdown rendering
//
// Two deliberate choices:
//
// 1. **Do not use an off-the-shelf library**: the project convention is "no new dependency if native capability suffices", and the syntax actually used in model output
//    is just these few kinds (headings, lists, bold, inline code, code blocks, quotes) — writing it ourselves is actually more controllable.
//
// 2. **Produce React nodes, not an HTML string**: if dangerouslySetInnerHTML were used,
//    content in the model output would be executed as HTML, an injection risk. Everything here goes through React elements,
//    so it is **structurally impossible to execute scripts** — no need to add another sanitization layer.
// ============================================================================

import type { ReactNode } from 'react';

/** Inline syntax: bold and inline code. Only these two are recognized — enough and will not damage normal text. */
function renderInline(text: string, keyPrefix: string): ReactNode[] {
  const nodes: ReactNode[] = [];
  const pattern = /(\*\*[^*\n]+\*\*|`[^`\n]+`)/g;
  let lastIndex = 0;
  let match: RegExpExecArray | null;
  let index = 0;

  while ((match = pattern.exec(text)) !== null) {
    if (match.index > lastIndex) nodes.push(text.slice(lastIndex, match.index));
    const token = match[0];
    if (token.startsWith('**')) {
      nodes.push(
        <strong key={`${keyPrefix}-b${index}`} className="font-medium">
          {token.slice(2, -2)}
        </strong>,
      );
    } else {
      nodes.push(
        <code
          key={`${keyPrefix}-c${index}`}
          className="rounded bg-[var(--bg-hover)] px-1 py-0.5 font-mono text-[0.95em]"
        >
          {token.slice(1, -1)}
        </code>,
      );
    }
    lastIndex = match.index + token.length;
    index += 1;
  }

  if (lastIndex < text.length) nodes.push(text.slice(lastIndex));
  return nodes;
}

/**
 * Render the Markdown returned by the model into React nodes.
 * Unrecognized syntax is treated as a normal paragraph — better to recognize too little than to render wrong.
 */
export function renderMarkdown(source: string): ReactNode {
  const lines = source.replace(/\r\n/g, '\n').split('\n');
  const blocks: ReactNode[] = [];
  let index = 0;
  let key = 0;

  while (index < lines.length) {
    const line = lines[index];

    // Code block: from ``` to the next ```, contents preserved verbatim (no inline parsing)
    if (line.trimStart().startsWith('```')) {
      const codeLines: string[] = [];
      index += 1;
      while (index < lines.length && !lines[index].trimStart().startsWith('```')) {
        codeLines.push(lines[index]);
        index += 1;
      }
      index += 1; // skip the closing ```
      blocks.push(
        <pre
          key={`code-${key++}`}
          className="overflow-x-auto rounded-lg bg-[var(--bg-hover)] p-2 font-mono whitespace-pre"
        >
          <code>{codeLines.join('\n')}</code>
        </pre>,
      );
      continue;
    }

    // Heading: the number of # determines the level, enough for conversation
    const heading = /^(#{1,4})\s+(.*)$/.exec(line);
    if (heading) {
      const level = heading[1].length;
      blocks.push(
        <p
          key={`h-${key++}`}
          className={`font-medium text-[var(--text-primary)]${level <= 2 ? ' text-[1.05em]' : ''}`}
        >
          {renderInline(heading[2], `h-${key}`)}
        </p>,
      );
      index += 1;
      continue;
    }

    // Unordered list: consecutive lines merged into one list block
    if (/^\s*[-*]\s+/.test(line)) {
      const items: string[] = [];
      while (index < lines.length && /^\s*[-*]\s+/.test(lines[index])) {
        items.push(lines[index].replace(/^\s*[-*]\s+/, ''));
        index += 1;
      }
      blocks.push(
        <ul key={`ul-${key++}`} className="list-disc space-y-0.5 pl-4">
          {items.map((item, itemIndex) => (
            <li key={itemIndex}>{renderInline(item, `ul-${key}-${itemIndex}`)}</li>
          ))}
        </ul>,
      );
      continue;
    }

    // Ordered list
    if (/^\s*\d+[.)]\s+/.test(line)) {
      const items: string[] = [];
      while (index < lines.length && /^\s*\d+[.)]\s+/.test(lines[index])) {
        items.push(lines[index].replace(/^\s*\d+[.)]\s+/, ''));
        index += 1;
      }
      blocks.push(
        <ol key={`ol-${key++}`} className="list-decimal space-y-0.5 pl-4">
          {items.map((item, itemIndex) => (
            <li key={itemIndex}>{renderInline(item, `ol-${key}-${itemIndex}`)}</li>
          ))}
        </ol>,
      );
      continue;
    }

    // Quote
    if (/^\s*>\s?/.test(line)) {
      blocks.push(
        <blockquote
          key={`q-${key++}`}
          className="border-l-2 border-[var(--border-color)] pl-2 text-[var(--text-muted)]"
        >
          {renderInline(line.replace(/^\s*>\s?/, ''), `q-${key}`)}
        </blockquote>,
      );
      index += 1;
      continue;
    }

    // Skip blank lines
    if (!line.trim()) {
      index += 1;
      continue;
    }

    // Normal paragraph (merge adjacent non-empty lines, so every line is not cut into its own paragraph)
    const paragraph: string[] = [];
    while (
      index < lines.length &&
      lines[index].trim() &&
      !/^\s*([-*]\s+|\d+[.)]\s+|>\s?|#{1,4}\s+)/.test(lines[index]) &&
      !lines[index].trimStart().startsWith('```')
    ) {
      paragraph.push(lines[index]);
      index += 1;
    }
    blocks.push(
      <p key={`p-${key++}`} className="whitespace-pre-wrap break-words">
        {renderInline(paragraph.join('\n'), `p-${key}`)}
      </p>,
    );
  }

  return <>{blocks}</>;
}
