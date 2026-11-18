// ============================================================================
// i18n placeholder audit script
//
// Background: i18next does not throw when interpolation arguments are missing; it displays {{name}} verbatim.
// This project once had a production issue where a dialog showed "{{name}}" directly, so a static check is needed:
//
//   1. Inside language packs: whether keys in zh / zh-TW / en / ja align, and whether placeholders for the same key are consistent
//   2. At call sites: whether arguments passed to t('key', {...}) cover the placeholders in the text
//   3. Whether the key exists and the namespace matches
//
// Usage: node scripts/i18n-audit.js
// ============================================================================

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const LOCALES_DIR = path.join(ROOT, 'src', 'i18n', 'locales');
const SRC_DIR = path.join(ROOT, 'src');

/** Namespaces registered for language packs in i18n/index.ts */
const NAMESPACES = ['common', 'nav', 'settings', 'junkClean', 'modules', 'ui'];
const LANGUAGES = ['zh', 'zh-TW', 'en', 'ja'];
const DEFAULT_NS = 'common';
const INTERPOLATION_NS = 'common'; // useTranslation reads interpolation format config from common by default

/** Keep in sync with the interpolation config in i18n/index.ts */
const PREFIX = '{{';
const SUFFIX = '}}';

const issues = [];
const stats = { filesScanned: 0, callsFound: 0, staticCalls: 0, dynamicCalls: 0 };

function report(level, file, line, message) {
  issues.push({ level, file, line, message });
}

// ---------------------------------------------------------------------------
// Language pack loading
// ---------------------------------------------------------------------------

/** Read all language packs and flatten them into { ns: { lang: { 'a.b.c': 'text' } } } */
function loadLocales() {
  const byNs = {};
  for (const ns of NAMESPACES) {
    byNs[ns] = {};
    for (const lang of LANGUAGES) {
      const file = path.join(LOCALES_DIR, lang, `${ns}.json`);
      if (!fs.existsSync(file)) {
        report('error', path.relative(ROOT, file), 0, `语言包文件缺失`);
        byNs[ns][lang] = {};
        continue;
      }
      let parsed;
      try {
        parsed = JSON.parse(fs.readFileSync(file, 'utf8'));
      } catch (error) {
        report('error', path.relative(ROOT, file), 0, `JSON 解析失败：${error.message}`);
        byNs[ns][lang] = {};
        continue;
      }
      byNs[ns][lang] = flatten(parsed, '');
    }
  }
  return byNs;
}

/** Flatten a nested object into dot-separated paths; non-string leaves are ignored and reported separately */
function flatten(node, prefix, out = {}, fileLabel = '') {
  if (node === null || typeof node !== 'object') return out;
  for (const [key, value] of Object.entries(node)) {
    const fullKey = prefix ? `${prefix}.${key}` : key;
    if (typeof value === 'string') {
      out[fullKey] = value;
    } else if (value && typeof value === 'object') {
      flatten(value, fullKey, out, fileLabel);
    } else {
      report('warn', fileLabel, 0, `非字符串文案（${typeof value}）：${fullKey}`);
    }
  }
  return out;
}

/** Extract interpolation variable names from text, supporting i18next's {{var}} / {{ var }} syntax */
const PLACEHOLDER_RE = /\{\{\s*([A-Za-z0-9_$.-]+)\s*\}\}/g;
function extractPlaceholders(text) {
  const found = new Set();
  let match;
  PLACEHOLDER_RE.lastIndex = 0;
  while ((match = PLACEHOLDER_RE.exec(text)) !== null) {
    found.add(match[1]);
  }
  return found;
}

// ---------------------------------------------------------------------------
// Language pack consistency check
// ---------------------------------------------------------------------------

function checkLocaleAlignment(byNs) {
  for (const ns of NAMESPACES) {
    const languagesPresent = Object.keys(byNs[ns]).filter((l) => Object.keys(byNs[ns][l]).length > 0);
    if (languagesPresent.length === 0) continue;

    // Use zh as the baseline; if zh is missing, fall back to the first language with content
    const base = byNs[ns]['zh'] && Object.keys(byNs[ns]['zh']).length > 0 ? 'zh' : languagesPresent[0];
    const baseKeys = Object.keys(byNs[ns][base]);

    // Missing / extra keys
    for (const lang of LANGUAGES) {
      const target = byNs[ns][lang];
      if (!target) continue;
      for (const key of baseKeys) {
        if (!(key in target)) {
          report('error', `i18n/locales/${lang}/${ns}.json`, 0, `缺少 key（${base} 有）：${key}`);
        }
      }
      for (const key of Object.keys(target)) {
        if (!(key in byNs[ns][base])) {
          report('warn', `i18n/locales/${lang}/${ns}.json`, 0, `多余 key（${base} 没有）：${key}`);
        }
      }
    }

    // The set of placeholders for the same key must be consistent across languages, otherwise switching languages drops variables
    for (const key of baseKeys) {
      const baseSet = extractPlaceholders(byNs[ns][base][key]);
      for (const lang of LANGUAGES) {
        if (lang === base) continue;
        const text = byNs[ns][lang]?.[key];
        if (text === undefined) continue;
        const targetSet = extractPlaceholders(text);
        const missing = [...baseSet].filter((v) => !targetSet.has(v));
        const extra = [...targetSet].filter((v) => !baseSet.has(v));
        if (missing.length || extra.length) {
          const detail = [
            missing.length ? `缺少 ${missing.join(', ')}` : '',
            extra.length ? `多出 ${extra.join(', ')}` : '',
          ].filter(Boolean).join('；');
          report('warn', `i18n/locales/${lang}/${ns}.json`, 0, `占位符与 ${base} 不一致（${key}）：${detail}`);
        }
      }
    }
  }
}

// ---------------------------------------------------------------------------
// Call-site check
// ---------------------------------------------------------------------------

/**
 * Infer the namespace for each t variable from useTranslation('ns').
 *
 * Note 1: isIdentifier(node.name) cannot be used as a pre-filter — the name in `const { t } = useTranslation()`
 * is an ObjectBindingPattern (destructuring), not an Identifier, so that would miss all normal usages.
 *
 * Note 2: lexical scope must be resolved. The same file may have several components each declaring `const { t } = useTranslation('x')`,
 * e.g. AboutSettings.tsx: the main component uses settings and the child component at the bottom uses ui. If overwritten at file level,
 * later declarations would pollute earlier calls and produce many false errors.
 * Here a "scope chain per function body" approach is used: on entering a function, push its internal t mappings,
 * and look up along the scope chain from inner to outer.
 */
function collectTranslators(sourceFile) {
  const hooksByScope = new Map(); // scopeNode -> Map<tVarName, ns>

  const isUseTranslationCall = (call) => {
    const callee = call.expression;
    return (ts.isIdentifier(callee) && callee.text === 'useTranslation')
      || (ts.isPropertyAccessExpression(callee) && callee.name.text === 'useTranslation');
  };

  /** Find t mappings declared directly in a scope node (function/arrow function body), without recursing into nested functions */
  const scanScopeDeclarations = (scopeNode, result) => {
    const walk = (node) => {
      // Stop at nested functions; they belong to an inner scope
      if (node !== scopeNode && ts.isFunctionLike(node)) return;
      if (ts.isVariableDeclaration(node) && node.initializer
        && ts.isObjectBindingPattern(node.name) && ts.isCallExpression(node.initializer)
        && isUseTranslationCall(node.initializer)) {
        const nsArg = node.initializer.arguments[0];
        const ns = nsArg && ts.isStringLiteralLike(nsArg) ? nsArg.text : DEFAULT_NS;
        for (const el of node.name.elements) {
          // Support both `{ t }` and `{ t: moduleT }` destructuring forms
          if (el.name && ts.isIdentifier(el.name)) result.set(el.name.text, ns);
        }
      }
      ts.forEachChild(node, walk);
    };
    ts.forEachChild(scopeNode, walk);
  };

  // Precompute declarations for each scope (SourceFile + each function) first
  const visit = (node) => {
    if (ts.isSourceFile(node) || ts.isFunctionLike(node)) {
      const decls = new Map();
      scanScopeDeclarations(node, decls);
      if (decls.size > 0) hooksByScope.set(node, decls);
    }
    ts.forEachChild(node, visit);
  };
  visit(sourceFile);

  return hooksByScope;
}

/**
 * Look up the namespace of a t variable along the AST parent chain from inner to outer.
 * Returning null means this t does not come from useTranslation (it may be a plain local variable) and should be skipped.
 */
function resolveTranslatorNs(node, hooksByScope) {
  let current = node;
  while (current) {
    // Start from the nearest function scope and work outward
    if (ts.isFunctionLike(current) || ts.isSourceFile(current)) {
      const decls = hooksByScope.get(current);
      if (decls) {
        const name = node.text;
        if (decls.has(name)) return decls.get(name);
      }
    }
    current = current.parent;
  }
  return null;
}

/** Get the value of a string literal */
function literalText(node) {
  if (!node) return null;
  if (ts.isStringLiteralLike(node)) return node.text;
  if (ts.isNoSubstitutionTemplateLiteral(node)) return node.text;
  return null;
}

/**
 * Parse key names in the call argument object, only statically determinable ones:
 * { size: x }、{ 'size': x }、{ ...spread }
 * Returns { keys: Set<string>, hasSpread: boolean }
 */
function collectObjectKeys(objLiteral) {
  const keys = new Set();
  let hasSpread = false;
  for (const prop of objLiteral.properties) {
    if (ts.isSpreadAssignment(prop)) {
      hasSpread = true;
      continue;
    }
    if (!prop.name) continue;
    if (ts.isIdentifier(prop.name) || ts.isStringLiteralLike(prop.name)) {
      keys.add(prop.name.text);
    } else if (ts.isComputedPropertyName(prop.name)) {
      hasSpread = true; // computed property names cannot be statically determined
    }
  }
  return { keys, hasSpread };
}

/**
 * Try to restore an expression to a static string (supports static fragments in template literals).
 * Returns { values: Set<string>, hasDynamic: boolean }
 */
function collectDynamicParts(node) {
  const parts = new Set();
  let hasDynamic = false;
  const walk = (n) => {
    if (ts.isTemplateExpression(n)) {
      parts.add(n.head.text);
      for (const span of n.templateSpans) {
        parts.add(span.literal.text);
        walk(span.expression);
      }
      return;
    }
    ts.forEachChild(n, walk);
  };
  walk(node);
  if (node && ts.isTemplateExpression(node)) return { parts, hasDynamic: true };
  // String concatenation 'a' + x + 'b'
  if (node && ts.isBinaryExpression(node) && node.operatorToken.kind === ts.SyntaxKind.PlusToken) {
    const gather = (n) => {
      if (ts.isBinaryExpression(n) && n.operatorToken.kind === ts.SyntaxKind.PlusToken) {
        gather(n.left); gather(n.right); return;
      }
      const lit = literalText(n);
      if (lit !== null) parts.add(lit);
      else hasDynamic = true;
    };
    gather(node);
    return { parts, hasDynamic };
  }
  return { parts, hasDynamic: true };
}

function checkCallSites(byNs) {
  const files = [];
  const walkDir = (dir) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) walkDir(full);
      else if (/\.(ts|tsx)$/.test(entry.name) && !entry.name.endsWith('.d.ts')) files.push(full);
    }
  };
  walkDir(SRC_DIR);

  for (const file of files) {
    stats.filesScanned++;
    const content = fs.readFileSync(file, 'utf8');
    const sourceFile = ts.createSourceFile(
      file, content, ts.ScriptTarget.Latest, true,
      file.endsWith('.tsx') ? ts.ScriptKind.TSX : ts.ScriptKind.TS,
    );
    // Parsing scope requires parent pointers, which createSourceFile does not attach by default; add them manually here
    attachParents(sourceFile);
    const hooksByScope = collectTranslators(sourceFile);
    const rel = path.relative(ROOT, file).replace(/\\/g, '/');

    /** Locate the line number (1-based) */
    const lineOf = (node) => sourceFile.getLineAndCharacterOfPosition(node.getStart(sourceFile)).line + 1;

    const visit = (node) => {
      if (ts.isCallExpression(node) && ts.isIdentifier(node.expression)) {
        // Only handle t / aliases coming from useTranslation
        const ns = resolveTranslatorNs(node.expression, hooksByScope);
        if (ns) {
          stats.callsFound++;
          handleCall(node, ns, rel, lineOf(node), byNs);
        }
      }
      ts.forEachChild(node, visit);
    };
    visit(sourceFile);
  }
}

/** Add parent pointers to the AST for looking up variable origins along the scope chain */
function attachParents(node, parent = null) {
  node.parent = parent;
  ts.forEachChild(node, (child) => attachParents(child, node));
}

function handleCall(node, defaultNs, rel, line, byNs) {
  const args = node.arguments;
  if (args.length === 0) return;

  const first = args[0];
  const explicitNs = literalText(args[1]);
  const options = args[2];

  // Only handle calls whose first argument is a string literal
  const rawKey = literalText(first);
  if (rawKey === null) {
    // Dynamic keys like t(`social.category.${id}.name`) cannot be statically verified
    const { hasDynamic } = collectDynamicParts(first);
    if (hasDynamic) {
      stats.dynamicCalls++;
      // For a full path with an ns prefix, do a coarse-grained check
      return;
    }
    return;
  }

  // Parse the namespace prefix in the key: 'common:foo' / 'modules:driverUi.scan'
  let ns = defaultNs;
  let key = rawKey;
  const colonIndex = rawKey.indexOf(':');
  if (colonIndex > 0) {
    const maybeNs = rawKey.slice(0, colonIndex);
    if (NAMESPACES.includes(maybeNs)) {
      ns = maybeNs;
      key = rawKey.slice(colonIndex + 1);
    }
  }
  // t('key', { ns: 'modules' })
  if (options && ts.isObjectLiteralExpression(options)) {
    for (const prop of options.properties) {
      if (ts.isPropertyAssignment(prop) && prop.name && prop.name.getText() === 'ns') {
        const v = literalText(prop.initializer);
        if (v) ns = v;
      }
    }
  }
  if (explicitNs && NAMESPACES.includes(explicitNs)) ns = explicitNs;

  // Interpolation arguments are always taken from the 2nd argument (i18next: t(key, options))
  const interpolationArg = args[1] && ts.isObjectLiteralExpression(args[1]) ? args[1] : null;

  // returnObjects means this key retrieves an array/object itself, not text,
  // so interpolation arguments need no validation (the scanner only flattens string leaves)
  const wantsRawObject = !!interpolationArg && interpolationArg.properties.some(
    (prop) => ts.isPropertyAssignment(prop)
      && prop.name
      && prop.name.getText() === 'returnObjects'
      && prop.initializer.kind === ts.SyntaxKind.TrueKeyword,
  );

  // returnObjects retrieves the raw array/object, whose leaves do not appear in the flattened table; skip validation
  if (wantsRawObject) return;

  const localeTable = byNs[ns];
  if (!localeTable) return;
  const zhTable = localeTable['zh'] || {};
  if (!(key in zhTable)) {
    // Dynamically concatenated keys cause false positives; first confirm it does not exist in any language
    const existsAnywhere = LANGUAGES.some((lang) => localeTable[lang] && key in localeTable[lang]);
    if (!existsAnywhere) {
      report('error', rel, line, `key 不存在：${ns}:${key}`);
      return;
    }
  }

  // Placeholder validation uses zh as the baseline and takes the union of placeholders across all languages (stricter)
  const required = new Set();
  for (const lang of LANGUAGES) {
    const text = localeTable[lang]?.[key];
    if (typeof text === 'string') {
      for (const v of extractPlaceholders(text)) required.add(v);
    }
  }
  if (required.size === 0) return;

  // count is an i18next plural/built-in variable, usually injected by the library; still require it to be passed explicitly for troubleshooting
  const provided = new Set();
  let hasSpread = false;
  if (interpolationArg) {
    const { keys, hasSpread: spread } = collectObjectKeys(interpolationArg);
    hasSpread = spread;
    for (const k of keys) provided.add(k);
  }

  if (hasSpread) return; // cannot be statically determined, skip

  const missing = [...required].filter((v) => !provided.has(v));
  if (missing.length > 0) {
    report('error', rel, line, `缺少插值参数 [${missing.join(', ')}] → ${ns}:${key}`);
  }
}

// ---------------------------------------------------------------------------
// Main flow
// ---------------------------------------------------------------------------

function main() {
  const byNs = loadLocales();
  checkLocaleAlignment(byNs);
  checkCallSites(byNs);

  const errors = issues.filter((i) => i.level === 'error');
  const warns = issues.filter((i) => i.level === 'warn');

  console.log(`扫描文件 ${stats.filesScanned} 个，翻译调用 ${stats.callsFound} 处（动态 key ${stats.dynamicCalls} 处跳过）`);
  console.log(`问题：${errors.length} error / ${warns.length} warn\n`);

  if (errors.length) {
    console.log('=== ERROR ===');
    for (const i of errors) {
      console.log(`  [${i.file}${i.line ? ':' + i.line : ''}] ${i.message}`);
    }
    console.log('');
  }
  if (warns.length) {
    console.log('=== WARN ===');
    for (const i of warns) {
      console.log(`  [${i.file}${i.line ? ':' + i.line : ''}] ${i.message}`);
    }
    console.log('');
  }

  fs.writeFileSync(
    path.join(ROOT, '.i18n-audit.json'),
    JSON.stringify({ stats, issues }, null, 2),
    'utf8',
  );
  process.exit(errors.length > 0 ? 1 : 0);
}

main();
