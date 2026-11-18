// ============================================================================
// AI assistant - frontend calling layer
//
// Only wraps invoke, with no business logic: network requests, key management, and protocol adaptation all live in the backend.
// ⚠️ The plaintext API key is never available here.
// ============================================================================

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type {
  AiChatReply,
  AiChatRequest,
  AiProfileInput,
  AiProviderPreset,
  AiPublicConfig,
} from '../types/ai';

/** Get the list of built-in provider presets (used by the settings page dropdown). */
export async function listAiProviders(): Promise<AiProviderPreset[]> {
  return invoke<AiProviderPreset[]>('list_ai_providers');
}

/** Read the current config (keys are masked). */
export async function getAiConfig(): Promise<AiPublicConfig> {
  return invoke<AiPublicConfig>('get_ai_config');
}

/**
 * Add or update a provider profile.
 * An empty `id` = add new; an empty `apiKey` = leave the saved key unchanged
 * (the frontend only gets the masked value and cannot return plaintext, so leaving it empty is the only way to say "do not change").
 */
export async function saveAiProfile(profile: AiProfileInput): Promise<AiPublicConfig> {
  return invoke<AiPublicConfig>('save_ai_profile', { profile });
}

/** Delete a provider profile; if the deleted one is the current profile, the backend switches to the first remaining profile. */
export async function deleteAiProfile(profileId: string): Promise<AiPublicConfig> {
  return invoke<AiPublicConfig>('delete_ai_profile', { profileId });
}

/**
 * Clear the saved key of a profile (the profile itself is kept).
 * If a key is leaked, the user must be able to revoke it — otherwise the only option is to delete the whole profile.
 */
export async function clearAiProfileKey(profileId: string): Promise<AiPublicConfig> {
  return invoke<AiPublicConfig>('clear_ai_profile_key', { profileId });
}

/** Switch the currently active provider profile. */
export async function setActiveAiProfile(profileId: string): Promise<AiPublicConfig> {
  return invoke<AiPublicConfig>('set_active_ai_profile', { profileId });
}

/** Set the AI data directory (a global setting, not belonging to any provider). */
export async function setAiDataDir(dataDir: string): Promise<AiPublicConfig> {
  return invoke<AiPublicConfig>('set_ai_data_dir', { dataDir });
}

/**
 * Test connectivity and fetch the list of available models.
 * Getting the list successfully means the endpoint, key, and network are all working — this is what the settings page's "Test Connection" uses.
 */
export async function testAiConnection(): Promise<string[]> {
  return invoke<string[]>('test_ai_connection');
}

/** Send one chat message (non-streaming). Module context injection belongs to a later stage. */
export async function aiChat(request: AiChatRequest): Promise<AiChatReply> {
  return invoke<AiChatReply>('ai_chat', { request });
}

/** Streaming delta payload, corresponding to AiStreamDelta in the backend commands/ai.rs. */
export interface AiStreamDelta {
  streamId: string;
  text: string;
}

/**
 * Listen for streaming deltas. Returns a function to cancel the listener.
 *
 * Note: the caller must cancel the listener when finished (success or failure) — otherwise a chat leaves behind a
 * permanent listener, and the next chat's deltas get accumulated twice.
 */
export async function listenAiStream(
  onDelta: (delta: AiStreamDelta) => void,
): Promise<() => void> {
  return listen<AiStreamDelta>('ai-stream-delta', (event) => onDelta(event.payload));
}

/**
 * Streaming chat: deltas are pushed via events (for display as they are generated),
 * and the command returns the full text — the frontend treats the return value as authoritative; events only provide the "real-time feel".
 */
export async function aiChatStream(request: AiChatRequest, streamId: string): Promise<AiChatReply> {
  return invoke<AiChatReply>('ai_chat_stream', { request, streamId });
}

/** A session message written to disk, corresponding to AiSessionMessage in the backend ai/session.rs. */
export interface AiSessionMessage {
  role: 'user' | 'assistant';
  content: string;
  /** Attachment file name (stored under attachments in the AI data directory). **Only the name is stored, not base64** */
  images?: string[];
  /** Unix millisecond timestamp */
  at: number;
  /**
   * Chain of thought in thinking mode. **Must be written to disk** — not returning it to the provider causes a 400,
   * and history restored from disk after a restart must send it along as well.
   */
  reasoningContent?: string;
  /** This assistant message was generated locally (tool query result); treat it as user-side content when sending the request */
  localToolOutput?: boolean;
  /** ID of the profile that produced this reply: chain of thought is only sent back to the profile that produced it */
  producedByProfile?: string;
}

/**
 * Result of reading history by range (backend `load_ai_session_range`).
 *
 * `messages` are **in chronological order**; the frontend can prepend them directly to the message list.
 * Defined here rather than in `types/ai.ts`: it depends on `AiSessionMessage`, which is also in this file
 * (putting it in types would create a circular reference between types ↔ api).
 */
export interface AiSessionPage {
  messages: AiSessionMessage[];
  /** Whether there are earlier messages not yet read (the UI shows "Load earlier" accordingly) */
  hasMore: boolean;
}

/**
 * Local date key (YYYY-MM-DD).
 * Built manually with local time; **do not use toISOString** — that is UTC,
 * and in UTC+8 after 8 PM it becomes "tomorrow", shifting the session across days.
 */
export function localDateKey(date = new Date()): string {
  const pad = (value: number) => String(value).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

/**
 * Save an image attachment and return its file name.
 * The image is stored as a separate file and only its name is kept in the session — otherwise base64 would blow up the jsonl.
 */
export async function saveAiAttachment(dataBase64: string, mediaType: string): Promise<string> {
  return invoke<string>('save_ai_attachment', { dataBase64, mediaType });
}

/** Read back an attachment and return a data URL that can be put directly into <img src>; returns an empty string if the file is missing. */
export async function readAiAttachment(name: string): Promise<string> {
  return invoke<string>('read_ai_attachment', { name });
}

/** Append a session message to the local file ({AI data directory}/ai/sessions/{module}/{date}.jsonl). */
export async function appendAiSessionMessage(
  moduleId: string,
  date: string,
  message: AiSessionMessage,
): Promise<void> {
  return invoke<void>('append_ai_session_message', { moduleId, date, message });
}

/** Clear a module's session history for a given day. */
export async function clearAiSession(moduleId: string, date: string): Promise<void> {
  return invoke<void>('clear_ai_session', { moduleId, date });
}

/** Read a module's session history for a given day; returns an empty array when there is no history. */
export async function loadAiSession(moduleId: string, date: string): Promise<AiSessionMessage[]> {
  return invoke<AiSessionMessage[]>('load_ai_session', { moduleId, date });
}

/**
 * Read history by range: go back from `endDate` and stop once `maxMessages` entries are collected.
 *
 * Sessions are split into files by day, so "load earlier" naturally means "read a few more days back" — no date arithmetic needed.
 * The backend caps the count per call (200), so nothing the frontend passes can actually overload the read.
 */
export async function loadAiSessionRange(
  moduleId: string,
  endDate: string,
  maxMessages: number,
): Promise<AiSessionPage> {
  return invoke<AiSessionPage>('load_ai_session_range', { moduleId, endDate, maxMessages });
}

/**
 * Clear a module's sessions for **all dates** (the UI's "Clear Conversation"; the frontend already asks for confirmation twice).
 *
 * Difference from `clearAiSession` (which only deletes one day): history now loads across days,
 * so deleting only today would result in "cleared, but yesterday's came back".
 */
export async function clearAiModuleSession(moduleId: string): Promise<void> {
  return invoke<void>('clear_ai_module_session', { moduleId });
}

/** Offset `YYYY-MM-DD` by N days. Uses local time, consistent with `localDateKey`. */
export function shiftDateKey(dateKey: string, days: number): string {
  const [year, month, day] = dateKey.split('-').map(Number);
  return localDateKey(new Date(year, (month || 1) - 1, (day || 1) + days));
}
