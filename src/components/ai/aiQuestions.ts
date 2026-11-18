// ============================================================================
// Question templates for quick asks
//
// **Do not directly reuse the "one-click search" keywords**. Those are phrases for search engines, e.g.
//   `Windows file C:\Windows\Temp - can it be deleted`
// piling up space-separated words is fine for search engines, but a model in a specific module's context reads it as "handle this object" -
// the old driver module thus understood "look up this driver" as "delete this driver" and started proposing deletion outright.
//
// So wrap the same object into a full question here uniformly: fields still come from the module's existing data,
// no separate set is maintained; only "how to ask" changes.
//
// ⚠️ The question must also follow the UI language: a Chinese question popping up in an English/Japanese UI is jarring,
// and would lead the model to answer in Chinese as well. The text lives under i18n `askQuestion.*`, one set for each of the four languages.
// ============================================================================

import i18n from '../../i18n';

/** Type of object being asked about; determines wording and follow-up focus. */
export type AskTargetKind = 'file' | 'folder' | 'model';

/**
 * Build the full question for an object. target is usually a path or a model name.
 *
 * ⚠️ The three keys `askQuestion.file / folder / model` are **not type-checked** -
 * a typo in a key raises no tsc or build error; the key name itself just shows up in the bubble.
 * All four languages are filled in; when changing them, verify each one.
 */
export function buildItemQuestion(target: string, kind: AskTargetKind = 'file'): string {
  const trimmed = target.trim();
  if (!trimmed) return '';
  return i18n.t(`askQuestion.${kind}`, { ns: 'ai', target: trimmed });
}
