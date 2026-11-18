// i18n key validation: check keys across the four locale JSON files using the file's namespace + the key path referenced in code.
//
// Why it matters: when i18next cannot find a key it **does not error** - it shows the key name verbatim in the UI
// (e.g. a button reads "aiAssistant.removeKey"). Neither tsc nor the build catches this.
//
// Usage: node scripts/check-i18n-keys.mjs (or npm run check:i18n)
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const langs = ['zh', 'en', 'ja', 'zh-TW'];
const nsDir = path.join(root, 'src/i18n/locales');

// Treat arrays as leaves: expanding guide.notes-style arrays into .0/.1 would break the path check
const flat = (obj, prefix = '') =>
  Object.entries(obj).flatMap(([k, v]) => {
    const key = prefix ? `${prefix}.${k}` : k;
    return v && typeof v === 'object' && !Array.isArray(v) ? flat(v, key) : [key];
  });

const nsKeys = {};
for (const file of fs.readdirSync(path.join(nsDir, 'zh')).filter((f) => f.endsWith('.json'))) {
  const name = file.replace('.json', '');
  nsKeys[name] = Object.fromEntries(
    langs.map((lang) => [
      lang,
      new Set(flat(JSON.parse(fs.readFileSync(path.join(nsDir, lang, file), 'utf8')))),
    ]),
  );
}

const walk = (dir) =>
  fs.readdirSync(dir, { withFileTypes: true }).flatMap((entry) =>
    entry.isDirectory()
      ? walk(path.join(dir, entry.name))
      : entry.name.endsWith('.tsx')
        ? [path.join(dir, entry.name)]
        : [],
  );

const problems = [];
for (const file of walk(path.join(root, 'src'))) {
  const text = fs.readFileSync(file, 'utf8');
  // common is loaded globally, so any file may use it; collect the rest via useTranslation.
  // Also recognize translation functions injected as props: `t: TFunction<'modules'>` names its namespace via the generic argument
  // (child components such as LeftoverWhitelistModal get `t` this way).
  const namespaces = [
    'common',
    ...new Set([
      ...[...text.matchAll(/useTranslation\(\s*['"](\w+)['"]\s*\)/g)].map((match) => match[1]),
      ...[...text.matchAll(/:\s*TFunction<\s*['"](\w+)['"]/g)].map((match) => match[1]),
    ]),
  ];

  for (const match of text.matchAll(/\b(?:t|moduleT|commonT)\(\s*['"`]([A-Za-z][\w.]*)['"`]/g)) {
    const key = match[1];
    // ⚠️ Do not skip keys without a dot: root-level keys (e.g. footerWebsite) can also go missing and render verbatim.
    //    We once skipped them and a key wrongly placed in groups passed validation (hit on 2026-10-22).
    // A key is only a problem when it is missing in **every** namespace this file uses.
    const missingEverywhere = namespaces.every((ns) =>
      langs.some((lang) => !nsKeys[ns]?.[lang]?.has(key)),
    );
    if (!missingEverywhere) continue;
    const elsewhere = Object.keys(nsKeys).filter(
      (ns) => !namespaces.includes(ns) && nsKeys[ns].zh.has(key),
    );
    problems.push(
      elsewhere.length
        ? `${path.relative(root, file)}  ${key}  ✗ 放错命名空间（在 ${elsewhere.join('/')} 里，文件用的是 ${namespaces.join('/')}）`
        : `${path.relative(root, file)}  ${key}  ✗ 四语言都找不到`,
    );
  }
}

// Key mismatches between locales (missing translations / extra keys)
for (const [namespace, byLang] of Object.entries(nsKeys)) {
  const base = byLang.zh;
  for (const lang of langs.slice(1)) {
    const missing = [...base].filter((key) => !byLang[lang].has(key));
    const extra = [...byLang[lang]].filter((key) => !base.has(key));
    if (missing.length || extra.length) {
      problems.push(`${namespace}.json  ${lang}: 缺[${missing}] 多[${extra}]`);
    }
  }
}

if (problems.length === 0) {
  console.log('✓ i18n 键全部有效，四语言一致');
} else {
  console.log(problems.join('\n'));
  console.log(`\n共 ${problems.length} 个问题`);
}
process.exitCode = problems.length ? 1 : 0;
