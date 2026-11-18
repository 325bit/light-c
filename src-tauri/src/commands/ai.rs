// ============================================================================
// AI assistant commands
//
// This layer only does three things: shape the parameters, call the ai module, and turn errors into
// `AppError` values (code + args, never a user-facing sentence). The real business logic is all under
// src/ai/ — do not write it here.
// ============================================================================

use crate::ai::{
    AiChatReply, AiChatRequest, AiProfile, AiProviderPreset, AiPublicConfig, AiSessionMessage,
};
use crate::error::AppError;

/// Return the built-in provider presets, for the settings page to render dropdowns.
#[tauri::command]
pub fn list_ai_providers() -> Vec<AiProviderPreset> {
    crate::ai::list_providers()
}

/// Read the current AI config. **Keys are returned masked**; the plaintext never leaves the backend.
#[tauri::command]
pub fn get_ai_config() -> AiPublicConfig {
    crate::ai::read_public_config()
}

/// Save the AI config.
///
/// Convention: when the apiKey sent by the frontend is an empty string it means "do not change the saved key" —
/// otherwise, whenever a user only wanted to change the model, they would have to paste the key again.
#[tauri::command]
pub fn save_ai_profile(profile: AiProfile) -> Result<AiPublicConfig, AppError> {
    crate::ai::upsert_profile(profile)?;
    Ok(crate::ai::read_public_config())
}

/// Clear the saved key for a profile (the profile is kept). Users must be able to revoke it if the key leaks.
#[tauri::command]
pub fn clear_ai_profile_key(profile_id: String) -> Result<AiPublicConfig, AppError> {
    crate::ai::clear_profile_key(&profile_id)?;
    Ok(crate::ai::read_public_config())
}

/// Delete a provider profile. If the deleted one was the current profile, the backend automatically switches to the first remaining one.
#[tauri::command]
pub fn delete_ai_profile(profile_id: String) -> Result<AiPublicConfig, AppError> {
    crate::ai::delete_profile(&profile_id)?;
    Ok(crate::ai::read_public_config())
}

/// Switch the currently used provider profile.
#[tauri::command]
pub fn set_active_ai_profile(profile_id: String) -> Result<AiPublicConfig, AppError> {
    crate::ai::set_active_profile(&profile_id)?;
    Ok(crate::ai::read_public_config())
}

/// Set the AI data directory (global, not belonging to any provider — switching providers should not move the chat history).
#[tauri::command]
pub fn set_ai_data_dir(data_dir: String) -> Result<AiPublicConfig, AppError> {
    crate::ai::set_data_dir(data_dir)?;
    Ok(crate::ai::read_public_config())
}

/// Test connectivity: successfully fetching a model list means the address, key and network are all working.
#[tauri::command]
pub async fn test_ai_connection() -> Result<Vec<String>, AppError> {
    let models = crate::ai::fetch_models().await?;
    // If fetched, cache it: both the settings page and the panel need to show "which models are available",
    // it should not be visible only in this one session. A cache failure does not affect the success conclusion of this test.
    if let Err(error) = crate::ai::cache_fetched_models(models.clone()) {
        log::warn!("缓存模型列表失败: {}", error);
    }
    Ok(models)
}

/// Send one chat message (non-streaming).
/// For the streaming version see ai_chat_stream; the non-streaming one is kept to help diagnose "whether it is a streaming parsing problem".
#[tauri::command]
pub async fn ai_chat(request: AiChatRequest) -> Result<AiChatReply, AppError> {
    crate::ai::chat_once(request).await.map_err(AppError::from)
}

/// Payload pushed for streaming increments.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiStreamDelta {
    /// Same as the one passed when starting the request; the frontend filters by it (only one stream is allowed at a time; adding this is more robust)
    pub stream_id: String,
    pub text: String,
}

/// Save an image attachment in a chat, returning the file name (the frontend writes it into the session record).
#[tauri::command]
pub fn save_ai_attachment(data_base64: String, media_type: String) -> Result<String, AppError> {
    crate::ai::save_attachment(&data_base64, &media_type).map_err(AppError::from)
}

/// Read back an attachment, returning a data URL that can be put directly into <img src>; return an empty string if the file is not there.
#[tauri::command]
pub fn read_ai_attachment(name: String) -> Result<String, AppError> {
    crate::ai::read_attachment(&name).map_err(AppError::from)
}

/// Append a session message.
/// The module id and date are given by the frontend (the backend introduces no date library for this); semantic and security validation is in ai::session.
#[tauri::command]
pub fn append_ai_session_message(
    module_id: String,
    date: String,
    message: AiSessionMessage,
) -> Result<(), AppError> {
    crate::ai::append_message(&module_id, &date, message).map_err(AppError::from)
}

/// Clear the chat history for a module on a given day (the UI's "Clear Conversation").
/// Only delete that day's files: earlier history is left to "clean by date"; no implicit large-scale deletion.
#[tauri::command]
pub fn clear_ai_session(module_id: String, date: String) -> Result<(), AppError> {
    crate::ai::clear_session(&module_id, &date).map_err(AppError::from)
}

/// Read the chat history for a module on a given day. A missing file returns an empty list ("no history yet" is a normal state).
#[tauri::command]
pub fn load_ai_session(module_id: String, date: String) -> Result<Vec<AiSessionMessage>, AppError> {
    crate::ai::load_session(&module_id, &date).map_err(AppError::from)
}

/// Read history by range: read backwards from `end_date`, stop once `max_messages` have been collected.
///
/// The UI uses this to load "the most recent N" (across days); "Load Earlier" also goes through it —
/// the per-call count is capped in `ai::session`, **preventing a single read from blowing up at the source**.
#[tauri::command]
pub fn load_ai_session_range(
    module_id: String,
    end_date: String,
    max_messages: usize,
) -> Result<crate::ai::AiSessionPage, AppError> {
    crate::ai::load_session_range(&module_id, &end_date, max_messages).map_err(AppError::from)
}

/// Clear a module's chat history for **all dates** (the UI's "Clear Conversation", the frontend has already asked for a second confirmation).
///
/// Difference from `clear_ai_session` above (which only deletes one day): history is now loaded across days,
/// so deleting only the current day results in "cleared, but yesterday's came back"; therefore "Clear Conversation" goes through this one.
#[tauri::command]
pub fn clear_ai_module_session(module_id: String) -> Result<(), AppError> {
    crate::ai::clear_all_sessions(&module_id).map_err(AppError::from)
}

/// Streaming chat: increments are pushed via events, and the command finally returns the full body.
///
/// Why use events rather than "the return value is the stream": a Tauri command can only return a one-off result,
/// so to "display while generating" we must actively push to the frontend during generation.
/// The full body returned by the command is also a fallback — even if the frontend misses events, no content is lost.
#[tauri::command]
pub async fn ai_chat_stream(
    app: tauri::AppHandle,
    request: AiChatRequest,
    stream_id: String,
) -> Result<AiChatReply, AppError> {
    use tauri::Emitter;

    let emitter = app.clone();
    let event_stream_id = stream_id.clone();
    crate::ai::chat_stream(request, move |delta: &str| {
        // Event send failure is not treated as a fatal error: the command's return value still contains the full body.
        let _ = emitter.emit(
            "ai-stream-delta",
            AiStreamDelta {
                stream_id: event_stream_id.clone(),
                text: delta.to_string(),
            },
        );
    })
    .await
    .map_err(AppError::from)
}
