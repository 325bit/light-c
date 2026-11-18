// ============================================================================
// Error contract adapter (M16)
//
// The backend returns a structured error object over IPC:
//
//     { code: "permission_denied", message: "...", detail?: "...", args?: {...} }
//
// `code` is the only field the UI switches on; `message` is an English diagnostic
// that is only shown when a code has no translation yet; `detail` is the raw OS /
// tool output and belongs in a tooltip or a log line.
//
// Every `catch` block must go through this module. Calling `String(error)` on a
// structured error yields "[object Object]", which is exactly the bug this file
// exists to prevent.
// ============================================================================

import i18n from '../i18n';

/** Closed set — must stay in sync with `ErrorCode` in src-tauri/src/error.rs. */
export const ERROR_CODES = [
  'permission_denied',
  'not_found',
  'invalid_path',
  'invalid_argument',
  'unsupported',
  'target_in_use',
  'protected',
  'too_large',
  'cancelled',
  'external_failed',
  'internal',
] as const;

export type ErrorCode = (typeof ERROR_CODES)[number];

export interface AppErrorPayload {
  code: ErrorCode;
  /** English diagnostic from the backend. Fallback only — never the primary copy. */
  message: string;
  /** Raw OS / external-tool output; untranslatable, shown in tooltips. */
  detail?: string;
  /** Interpolation values for the localized message. */
  args?: Record<string, string>;
}

const ERROR_NS = 'errors';

function isErrorCode(value: unknown): value is ErrorCode {
  return typeof value === 'string' && (ERROR_CODES as readonly string[]).includes(value);
}

function asPayload(value: unknown): AppErrorPayload | null {
  if (!value || typeof value !== 'object') return null;
  const candidate = value as Record<string, unknown>;
  if (!isErrorCode(candidate.code)) return null;
  return {
    code: candidate.code,
    message: typeof candidate.message === 'string' ? candidate.message : '',
    detail: typeof candidate.detail === 'string' ? candidate.detail : undefined,
    args:
      candidate.args && typeof candidate.args === 'object'
        ? (candidate.args as Record<string, string>)
        : undefined,
  };
}

/**
 * Normalize anything a `catch` can receive into the structured shape.
 *
 * Tolerates the pre-M16 string ABI on purpose: a command that has not been
 * migrated yet must still produce readable text instead of throwing.
 */
export function toAppError(error: unknown): AppErrorPayload {
  const payload = asPayload(error);
  if (payload) return payload;

  if (typeof error === 'string') {
    return { code: 'internal', message: error };
  }
  if (error instanceof Error) {
    return { code: 'internal', message: error.message };
  }
  if (error === undefined || error === null) {
    return { code: 'internal', message: '' };
  }
  return { code: 'internal', message: String(error) };
}

function translate(key: string, payload: AppErrorPayload): string | undefined {
  if (!i18n.exists(`${ERROR_NS}:${key}`)) return undefined;
  return i18n.t(`${ERROR_NS}:${key}`, { ...payload.args, defaultValue: '' }) || undefined;
}

/**
 * Localized, user-facing message for an error.
 *
 * Fallback chain: `errors.<code>` → backend `message` → `errors.unknown`.
 * The middle step means an unmapped code degrades to an English diagnostic
 * instead of a blank toast or `[object Object]`.
 */
export function errorText(error: unknown): string {
  const payload = toAppError(error);
  const localized = translate(payload.code, payload);
  if (localized) return localized;
  if (payload.message) return payload.message;
  return translate('unknown', payload) ?? '';
}

/** Localized actionable hint for a code, when one is defined. */
export function errorHint(error: unknown): string | undefined {
  const payload = toAppError(error);
  return translate(`${payload.code}_hint`, payload);
}

/** Raw diagnostic text (backend message + OS detail), for logs and text sniffing. */
export function rawErrorText(error: unknown): string {
  const payload = toAppError(error);
  return payload.detail ? `${payload.message} (${payload.detail})` : payload.message;
}

/** Raw OS / external-tool output, for the tooltip line under a toast. */
export function errorDetail(error: unknown): string | undefined {
  return toAppError(error).detail;
}

/** Label to prefix {@link errorDetail} with. */
export function errorDetailLabel(): string {
  return translate('detailLabel', { code: 'internal', message: '' }) ?? 'detail';
}

/** Type guard for code-based branching; never branch on message text. */
export function isErrorCodeOf(error: unknown, code: ErrorCode): boolean {
  return toAppError(error).code === code;
}
