// ============================================================================
// AI config read/write (multi-provider profiles)
//
// Location: {data dir}/ai/ai_settings.json (follows the main data directory, so portable builds can take it along)
//
// Structure:
//   { "activeProfileId": "p_xxx", "profiles": [ { ...a complete provider config } ], "dataDir": "" }
//
// Why "array + current selection" instead of a single config:
//   Users may have both an official API and a third-party relay, or use different models for different purposes.
//   With profiles, "adding another provider" is just adding an element, and "switching" only changes the activeProfileId field.
//
// About keys: the api_key field is encrypted with the current Windows user's DPAPI (stored as "dpapi:" + hex);
//   if encryption fails, the key falls back to plaintext and a warning is logged. Plaintext values from configs
//   saved before this version still work when read and are encrypted automatically on the next save.
//   Keys are **never returned to the frontend** — the frontend only gets the masked value (sk-****1234);
//   the UI layer cannot obtain a usable key. A copied config file is undecryptable for any other user or machine.
// ============================================================================

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// A provider profile: one key + one endpoint + one current model.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProfile {
    /// Stable id, used for switching and updates
    pub id: String,
    /// User-editable display name; when empty the UI falls back to the provider name
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub provider_id: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    /// **The last successfully fetched model list**.
    /// Must be cached: otherwise the UI shows only a lone model name, and users can neither see how many options there are
    /// nor judge whether that name is still valid (historical issue: early presets carried default_model,
    /// and after saving users kept seeing that name even when it was long gone from the key's available list).
    #[serde(default)]
    pub models: Vec<String>,
    /// When the model list was fetched (Unix seconds), 0 means never
    #[serde(default)]
    pub models_fetched_at: i64,
    /// Reasoning effort. **Leaving it empty means the parameter is not sent** — unsupported models may error out if it is.
    #[serde(default)]
    pub reasoning_effort: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiStoredConfig {
    #[serde(default)]
    pub active_profile_id: String,
    #[serde(default)]
    pub profiles: Vec<AiProfile>,
    /// AI data directory (chat history, etc.). **Global setting**, not tied to any provider —
    /// switching providers should not move the chat history as well.
    #[serde(default)]
    pub data_dir: String,
}

impl AiStoredConfig {
    /// The currently active profile. When the id is not found, fall back to the first —
    /// a config that exists but "cannot be used" is the hardest state to diagnose, so prefer falling back to a working one.
    pub fn active_profile(&self) -> Option<&AiProfile> {
        self.profiles
            .iter()
            .find(|profile| profile.id == self.active_profile_id)
            .or_else(|| self.profiles.first())
    }

    pub fn active_profile_mut(&mut self) -> Option<&mut AiProfile> {
        let target = self
            .profiles
            .iter()
            .position(|profile| profile.id == self.active_profile_id)
            .or(if self.profiles.is_empty() {
                None
            } else {
                Some(0)
            })?;
        self.profiles.get_mut(target)
    }
}

/// Profile view for the frontend: keys are masked only.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiPublicProfile {
    pub id: String,
    pub name: String,
    pub provider_id: String,
    pub base_url: String,
    pub model: String,
    pub api_key_masked: String,
    pub has_api_key: bool,
    pub models: Vec<String>,
    pub models_fetched_at: i64,
    pub reasoning_effort: String,
}

/// Overall view for the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiPublicConfig {
    pub active_profile_id: String,
    pub profiles: Vec<AiPublicProfile>,
    pub data_dir: String,
    /// **Resolved real directory** (the default location when config is empty).
    /// The UI must show "where exactly it is stored" and support one-click opening; the frontend cannot construct that on its own.
    pub resolved_data_dir: String,
}

/// Resolved AI data root: falls back to "data dir/ai" when config is empty.
/// Both chat persistence and "clear local data" use it, ensuring both always point to the same place.
pub fn resolve_data_dir() -> PathBuf {
    let configured = read_config().data_dir;
    let trimmed = configured.trim();
    if trimmed.is_empty() {
        crate::data_dir::get_data_dir().join("ai")
    } else {
        PathBuf::from(trimmed)
    }
}

/// Directory containing the config file: {data dir}/ai
fn config_dir() -> PathBuf {
    crate::data_dir::get_data_dir().join("ai")
}

/// Config file path.
pub fn config_path() -> PathBuf {
    config_dir().join("ai_settings.json")
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Migrate the old single-config structure into a profile.
///
/// The old file looked like: `{ "providerId": "...", "baseUrl": "...", "model": "...", "apiKey": "..." }`
/// No profiles field. **Migration is mandatory**: otherwise users' keys and models all "disappear" after upgrading.
fn migrate_legacy(value: &serde_json::Value) -> Option<AiStoredConfig> {
    let text = |key: &str| -> String {
        value
            .get(key)
            .and_then(|item| item.as_str())
            .unwrap_or_default()
            .to_string()
    };

    let api_key = text("apiKey");
    let provider_id = text("providerId");
    let base_url = text("baseUrl");
    let model = text("model");
    // If the old config has no key either, it was never configured; do not create an empty profile
    if api_key.trim().is_empty() && provider_id.trim().is_empty() {
        return None;
    }

    let profile = AiProfile {
        id: "p_migrated".to_string(),
        name: String::new(),
        provider_id,
        base_url,
        model,
        models: value
            .get("models")
            .and_then(|item| item.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .map(|item| item.to_string())
                    .collect()
            })
            .unwrap_or_default(),
        models_fetched_at: value
            .get("modelsFetchedAt")
            .and_then(|item| item.as_i64())
            .unwrap_or(0),
        api_key,
        reasoning_effort: String::new(),
    };

    Some(AiStoredConfig {
        active_profile_id: profile.id.clone(),
        profiles: vec![profile],
        data_dir: text("dataDir"),
    })
}

/// Prefix for DPAPI-encrypted keys.
///
/// **Anything without the prefix is treated as plaintext** — configs saved before this version need no migration code,
/// they still work when read and are automatically encrypted on the next save.
const DPAPI_PREFIX: &str = "dpapi:";

/// Binary → hex. Only to fit ciphertext into a JSON string; not worth adding a base64 dependency.
fn to_hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push_str(&format!("{:02x}", byte));
    }
    text
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    let chars: Vec<char> = text.chars().collect();
    for pair in chars.chunks(2) {
        let byte = u8::from_str_radix(&format!("{}{}", pair[0], pair[1]), 16).ok()?;
        bytes.push(byte);
    }
    Some(bytes)
}

/// Encrypt a key with the current Windows user's DPAPI (`CryptProtectData`).
///
/// **Encrypt only the api_key field**, leave everything else in the profile alone: another user / another machine cannot decrypt it,
/// which is exactly the right semantic for a key (decryption failure means treat as unconfigured and ask the user to re-enter it, rather than sending old ciphertext to the provider).
/// If encryption itself fails, **fall back to plaintext** and log it — degradation is acceptable, but users must be able to save a key.
fn protect_key(plain: &str) -> String {
    if plain.trim().is_empty() || plain.starts_with(DPAPI_PREFIX) {
        return plain.to_string();
    }
    match dpapi_protect(plain.as_bytes()) {
        Some(encrypted) => format!("{}{}", DPAPI_PREFIX, to_hex(&encrypted)),
        None => {
            log::warn!("DPAPI 加密 API Key 失败，本次以明文保存");
            plain.to_string()
        }
    }
}

/// Decrypt (`CryptUnprotectData`). No prefix = old plaintext config, returned as is.
/// Return an empty string on decryption failure: the UI shows "not configured" and the user re-enters it — never send ciphertext out as a key.
fn unprotect_key(stored: &str) -> String {
    let Some(encoded) = stored.strip_prefix(DPAPI_PREFIX) else {
        return stored.to_string();
    };
    let Some(encrypted) = from_hex(encoded) else {
        log::warn!("API Key 密文格式非法，按未配置处理");
        return String::new();
    };
    match dpapi_unprotect(&encrypted) {
        Some(plain) => String::from_utf8_lossy(&plain).to_string(),
        None => {
            log::warn!("DPAPI 解密 API Key 失败（可能换了用户或机器），按未配置处理");
            String::new()
        }
    }
}

fn dpapi_protect(plain: &[u8]) -> Option<Vec<u8>> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};

    let mut buffer = plain.to_vec();
    let input = CRYPT_INTEGER_BLOB {
        cbData: buffer.len() as u32,
        pbData: buffer.as_mut_ptr(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: input points at buffer, which lives for the whole call; output
    // receives an API-allocated buffer that is copied out and LocalFree'd below
    let result =
        unsafe { CryptProtectData(&input, PCWSTR::null(), None, None, None, 0, &mut output) };
    if result.is_err() {
        return None;
    }
    // SAFETY: on success the API guarantees output points at cbData bytes
    let encrypted =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    // SAFETY: the buffer was allocated by CryptProtectData with LocalAlloc semantics
    unsafe {
        LocalFree(Some(HLOCAL(output.pbData as *mut core::ffi::c_void)));
    }
    Some(encrypted)
}

fn dpapi_unprotect(encrypted: &[u8]) -> Option<Vec<u8>> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

    let mut buffer = encrypted.to_vec();
    let input = CRYPT_INTEGER_BLOB {
        cbData: buffer.len() as u32,
        pbData: buffer.as_mut_ptr(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: input points at buffer, which lives for the whole call; output
    // receives an API-allocated buffer that is copied out and LocalFree'd below
    let result = unsafe { CryptUnprotectData(&input, None, None, None, None, 0, &mut output) };
    if result.is_err() {
        return None;
    }
    // SAFETY: on success the API guarantees output points at cbData bytes
    let plain =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    // SAFETY: the buffer was allocated by CryptUnprotectData with LocalAlloc semantics
    unsafe {
        LocalFree(Some(HLOCAL(output.pbData as *mut core::ffi::c_void)));
    }
    Some(plain)
}

/// Before writing to disk, encrypt each profile's key (skip ones already encrypted, to avoid double encryption).
fn encrypt_keys(config: &mut AiStoredConfig) {
    for profile in &mut config.profiles {
        profile.api_key = protect_key(&profile.api_key);
    }
}

/// After reading from disk, decrypt each profile's key (plaintext without a prefix is kept as is).
fn decrypt_keys(config: &mut AiStoredConfig) {
    for profile in &mut config.profiles {
        profile.api_key = unprotect_key(&profile.api_key);
    }
}

/// Read config. Missing file or corrupt content falls back to defaults — unreadable config must not prevent the UI from opening.
pub fn read_config() -> AiStoredConfig {
    let mut config = read_config_raw();
    decrypt_keys(&mut config);
    config
}

/// The actual read logic (no encryption/decryption).
///
/// Split into a separate layer because `read_config` has several return points (default / new structure / old-structure migration),
/// so decryption in the outer layer only needs to happen once — future branches cannot forget it.
fn read_config_raw() -> AiStoredConfig {
    let path = config_path();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        // "Never configured" is a normal state; do not log it
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return AiStoredConfig::default();
        }
        Err(error) => {
            // ⚠️ File present but unreadable (locked / permissions / bad disk) **must not be silently treated as "no config"**:
            // users see "all my keys are gone" with no clue to diagnose.
            log::warn!(
                "读取 AI 配置失败，已回退默认值 {}: {}",
                path.display(),
                error
            );
            return AiStoredConfig::default();
        }
    };

    // Read as generic JSON first: this lets us tell old structure (needs migration) from new structure
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        log::warn!("AI 配置不是合法 JSON，已回退默认值: {}", path.display());
        return AiStoredConfig::default();
    };

    if value.get("profiles").is_some() {
        return serde_json::from_value(value).unwrap_or_else(|error| {
            log::warn!(
                "AI 配置解析失败，已回退默认值: {} ({})",
                path.display(),
                error
            );
            AiStoredConfig::default()
        });
    }

    // Old structure: migrate and **write back immediately**, so we do not judge it on every startup;
    // users may not click save again, and without writing back they stay in a "seemingly unconfigured" state.
    match migrate_legacy(&value) {
        Some(migrated) => {
            log::info!("检测到旧版 AI 配置，已迁移为单档案结构");
            if let Err(error) = save_config(migrated.clone()) {
                log::warn!("迁移后的配置写回失败（本次仍按迁移结果使用）: {}", error);
            }
            migrated
        }
        None => AiStoredConfig::default(),
    }
}

/// Write config.
///
/// **Must write atomically**: plain `fs::write` truncates first, so a crash or process kill mid-write leaves half a
/// JSON file — next read fails to parse → falls back to defaults → all keys and profiles silently vanish.
/// Use "write temp file → rename over target": rename on the same volume is atomic,
/// leaving either the complete old file or the complete new file, never an intermediate state.
pub fn save_config(mut config: AiStoredConfig) -> Result<(), String> {
    // Encrypt keys before writing (only this field). This modifies a copy —
    // the caller still holds plaintext, so no decryption is needed when returning to the command layer.
    encrypt_keys(&mut config);
    let directory = config_dir();
    fs::create_dir_all(&directory)
        .map_err(|error| format!("创建 AI 配置目录失败 {}: {}", directory.display(), error))?;

    let text = serde_json::to_string_pretty(&config)
        .map_err(|error| format!("序列化 AI 配置失败: {}", error))?;

    let path = config_path();
    // Temp name includes the process id: avoid collisions with another instance or another in-progress temp file
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&temporary, text)
        .map_err(|error| format!("写入 AI 配置失败 {}: {}", temporary.display(), error))?;
    fs::rename(&temporary, &path).map_err(|error| {
        // Even the rename failed; the temp file has no reason to remain, so clean it up rather than leaving junk in the config directory
        let _ = fs::remove_file(&temporary);
        format!("替换 AI 配置失败 {}: {}", path.display(), error)
    })?;
    Ok(())
}

/// Serialization gate for config read → modify → write.
///
/// Tauri commands can arrive concurrently, and every config-modifying function below is read_config → modify → save_config:
/// interleaving two of them loses updates (typical case: test connection caching a model list while the user clicks save profile).
/// A std Mutex suffices — these operations only read/write a tiny local file.
/// ⚠️ Do not call another gated function from inside a gated function: std's Mutex is not reentrant and will deadlock.
/// (`read_config` / `save_config` themselves do not hold the gate, so calling each other is safe.)
static CONFIG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Acquire the config gate. If the lock is poisoned (holder panicked), let it through as normal — this gate only prevents interleaving, it protects no invariant.
fn lock_config() -> std::sync::MutexGuard<'static, ()> {
    CONFIG_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Validate and normalize a profile (used by both create and update).
///
/// The frontend form already validates once, but the command layer is an **external surface** (any caller can pass anything),
/// so add another check here: a bad profile written into config shows up as "settings page looks filled in but questions keep failing".
fn validate_profile(mut profile: AiProfile) -> Result<AiProfile, String> {
    let preset = super::presets::list_providers()
        .into_iter()
        .find(|preset| preset.id == profile.provider_id);
    let Some(preset) = preset else {
        return Err(format!("未知的服务商: {}", profile.provider_id));
    };
    // Empty display name falls back to the provider name: the profile list should have no blank entries, or users cannot tell which one is active
    if profile.name.trim().is_empty() {
        profile.name = preset.name.to_string();
    }
    // Empty endpoint is **legal** (means use the preset default), but if provided it must be http(s)
    let base_url = profile.base_url.trim();
    if !base_url.is_empty()
        && !(base_url.starts_with("http://") || base_url.starts_with("https://"))
    {
        return Err(format!(
            "接口地址必须以 http:// 或 https:// 开头: {}",
            base_url
        ));
    }
    Ok(profile)
}

/// Generate a new profile id: second-level timestamp plus an incrementing suffix on collision.
///
/// ⚠️ Using only `p_{second timestamp}` collides when "two profiles are created within the same second" —
/// after a collision upsert matches the earlier one (the new key is written into the old profile) and deletion removes both.
fn next_profile_id(config: &AiStoredConfig) -> String {
    let base = format!("p_{}", now_seconds());
    if !config.profiles.iter().any(|profile| profile.id == base) {
        return base;
    }
    let mut suffix = 2;
    loop {
        let candidate = format!("{}_{}", base, suffix);
        if !config
            .profiles
            .iter()
            .any(|profile| profile.id == candidate)
        {
            return candidate;
        }
        suffix += 1;
    }
}

/// Merge the profile submitted by the frontend into existing profiles: only let fields the form can express take effect.
///
/// `models` / `models_fetched_at` are metadata cached by the backend during "test connection"; the frontend form
/// has neither field (`AiProfileInput` does not include them, so they deserialize to empty). Replacing wholesale would
/// clear the cache — the panel's dropdown would immediately degrade to plain text and users could no longer switch models,
/// only reopening the settings page and waiting for an automatic refetch. So these two fields must be carried over unchanged.
/// But when the provider / endpoint / key changes, the old list is no longer trustworthy; leave it empty so the settings page refetches.
fn merge_profile_update(existing: &mut AiProfile, incoming: AiProfile) {
    // Empty key = do not change the saved key (the frontend only has the masked value, it cannot send back plaintext)
    let keep_key = incoming.api_key.trim().is_empty();
    let previous_key = existing.api_key.clone();
    let same_endpoint = existing.provider_id == incoming.provider_id
        && existing.base_url == incoming.base_url
        && (keep_key || existing.api_key == incoming.api_key);
    let cached_models =
        same_endpoint.then(|| (existing.models.clone(), existing.models_fetched_at));

    *existing = incoming;
    if keep_key {
        existing.api_key = previous_key;
    }
    match cached_models {
        Some((models, fetched_at)) => {
            existing.models = models;
            existing.models_fetched_at = fetched_at;
        }
        // If the endpoint changes, invalidate explicitly: do not rely on the caller "happening not to include" the old list
        None => {
            existing.models.clear();
            existing.models_fetched_at = 0;
        }
    }
}

/// Create or update a profile (match by id). Return the updated overall config.
pub fn upsert_profile(profile: AiProfile) -> Result<AiStoredConfig, String> {
    let _guard = lock_config();
    let profile = validate_profile(profile)?;
    let mut config = read_config();
    match config
        .profiles
        .iter_mut()
        .find(|item| item.id == profile.id)
    {
        Some(existing) => merge_profile_update(existing, profile),
        None => {
            let mut created = profile;
            if created.id.trim().is_empty() {
                created.id = next_profile_id(&config);
            }
            // ⚠️ A newly created profile **must be set as current**.
            // Otherwise: the form in the UI always edits "the current profile", so a key entered after creating would be saved into the old profile,
            // and the new profile stays empty forever (a real bug found while reading the flow during implementation).
            config.active_profile_id = created.id.clone();
            config.profiles.push(created);
        }
    }
    save_config(config.clone())?;
    Ok(config)
}

/// Clear a saved key for a profile (keep the profile itself).
///
/// A separate entry point because the "empty key = no change" convention means users **cannot revoke** a leaked
/// key (they could only delete the whole profile). Revoking a key is a security operation and must be possible.
pub fn clear_profile_key(profile_id: &str) -> Result<AiStoredConfig, String> {
    let _guard = lock_config();
    let mut config = read_config();
    let Some(profile) = config
        .profiles
        .iter_mut()
        .find(|profile| profile.id == profile_id)
    else {
        return Err(format!("要清除密钥的服务商配置不存在: {}", profile_id));
    };
    profile.api_key.clear();
    save_config(config.clone())?;
    Ok(config)
}

/// Delete a profile. If the deleted one is current, automatically switch to the first remaining.
pub fn delete_profile(profile_id: &str) -> Result<AiStoredConfig, String> {
    let _guard = lock_config();
    let mut config = read_config();
    config.profiles.retain(|profile| profile.id != profile_id);
    if config.active_profile_id == profile_id {
        config.active_profile_id = config
            .profiles
            .first()
            .map(|profile| profile.id.clone())
            .unwrap_or_default();
    }
    save_config(config.clone())?;
    Ok(config)
}

/// Switch the current profile.
pub fn set_active_profile(profile_id: &str) -> Result<AiStoredConfig, String> {
    let _guard = lock_config();
    let mut config = read_config();
    if !config
        .profiles
        .iter()
        .any(|profile| profile.id == profile_id)
    {
        return Err(format!("要切换的服务商配置不存在: {}", profile_id));
    }
    config.active_profile_id = profile_id.to_string();
    save_config(config.clone())?;
    Ok(config)
}

/// Cache a model list into the **current profile**: called after a successful fetch.
/// Only change these two fields, keep everything else as is — this function must have no other side effects.
pub fn cache_fetched_models(models: Vec<String>) -> Result<(), String> {
    let _guard = lock_config();
    let mut config = read_config();
    let Some(profile) = config.active_profile_mut() else {
        return Ok(());
    };
    profile.models = models;
    profile.models_fetched_at = now_seconds();
    save_config(config)
}

/// Public view for the frontend: keys are masked only, plus a boolean indicating whether configured.
pub fn read_public_config() -> AiPublicConfig {
    let stored = read_config();
    AiPublicConfig {
        active_profile_id: stored.active_profile_id.clone(),
        profiles: stored
            .profiles
            .iter()
            .map(|profile| AiPublicProfile {
                id: profile.id.clone(),
                name: profile.name.clone(),
                provider_id: profile.provider_id.clone(),
                base_url: profile.base_url.clone(),
                model: profile.model.clone(),
                api_key_masked: mask_api_key(&profile.api_key),
                has_api_key: !profile.api_key.trim().is_empty(),
                models: profile.models.clone(),
                models_fetched_at: profile.models_fetched_at,
                reasoning_effort: profile.reasoning_effort.clone(),
            })
            .collect(),
        data_dir: stored.data_dir.clone(),
        resolved_data_dir: resolve_data_dir().to_string_lossy().to_string(),
    }
}

/// Masking rule: keep the first 3 and last 4 characters, with four fixed asterisks in between.
/// Too short (under 12 characters) is masked entirely, to avoid leaking the key itself.
pub fn mask_api_key(key: &str) -> String {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.chars().count() < 12 {
        return "****".to_string();
    }
    let prefix: String = trimmed.chars().take(3).collect();
    let suffix: String = trimmed
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{}****{}", prefix, suffix)
}

/// Global data directory setting (not part of any profile).
pub fn set_data_dir(data_dir: String) -> Result<AiStoredConfig, String> {
    let _guard = lock_config();
    let mut config = read_config();
    config.data_dir = data_dir;
    save_config(config.clone())?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Key encryption: locally encrypted keys decrypt back to the original; old plaintext passes through unchanged; undecryptable ones are treated as "not configured".
    #[test]
    fn api_key_round_trips_through_dpapi_and_legacy_plaintext_passes_through() {
        let encrypted = protect_key("sk-abcdefghijklmn");
        assert!(
            encrypted.starts_with(DPAPI_PREFIX),
            "加密后应带 dpapi: 前缀，实际 {}",
            encrypted
        );
        assert_ne!(encrypted, "sk-abcdefghijklmn", "密文不该还是明文");
        assert_eq!(unprotect_key(&encrypted), "sk-abcdefghijklmn");

        // Plaintext config from before the version upgrade: read as is, no migration code needed
        assert_eq!(unprotect_key("sk-legacy-plain-key"), "sk-legacy-plain-key");

        // Do not encrypt an empty key: otherwise we store an "encrypted empty string" that looks configured
        assert_eq!(protect_key(""), "");

        // Bad ciphertext (machine changed / file edited by hand): treat as not configured — never send ciphertext out as a key
        assert_eq!(unprotect_key("dpapi:zzzz"), "");
    }

    /// New profile ids must not collide: a collision makes upsert pick the wrong profile and deletion remove both.
    #[test]
    fn new_profile_ids_do_not_collide_within_the_same_second() {
        let mut config = AiStoredConfig {
            active_profile_id: String::new(),
            profiles: Vec::new(),
            data_dir: String::new(),
        };
        let first = next_profile_id(&config);
        config.profiles.push(AiProfile {
            id: first.clone(),
            ..Default::default()
        });
        let second = next_profile_id(&config);
        assert_ne!(first, second, "同一秒内第二份档案拿到了相同 id");
        assert!(
            second.starts_with(&first),
            "冲突时应带后缀，实际 {}",
            second
        );
    }

    /// Switching models in place in the panel saves the profile again; the frontend form does not include models,
    /// so merging must not wipe the cached list (or the dropdown disappears and users cannot switch a second time).
    #[test]
    fn updating_model_keeps_cached_model_list_and_api_key() {
        let mut existing = AiProfile {
            id: "p_1".into(),
            provider_id: "deepseek".into(),
            base_url: "https://api.deepseek.com".into(),
            model: "deepseek-flash".into(),
            api_key: "sk-secret".into(),
            models: vec!["deepseek-flash".into(), "deepseek-v4-pro".into()],
            models_fetched_at: 1_760_000_000,
            ..Default::default()
        };
        // Simulate a panel submit: only model changed, key left empty, no models / modelsFetchedAt
        let incoming = AiProfile {
            id: "p_1".into(),
            provider_id: "deepseek".into(),
            base_url: "https://api.deepseek.com".into(),
            model: "deepseek-v4-pro".into(),
            ..Default::default()
        };
        merge_profile_update(&mut existing, incoming);

        assert_eq!(existing.model, "deepseek-v4-pro");
        assert_eq!(existing.api_key, "sk-secret");
        assert_eq!(
            existing.models,
            vec![
                String::from("deepseek-flash"),
                String::from("deepseek-v4-pro")
            ]
        );
        assert_eq!(existing.models_fetched_at, 1_760_000_000);
    }

    /// After changing endpoint or key, the old list is untrustworthy and must be cleared so the settings page refetches.
    #[test]
    fn changing_endpoint_or_key_invalidates_cached_model_list() {
        let base = AiProfile {
            provider_id: "deepseek".into(),
            base_url: "https://api.deepseek.com".into(),
            api_key: "sk-old".into(),
            models: vec!["deepseek-flash".into()],
            models_fetched_at: 1_760_000_000,
            ..Default::default()
        };

        // Endpoint change: simulate frontend submit (form has no models field), empty key = keep old key
        let mut existing = base.clone();
        merge_profile_update(
            &mut existing,
            AiProfile {
                provider_id: base.provider_id.clone(),
                base_url: "https://api.openai.com".into(),
                model: base.model.clone(),
                ..Default::default()
            },
        );
        assert!(existing.models.is_empty(), "换地址后不该还留着旧模型列表");
        assert_eq!(existing.models_fetched_at, 0);
        assert_eq!(existing.api_key, "sk-old", "密钥留空时仍要沿用旧密钥");

        // Key change: even if the caller includes the old list, it must be invalidated
        let mut existing = base.clone();
        merge_profile_update(
            &mut existing,
            AiProfile {
                provider_id: base.provider_id.clone(),
                base_url: base.base_url.clone(),
                model: base.model.clone(),
                api_key: "sk-new".into(),
                models: base.models.clone(),
                models_fetched_at: base.models_fetched_at,
                ..Default::default()
            },
        );
        assert!(existing.models.is_empty(), "换密钥后不该还留着旧模型列表");
        assert_eq!(existing.models_fetched_at, 0);
        assert_eq!(existing.api_key, "sk-new");
    }

    /// Profile validation: reject unknown providers and invalid endpoints; empty display name falls back to the provider name.
    #[test]
    fn profile_validation_rejects_broken_configs_and_fills_name() {
        let unknown = validate_profile(AiProfile {
            provider_id: "not-a-provider".into(),
            ..Default::default()
        });
        assert!(unknown.is_err(), "未知服务商应被拒绝");

        let bad_url = validate_profile(AiProfile {
            provider_id: "deepseek".into(),
            base_url: "api.deepseek.com".into(),
            ..Default::default()
        });
        assert!(bad_url.is_err(), "缺协议头的地址应被拒绝");

        let ok = validate_profile(AiProfile {
            provider_id: "deepseek".into(),
            base_url: "https://api.deepseek.com".into(),
            ..Default::default()
        })
        .expect("合法档案应通过");
        assert!(!ok.name.trim().is_empty(), "显示名留空时应回落到服务商名");

        // Empty endpoint = use the preset default, must be allowed
        assert!(validate_profile(AiProfile {
            provider_id: "deepseek".into(),
            ..Default::default()
        })
        .is_ok());
    }

    #[test]
    fn legacy_single_config_migrates_into_one_profile() {
        // Old files have no profiles field; not migrating means all user config is lost
        let legacy = serde_json::json!({
            "providerId": "deepseek",
            "baseUrl": "https://api.deepseek.com",
            "model": "deepseek-chat",
            "apiKey": "sk-abcdefghijklmn",
            "models": ["deepseek-chat", "deepseek-reasoner"]
        });
        let migrated = migrate_legacy(&legacy).expect("有密钥就该迁移");
        assert_eq!(migrated.profiles.len(), 1);
        assert_eq!(migrated.profiles[0].model, "deepseek-chat");
        assert_eq!(migrated.profiles[0].models.len(), 2);
        // The migrated profile must be the current one, otherwise users save but nothing takes effect
        assert_eq!(migrated.active_profile_id, migrated.profiles[0].id);
    }

    #[test]
    fn empty_legacy_config_is_not_migrated_into_a_ghost_profile() {
        let legacy = serde_json::json!({});
        assert!(migrate_legacy(&legacy).is_none());
    }

    #[test]
    fn active_profile_falls_back_to_first_when_id_is_stale() {
        let config = AiStoredConfig {
            active_profile_id: "p_missing".to_string(),
            profiles: vec![AiProfile {
                id: "p_1".to_string(),
                provider_id: "deepseek".to_string(),
                ..Default::default()
            }],
            data_dir: String::new(),
        };
        assert_eq!(config.active_profile().map(|p| p.id.as_str()), Some("p_1"));
    }

    #[test]
    fn masking_keeps_edges_but_hides_the_middle() {
        assert_eq!(mask_api_key("sk-abcdefghijklmn"), "sk-****klmn");
        assert_eq!(mask_api_key("short"), "****");
        assert_eq!(mask_api_key(""), "");
    }
}
