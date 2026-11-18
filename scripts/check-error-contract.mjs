// ============================================================================
// Error contract gate (M16)
//
// Two invariants that a type checker cannot catch:
//
// 1. Every locale's errors.json must carry the same key set. A code without a
//    translation silently falls back to the English diagnostic, which is how the
//    previous hand-written message tables drifted apart.
// 2. Nothing outside src/api/errors.ts may stringify an unknown error. Calling
//    `String(error)` on the structured payload yields "[object Object]" and turns
//    a precise failure into a useless toast.
//
// Run: node scripts/check-error-contract.mjs
// ============================================================================

import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, relative } from 'node:path';

const ROOT = new URL('..', import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1');
const SRC = join(ROOT, 'src');
const LOCALES = ['zh', 'zh-TW', 'en', 'ja'];
const ALLOWED_STRINGIFY = 'src/api/errors.ts';

const failures = [];

function loadKeys(locale) {
  const file = join(SRC, 'i18n', 'locales', locale, 'errors.json');
  return new Set(Object.keys(JSON.parse(readFileSync(file, 'utf8'))));
}

const [reference, ...others] = LOCALES;
const referenceKeys = loadKeys(reference);

for (const locale of others) {
  const keys = loadKeys(locale);
  const missing = [...referenceKeys].filter((key) => !keys.has(key));
  const extra = [...keys].filter((key) => !referenceKeys.has(key));
  if (missing.length) failures.push(`${locale}/errors.json is missing: ${missing.join(', ')}`);
  if (extra.length) failures.push(`${locale}/errors.json has unknown keys: ${extra.join(', ')}`);
}

function walk(dir) {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) {
      walk(full);
      continue;
    }
    if (!/\.(ts|tsx)$/.test(entry)) continue;
    if (relative(ROOT, full).replace(/\\/g, '/') === ALLOWED_STRINGIFY) continue;

    const text = readFileSync(full, 'utf8');
    text.split('\n').forEach((line, index) => {
      if (line.includes('String(error)')) {
        failures.push(`${relative(ROOT, full).replace(/\\/g, '/')}:${index + 1} stringifies an error — use errorText() from api/errors`);
      }
    });
  }
}

walk(SRC);

if (failures.length) {
  console.error('Error contract check failed:');
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log(`Error contract OK (${referenceKeys.size} keys × ${LOCALES.length} locales, no raw String(error))`);
