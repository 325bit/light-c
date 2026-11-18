// ============================================================================
// AI assistant - backend capabilities
//
// Design constraints (full design see docs/dev/AI-assistant-design.md):
//
// 1. **Network requests are always made on the Rust side**; the API Key is never returned to the frontend.
//    The frontend can only submit "which model, what message" and cannot obtain the plaintext key - so even if the UI layer
//    is injected or the user opens dev tools, they cannot get the Key.
//
// 2. **Provider differences are concentrated in only two places**: presets.rs (preset table) and client.rs (protocol adaptation).
//    The rest of the code (config, command layer) need not know whether it is calling DeepSeek or Anthropic.
//
// 3. **Horizontally inserted**: this module does not modify any existing module's scan/cleanup logic, it only adds files;
//    it exposes commands to the outside only through commands/ai.rs.
// ============================================================================

mod attachment;
mod client;
mod config;
mod presets;
mod session;
mod stream;
mod tools;

// Public exports: only export what the command layer and frontend types actually use.
// Note AiStoredConfig contains a plaintext key, **only allowed to flow on the Rust side**; the command layer must not return it to the frontend.
pub use attachment::{read_attachment, save_attachment};
pub use client::{chat_once, chat_stream, fetch_models, AiChatReply, AiChatRequest};
pub use config::{
    cache_fetched_models, clear_profile_key, delete_profile, read_public_config,
    set_active_profile, set_data_dir, upsert_profile, AiProfile, AiPublicConfig,
};
pub use presets::{list_providers, AiProviderPreset};
pub use session::{
    append_message, clear_all_sessions, clear_session, load_session, load_session_range,
    sessions_root, AiSessionMessage, AiSessionPage,
};
