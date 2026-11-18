// ============================================================================
// AI assistant - frontend types
//
// One-to-one correspondence with the Rust structures under src-tauri/src/ai/ (camelCase).
// ⚠️ There is **no** key field here: the backend only returns the masked form, the frontend cannot get the plaintext Key.
// ============================================================================

/** The protocol family used by the provider. Currently only these two need to be distinguished; all other providers fall under OpenAI-compatible. */
export type AiProtocol = 'openAi' | 'anthropic';

/** Built-in provider presets (delivered by the backend, not hard-coded in the frontend). */
export interface AiProviderPreset {
  id: string;
  name: string;
  protocol: AiProtocol;
  /** Default API address (without a trailing slash) */
  baseUrl: string;
  /** Default model, used only as a hint */
  defaultModel: string;
  /** Local inference: the UI uses this to hint "data does not leave this machine" */
  local: boolean;
}

/** UI view of a provider profile: the key is masked. */
export interface AiPublicProfile {
  id: string;
  name: string;
  providerId: string;
  baseUrl: string;
  model: string;
  apiKeyMasked: string;
  hasApiKey: boolean;
  /** Model list from the last successful fetch */
  models: string[];
  modelsFetchedAt: number;
  /** Reasoning effort; an empty string means this parameter is not sent */
  reasoningEffort: string;
}

/** Config view returned by the backend to the UI: multiple profiles + the currently selected one. */
export interface AiPublicConfig {
  activeProfileId: string;
  profiles: AiPublicProfile[];
  /** Custom AI data directory; empty means follow the main data directory (global setting, not belonging to a profile) */
  dataDir: string;
  /** Resolved real directory, shown directly in the UI and supports one-click open */
  resolvedDataDir: string;
}

/**
 * Submit a profile.
 * `id` empty = add new; `apiKey` empty = do not change the saved key
 * (the frontend can only get the masked form and cannot send back the plaintext, so leaving it empty is the only way to express "no change").
 */
export interface AiProfileInput {
  /** Reasoning effort; an empty string means this parameter is not sent (default). Not all models support it */
  reasoningEffort: string;
  id: string;
  name: string;
  providerId: string;
  baseUrl: string;
  model: string;
  apiKey: string;
}

/** Chat message. role uses OpenAI's notation (system / user / assistant); Anthropic is translated by the backend. */
/**
 * Image attachment (already compressed, see components/ai/imageAttach.ts).
 * Only carried when **sending**: history and display are still based on content.
 */
export interface AiChatImage {
  /** For example image/jpeg */
  mediaType: string;
  /** Pure base64 without the data: prefix */
  dataBase64: string;
}

export interface AiChatMessage {
  role: 'system' | 'user' | 'assistant';
  content: string;
  /** Image attachment; when empty the request is sent as plain text, exactly the same as before this feature was added */
  images?: AiChatImage[];
  /**
   * The chain of thought for this turn in reasoning mode. **It must be sent back unchanged** to the server —
   * when a request carries tools, if the intermediate assistant turn's reasoning_content is not sent back, it directly returns 400.
   * It is not displayed and does not participate in the body text; it just travels with the message.
   */
  reasoningContent?: string;
  /**
   * The **profile id** that produced this reply. Chain of thought is only meaningful to the provider that produced it —
   * with another provider (especially a strict third-party gateway) it becomes an unknown field and may be rejected with 400,
   * so when building requests only send back those "produced by the current profile itself".
   */
  producedByProfile?: string;
  /**
   * This assistant message was **generated locally** (a tool query result), not something the model said.
   * When building a request it is treated as user-side content — otherwise it becomes "an assistant with no reasoning_content".
   */
  localToolOutput?: boolean;
  /**
   * The **actual model name** that produced this reply (the truth returned by the backend, displayed in the UI only).
   * Comparing entry by entry after switching models confirms whether it took effect, instead of blindly trusting the dropdown value in the input.
   * Not included in the request body: when the backend builds messages it only picks protocol fields, and UI-only fields are dropped.
   */
  model?: string;
  /**
   * Unix millisecond timestamp (**for UI use only**: used to insert date separators after loading across days).
   * Request construction does not include it — `toRequestMessage` only picks protocol fields like role/content.
   */
  at?: number;
}

/** A model request invoking a tool. */
export interface AiToolCall {
  callId: string;
  name: string;
  /** Arguments already parsed into an object; empty object if parsing fails */
  arguments: Record<string, unknown>;
}

export interface AiChatRequest {
  messages: AiChatMessage[];
  /** Override the model for a single call; if not provided, use the one saved in settings */
  model?: string;
  temperature?: number;
  /**
   * Tool names allowed for this request.
   * **Determined by the frontend based on "which actions this module registered"** — a module that only analyzes but does not delete will not pass
   * propose_delete, so the model cannot see that tool and has no way to propose deletion.
   */
  tools?: string[];
  /**
   * The conversation id for this chat. Generated by the frontend per module and changed on "Clear Conversation",
   * it only becomes a request header for providers that need session affinity; it is not a config item and is not persisted.
   */
  sessionId?: string;
}

export interface AiChatReply {
  content: string;
  model: string;
  /** The chain of thought for this turn in reasoning mode (not displayed); the frontend must store it on the next assistant message and send it back */
  reasoningContent?: string;
  /** When set, indicates the model wants to perform an action instead of replying */
  toolCalls?: AiToolCall[];
}
