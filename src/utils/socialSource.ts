// ============================================================================
// Social app source name normalization
//
// Backend adapters report app_name for "the same app" in several forms:
//   - WeChat: 3.x reports the Chinese name, 4.x reports "WeChat"
//   - DingTalk: reports the Chinese name or "DingTalk"
// Grouping directly by app_name would give the filter two labels pointing to the same app, leaving the user no choice.
// Here the raw app_name is mapped to a stable source key, and i18n decides the display text.
//
// Note: QQ and NTQQ are two independent clients that can coexist, not two spellings of the same app,
// so they remain two separate sources and are not merged.
// ============================================================================

/** Raw app_name (lowercase) → stable source key */
const SOURCE_KEY_MAP: Record<string, string> = {
  // WeChat 3.x / 4.x
  微信: 'wechat',
  wechat: 'wechat',
  // DingTalk Chinese / English names
  钉钉: 'dingtalk',
  dingtalk: 'dingtalk',
  企业微信: 'wxwork',
  wxwork: 'wxwork',
  飞书: 'feishu',
  feishu: 'feishu',
  // The following apps already use stable English names on the backend; map to themselves to reuse the same key validation
  qq: 'qq',
  ntqq: 'ntqq',
  telegram: 'telegram',
  line: 'line',
  whatsapp: 'whatsapp',
};

/** Stable source keys already included, used to decide whether i18n translation is needed */
const KNOWN_SOURCE_KEYS = new Set(Object.values(SOURCE_KEY_MAP));

/**
 * Normalize the app_name reported by the backend into a stable source key.
 *
 * Unlisted apps (e.g. adapters added later) are returned as-is,
 * so the filter can still group by them and no data is lost due to a missing mapping.
 */
export function getSourceKey(appName: string): string {
  const trimmed = appName.trim();
  if (trimmed.length === 0) {
    return appName;
  }
  return SOURCE_KEY_MAP[trimmed.toLowerCase()] ?? trimmed;
}

/**
 * Convert a source key to its display name in the current language.
 *
 * Unlisted keys are returned as-is, so new apps display normally before their text is added.
 */
export function getSourceLabel(
  sourceKey: string,
  translate: (key: string) => string,
): string {
  if (!KNOWN_SOURCE_KEYS.has(sourceKey)) {
    return sourceKey;
  }
  return translate(`social.app.${sourceKey}`);
}
