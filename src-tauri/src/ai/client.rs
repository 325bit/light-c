// ============================================================================
// AI client: send a chat request, fetch the model list
//
// Both protocols are unified here — upper layers only submit an AiChatRequest and do not care whether the backend is DeepSeek or
// Claude. Auth headers, request body shape, response parsing, and timeouts are all centralized in this file,
// keeping these details from leaking into the command or config layers.
// ============================================================================

use super::config::read_config;
use super::presets::{find_preset, AiProtocol};
use super::stream::StreamDecoder;
use super::tools::{
    anthropic_tools, extract_inline_tool_calls, openai_tools, parse_tool_calls, AiToolCall,
    TOOL_READ_ITEMS, TOOL_READ_ITEM_DETAIL,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;
/// Chat timeout: LLM time-to-first-token can be long, so keep it generous.
const CHAT_TIMEOUT_SECS: u64 = 60;
/// Fetching the model list is a lightweight request; keep the timeout tight so the settings page "Test Connection" does not hang too long.
const MODELS_TIMEOUT_SECS: u64 = 20;
/// Overall timeout for streaming chat (a fallback ceiling). reqwest's timeout covers 'until the response body is fully read',
/// using the 60-second chat timeout would cut long answers off mid-way, so streaming gets a looser one.
/// ⚠️ What actually detects a "stall" is the **idle timeout** below; this only prevents a connection from never ending.
const STREAM_TIMEOUT_SECS: u64 = 600;
/// Streaming **idle timeout**: if no new data arrives for this long, treat it as stalled and disconnect.
/// 30 seconds covers a provider's time-to-first-token (reasoning models sometimes take over ten seconds) without making the user wait too long.
const STREAM_IDLE_TIMEOUT_SECS: u64 = 30;
/// The Duration form of the value above (used on every loop iteration; no need to convert repeatedly)
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(STREAM_IDLE_TIMEOUT_SECS);
/// Anthropic requires max_tokens explicitly; give a sensible default here.
///
/// ⚠️ Do not set it too high: exceeding the model's own output limit is rejected outright (limits differ across Claude models,
/// smaller ones only go to 4096). 4096 is accepted by all Claude models in use —
/// the old 2048 was too tight; a single reply including a page of details would get truncated.
const ANTHROPIC_MAX_TOKENS: u32 = 4096;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChatMessage {
    pub role: String,
    pub content: String,
    /// Image attachments. When empty, send as plain text — **exactly the same as requests before this feature was added**.
    ///
    /// Why not put base64 directly into content: both protocols require text and images **as separate parts**,
    /// and the frontend must show thumbnails and replace them with placeholders when persisting, so handling them separately is simpler.
    #[serde(default)]
    pub images: Vec<AiImage>,
    /// Chain of thought in thinking mode. **Must be passed back verbatim** to OpenAI-compatible providers.
    ///
    /// DeepSeek requires: when a request carries `tools`, the intermediate assistant turn's reasoning_content
    /// must be passed back in full on all subsequent requests; omitting it yields a 400
    ///（"The reasoning_content in the thinking mode must be passed back to the API"）。
    /// Only meaningful on assistant messages — the frontend gets it from `AiChatReply` and passes it back unchanged.
    #[serde(default)]
    pub reasoning_content: Option<String>,
}

/// Image attachments on a message. The frontend is responsible for **compressing before sending** —
/// raw base64 would inflate every request by several MB (history is resent on every turn).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiImage {
    /// e.g. image/jpeg
    pub media_type: String,
    /// pure base64 without the `data:` prefix
    pub data_base64: String,
}

/// Content parts for the OpenAI-compatible protocol: a plain string for text-only, `image_url` parts when images are present.
fn build_openai_content(message: &AiChatMessage) -> serde_json::Value {
    if message.images.is_empty() {
        return serde_json::json!(message.content);
    }
    let mut parts: Vec<serde_json::Value> = Vec::new();
    if !message.content.trim().is_empty() {
        parts.push(serde_json::json!({ "type": "text", "text": message.content }));
    }
    for image in &message.images {
        parts.push(serde_json::json!({
            "type": "image_url",
            "image_url": { "url": format!("data:{};base64,{}", image.media_type, image.data_base64) },
        }));
    }
    serde_json::json!(parts)
}

/// Content parts for the Anthropic protocol: images use `source.base64`, not a data URI.
fn build_anthropic_content(message: &AiChatMessage) -> serde_json::Value {
    if message.images.is_empty() {
        return serde_json::json!(message.content);
    }
    let mut parts: Vec<serde_json::Value> = Vec::new();
    if !message.content.trim().is_empty() {
        parts.push(serde_json::json!({ "type": "text", "text": message.content }));
    }
    for image in &message.images {
        parts.push(serde_json::json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": image.media_type,
                "data": image.data_base64,
            },
        }));
    }
    serde_json::json!(parts)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChatRequest {
    pub messages: Vec<AiChatMessage>,
    /// Override the model for a single call; if omitted, use the one saved in settings
    #[serde(default)]
    pub model: Option<String>,
    /// Sampling temperature; if omitted, use the provider default
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Tool names allowed for this call.
    /// **Determined by the frontend based on "which actions the module registered"** — a module that only analyzes and never deletes will not send
    /// propose_delete, so the model never even sees this tool and cannot propose a deletion.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Session id for this chat. Generated by the frontend per module; only becomes a request header for providers that need session affinity.
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChatReply {
    pub content: String,
    pub model: String,
    /// This turn's chain of thought (only in thinking mode). The frontend stores it on the next assistant message,
    /// and subsequent requests must pass it back verbatim — otherwise the provider returns 400.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    /// Tool the model requested to call. When present, content is usually empty — it is not a "reply" but an action intent.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<AiToolCall>,
}

/// Joins an endpoint URL, tolerating base_url with or without a trailing slash.
fn join_url(base_url: &str, path: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    let suffix = path.trim_start_matches('/');
    format!("{}/{}", trimmed, suffix)
}

/// Whether an address points to the local machine.
///
/// String check only: `base_url` is entered by the user and will not have strange structure;
/// and pulling in URL parsing just for this one check is not worth it.
fn is_local_endpoint(base_url: &str) -> bool {
    let lower = base_url.to_ascii_lowercase();
    lower.contains("127.0.0.1")
        || lower.contains("localhost")
        || lower.contains("[::1]")
        || lower.contains("://0.0.0.0")
}

fn http_client(timeout_secs: u64, base_url: &str) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        // Use our own client name instead of reqwest's default UA: some gateways validate client identity by UA,
        // a generic HTTP library name is easily treated as script traffic (OpenCode Console's docs explicitly require this too).
        .user_agent(format!("LightC/{}", env!("CARGO_PKG_VERSION")));

    // ⚠️ Local inference must bypass the system proxy.
    // reqwest reads system/environment proxies by default; if the user has a proxy like Clash enabled,
    // requests to 127.0.0.1:11434 get routed to the proxy, which returns 502 —
    // the UI shows "provider temporarily unavailable" while the local model never received a single request.
    // Cloud requests are left unchanged; users in mainland China still reach external services through the proxy.
    if is_local_endpoint(base_url) {
        builder = builder.no_proxy();
    }

    builder
        .build()
        .map_err(|error| format!("创建网络请求客户端失败: {}", error))
}

/// Get the "currently effective" connection info: protocol from the preset + address/key/model from config.
/// When the address is empty, fall back to the preset default; users can get by with just entering a key.
/// Attach the "reasoning effort" configured in the profile to the request body.
///
/// ⚠️ **Cannot auto-detect which models support it**: `/models` only returns model names, no capability info.
/// So the approach is "send only if the user set it", and only over the OpenAI-compatible protocol — the field name most mainland providers
/// use; Anthropic's thinking is a `thinking` object with a completely different shape, so do not guess here.
///
/// Models that do not support this field either ignore it or error out. In the latter case the user sees the raw message in the UI,
/// and can simply set the option back to "default" — more reliable than guessing for them.
fn attach_reasoning_effort(body: &mut serde_json::Value, protocol: AiProtocol) {
    if protocol != AiProtocol::OpenAi {
        return;
    }
    let Some(effort) = read_config()
        .active_profile()
        .map(|profile| profile.reasoning_effort.trim().to_string())
    else {
        return;
    };
    if effort.is_empty() {
        return;
    }
    body["reasoning_effort"] = serde_json::json!(effort);
}

fn resolve_connection() -> Result<(AiProtocol, String, String, String), String> {
    let stored = read_config();
    // Everything follows the "current profile": when the user switches providers, the next call should use the new set
    let profile = stored
        .active_profile()
        .ok_or_else(|| "还没有添加 AI 服务商，请到「设置 - AI 助手」里添加。".to_string())?;
    let preset = find_preset(&profile.provider_id);

    // The API key may be empty: local inference (Ollama / LM Studio / vLLM) needs no auth at all.
    // Do not block here anymore; instead "empty means no auth header" — a missing key for a cloud provider yields a clearer 401 message.
    let api_key = profile.api_key.trim().to_string();

    let base_url = if profile.base_url.trim().is_empty() {
        preset.base_url.to_string()
    } else {
        profile.base_url.trim().to_string()
    };
    if base_url.is_empty() {
        return Err("还没有填写接口地址，请到「设置 - AI 助手」里填写。".to_string());
    }

    let model = if profile.model.trim().is_empty() {
        preset.default_model.to_string()
    } else {
        profile.model.trim().to_string()
    };

    Ok((preset.protocol, base_url, api_key, model))
}

// ---------------------------------------------------------------------------
// Request body construction (extracted as pure functions for unit testing; no network dependency)
// ---------------------------------------------------------------------------

/// OpenAI-compatible protocol: system and user/assistant are mixed in the same messages array.
fn build_openai_body(request: &AiChatRequest, model: &str) -> serde_json::Value {
    let messages: Vec<serde_json::Value> = request
        .messages
        .iter()
        .map(|message| {
            let mut value = serde_json::json!({
                "role": message.role,
                "content": build_openai_content(message),
            });
            // Thinking mode: requests with tools must include the intermediate assistant turn's reasoning_content
            // as well; omitting it yields a 400 from the provider (see AiChatMessage.reasoning_content).
            // Only add it to assistant messages — putting it on user/system is meaningless and may be rejected.
            if message.role == "assistant" {
                if let Some(reasoning) = message.reasoning_content.as_deref() {
                    if !reasoning.trim().is_empty() {
                        value["reasoning_content"] = serde_json::json!(reasoning);
                    }
                }
            }
            value
        })
        .collect();

    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false,
    });
    if let Some(temperature) = request.temperature {
        body["temperature"] = serde_json::json!(temperature);
    }
    body
}

/// Anthropic protocol: **system is not in messages**; it must be hoisted to the top level;
/// and max_tokens is required.
fn build_anthropic_body(request: &AiChatRequest, model: &str) -> serde_json::Value {
    let mut system_parts: Vec<String> = Vec::new();
    let mut messages: Vec<serde_json::Value> = Vec::new();

    for message in &request.messages {
        if message.role == "system" {
            system_parts.push(message.content.clone());
            continue;
        }
        messages.push(serde_json::json!({
            "role": message.role,
            "content": build_anthropic_content(message),
        }));
    }

    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": ANTHROPIC_MAX_TOKENS,
        "messages": messages,
    });
    if !system_parts.is_empty() {
        body["system"] = serde_json::json!(system_parts.join("\n\n"));
    }
    if let Some(temperature) = request.temperature {
        body["temperature"] = serde_json::json!(temperature);
    }
    body
}

// ---------------------------------------------------------------------------
// Response parsing (also pure functions)
// ---------------------------------------------------------------------------

/// OpenAI-compatible: choices[0].message.content
fn parse_openai_reply(value: &serde_json::Value) -> Option<String> {
    value
        .get("choices")?
        .get(0)?
        .get("message")?
        .get("content")?
        .as_str()
        .map(|text| text.to_string())
}

/// Chain of thought in non-streaming responses. Same constraint as the streaming version (stream.rs): must be passed back, otherwise 400.
fn parse_openai_reasoning(value: &serde_json::Value) -> Option<String> {
    value
        .get("choices")?
        .get(0)?
        .get("message")?
        .get("reasoning_content")?
        .as_str()
        .map(|text| text.to_string())
}

/// Anthropic: content is an array; take the parts with type == "text" and join them.
fn parse_anthropic_reply(value: &serde_json::Value) -> Option<String> {
    let blocks = value.get("content")?.as_array()?;
    let text: String = blocks
        .iter()
        .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("text"))
        .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
        .collect::<Vec<_>>()
        .join("");
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Model list: both protocols return `{ "data": [ { "id": ... } ] }`, so this can be shared.
fn parse_model_ids(value: &serde_json::Value) -> Vec<String> {
    value
        .get("data")
        .and_then(|data| data.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("id").and_then(|id| id.as_str()))
                .map(|id| id.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Turn the provider's error body into plain language, instead of dumping a huge JSON blob on the user.
/// `api_key_missing`: whether this request carried no key at all.
/// Used to distinguish "not filled in at all" from "filled in wrong" — the user must do different things in each case.
fn describe_http_error(status: u16, body: &str, api_key_missing: bool) -> String {
    // The provider returns a JSON blob; dumping the whole thing on the user is no help.
    // First extract the message field — that line is usually the actually useful information.
    let detail = extract_error_message(body);
    match status {
        // No key + 401: most likely the user is treating a cloud provider like a local model.
        // Do not just say "check that the Key is correct" — the user would go looking for a key that does not exist.
        401 | 403 if api_key_missing => format!(
            "这个服务商要求填写 API Key（{}）：本地模型可以留空，云端服务商需要到「设置 - AI 助手」里填写。{}",
            status, detail
        ),
        401 | 403 => format!("鉴权失败（{}），请检查 API Key 是否正确、是否已开通该模型。{}", status, detail),
        404 => format!("接口地址不对（404），请检查「接口地址」是否填成了正确的服务商地址。{}", detail),
        429 => format!("请求太频繁或额度不足（429）。{}", detail),
        // 5xx is a **provider-side** problem, not local and not a config error.
        // Say this clearly so users do not keep changing settings (in a real 503 the error text even suggests "switch providers").
        500..=599 => format!(
            "服务商暂时不可用（{}），稍后重试通常就好；持续不行的话可以换一个服务商。{}",
            status, detail
        ),
        _ => format!("请求失败（{}）：{}", status, detail),
    }
}

/// Extract a human-readable message from an error response.
///
/// More than one shape occurs: OpenAI-compatible is `{"error":{"message":"..."}}`,
/// Anthropic is `{"error":{"type":"...","message":"..."}}`,
/// and sometimes plain `{"message":"..."}`. If nothing is found, fall back to the raw text (truncated); **never return an empty string** —
/// better to give raw content than let the user see only "request failed".
fn extract_error_message(body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        let error = value.get("error");
        // Shape one: {"error": {"message": "..."}} (both OpenAI-compatible and Anthropic use this)
        if let Some(text) = error
            .and_then(|item| item.get("message"))
            .and_then(|item| item.as_str())
        {
            if !text.trim().is_empty() {
                return text.trim().to_string();
            }
        }
        // Shape two: {"error": "..."} (a few providers return a plain string)
        if let Some(text) = error.and_then(|item| item.as_str()) {
            if !text.trim().is_empty() {
                return text.trim().to_string();
            }
        }
        // Shape three: {"message": "..."} / {"detail": "..."}
        for key in ["message", "detail"] {
            if let Some(text) = value.get(key).and_then(|item| item.as_str()) {
                if !text.trim().is_empty() {
                    return text.trim().to_string();
                }
            }
        }
        // No message either (some providers only give a code); report the code to aid troubleshooting
        if let Some(code) = error
            .and_then(|item| item.get("code"))
            .and_then(|item| item.as_str())
        {
            return format!("错误代码：{}", code);
        }
    }
    body.chars().take(200).collect()
}

// ---------------------------------------------------------------------------
// Steps shared by streaming and non-streaming
//
// The two paths are identical except for "how the response is received": build the body, send the request, parse at the end.
// These three steps used to be duplicated — with real consequences: after adding "return only tools declared this round",
// it only took effect in the streaming copy. Every duplicate is one more place that can be forgotten during sync.
// ---------------------------------------------------------------------------

/// Build the request body: choose shape by protocol + attach tools declared this round + sampling parameters.
/// `streaming` only affects the `stream` field; everything else is identical.
fn build_request_body(
    request: &AiChatRequest,
    protocol: AiProtocol,
    model: &str,
    streaming: bool,
) -> serde_json::Value {
    let mut body = match protocol {
        AiProtocol::OpenAi => build_openai_body(request, model),
        AiProtocol::Anthropic => build_anthropic_body(request, model),
    };
    if streaming {
        body["stream"] = serde_json::json!(true);
    }

    // Tool declarations and "whether to stream" are independent: under streaming, tool arguments arrive in fragments,
    // reassembled by index in stream.rs, so declare them as usual — no need to give up streaming for tools.
    if !request.tools.is_empty() {
        let declarations = match protocol {
            AiProtocol::OpenAi => openai_tools(&request.tools),
            AiProtocol::Anthropic => anthropic_tools(&request.tools),
        };
        if !declarations.is_empty() {
            body["tools"] = serde_json::json!(declarations);
        }
    }

    attach_reasoning_effort(&mut body, protocol);
    body
}

/// Whether to attach the OpenCode Console session header to this request.
///
/// Only recognize `opencode.ai` and its subdomains: sending an unfamiliar header to other providers is pointless and may be rejected by strict gateways.
/// Take the host as only the part after the scheme and before the first path separator, to avoid `opencode.ai.evil.com`
/// slipping into the whitelist via such a suffix.
fn opencode_session_header<'a>(
    base_url: &str,
    session_id: Option<&'a str>,
) -> Option<(&'static str, &'a str)> {
    let id = session_id?.trim();
    if id.is_empty() {
        return None;
    }
    let host = base_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base_url)
        .split('/')
        .next()?
        .split(':')
        .next()?
        .to_ascii_lowercase();
    (host == "opencode.ai" || host.ends_with(".opencode.ai")).then_some(("x-opencode-session", id))
}

/// Add auth headers to a request; when the key is empty, **return it unchanged with no auth header**.
///
/// Local models (Ollama, LM Studio, vLLM) need no key — that is a prerequisite for them to work.
/// Extracted into a function so both call paths (chat, fetch models) share the same decision,
/// and so unit tests can directly verify "an empty key produces no Authorization".
fn apply_auth(
    request: reqwest::RequestBuilder,
    protocol: AiProtocol,
    api_key: &str,
) -> reqwest::RequestBuilder {
    if api_key.is_empty() {
        return request;
    }
    match protocol {
        AiProtocol::OpenAi => request.bearer_auth(api_key),
        AiProtocol::Anthropic => request.header("x-api-key", api_key),
    }
}

/// Sends a chat request per protocol: the two differ only in path and auth header.
async fn send_chat_request(
    client: &reqwest::Client,
    protocol: AiProtocol,
    base_url: &str,
    api_key: &str,
    session_id: Option<&str>,
    body: &serde_json::Value,
) -> Result<reqwest::Response, String> {
    // match only produces a builder: session header and body are identical across protocols, so no need to duplicate .send()
    // Auth headers go through apply_auth uniformly: an empty key must not send `Authorization: Bearer ` (empty value),
    // some services treat an empty Bearer as bad credentials and return 401, worse than not sending the header.
    let request = match protocol {
        AiProtocol::OpenAi => apply_auth(
            client.post(join_url(base_url, "/chat/completions")),
            protocol,
            api_key,
        ),
        AiProtocol::Anthropic => apply_auth(
            client
                .post(join_url(base_url, "/v1/messages"))
                .header("anthropic-version", "2023-06-01"),
            protocol,
            api_key,
        ),
    };
    let request = match opencode_session_header(base_url, session_id) {
        Some((name, value)) => request.header(name, value),
        None => request,
    };
    request
        .json(body)
        .send()
        .await
        .map_err(describe_request_error)
}

/// Whether the error message explicitly names `reasoning_effort`.
///
/// Only retry without it when it is **explicitly named** — otherwise other 400s (wrong model name, insufficient balance) get retried too,
/// wasting a request and masking the real error.
fn error_blames_reasoning_effort(error_text: &str) -> bool {
    error_text.to_ascii_lowercase().contains("reasoning_effort")
}

/// Report reqwest errors together with their **underlying causes**.
///
/// ⚠️ reqwest's `Display` only gives `error sending request for url (…)`,
/// the real cause (connection reset / handshake failure / DNS / timeout) is in the `source()` chain —
/// logging only the first layer leaves users and debuggers guessing from that single line (encountered in practice).
fn describe_request_error(error: reqwest::Error) -> String {
    let mut parts = vec![error.to_string()];
    let mut cause = std::error::Error::source(&error);
    while let Some(current) = cause {
        parts.push(current.to_string());
        cause = current.source();
    }
    format!("网络请求失败: {}", parts.join(" → "))
}

/// Sends one request; **retries once on send-phase failure**.
///
/// A single network blip (connection reset, handshake failure) should not make the user retype the problem.
/// On a send failure the request never reached the provider, so retrying is safe — no double billing.
async fn send_chat_request_with_retry(
    client: &reqwest::Client,
    protocol: AiProtocol,
    base_url: &str,
    api_key: &str,
    session_id: Option<&str>,
    body: &serde_json::Value,
) -> Result<reqwest::Response, String> {
    match send_chat_request(client, protocol, base_url, api_key, session_id, body).await {
        Ok(response) => Ok(response),
        Err(first_error) => {
            log::info!("首次发送失败，重试一次：{}", first_error);
            // The retry must use the same session id: changing it would split one conversation into two
            send_chat_request(client, protocol, base_url, api_key, session_id, body).await
        }
    }
}

/// Sends a request; if the provider explicitly rejects `reasoning_effort`, remove it and try once more.
///
/// Why this deserves its own handling: reasoning effort is **entered by the user**, and only a limited set of models supports it.
/// Making the user eat a 400 and then go guess "which field to clear" in settings is a poor experience.
/// Also: all non-2xx become Err here — streaming and non-streaming share the same status handling, no duplication.
async fn send_with_reasoning_fallback(
    client: &reqwest::Client,
    protocol: AiProtocol,
    base_url: &str,
    api_key: &str,
    session_id: Option<&str>,
    body: &serde_json::Value,
) -> Result<reqwest::Response, String> {
    let response =
        send_chat_request_with_retry(client, protocol, base_url, api_key, session_id, body).await?;
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(response);
    }

    let text = response.text().await.unwrap_or_default();
    if body.get("reasoning_effort").is_none() || !error_blames_reasoning_effort(&text) {
        return Err(describe_http_error(status, &text, api_key.is_empty()));
    }

    log::info!("服务端不认 reasoning_effort，去掉该参数重试一次");
    let mut retry_body = body.clone();
    if let Some(object) = retry_body.as_object_mut() {
        object.remove("reasoning_effort");
    }

    let retry =
        send_chat_request_with_retry(client, protocol, base_url, api_key, session_id, &retry_body)
            .await?;
    let retry_status = retry.status().as_u16();
    if (200..300).contains(&retry_status) {
        return Ok(retry);
    }
    let retry_text = retry.text().await.unwrap_or_default();
    Err(describe_http_error(
        retry_status,
        &retry_text,
        api_key.is_empty(),
    ))
}

/// Read-only tool names: **always allowed**, no requirement that they were "declared this round".
///
/// Reason: their shared premise is "no data is changed", so they were always promised to be available;
/// and this allowance is **required** — later rounds, to bypass the `reasoning_content` pass-back requirement of thinking mode,
/// declare no tools, so the model switches to expressing read intent in text / DSML markup.
/// If we blocked them across the board, the reply becomes "empty content + no tools" → the frontend reports
/// "provider returned no content" (encountered in practice).
/// ⚠️ **Tools that change data must still be declared this round** — this bottom line does not move.
const READ_ONLY_TOOL_NAMES: [&str; 2] = [TOOL_READ_ITEMS, TOOL_READ_ITEM_DETAIL];

/// Finalization: fallback parsing of content + keep only tools declared this round (read-only tools always allowed).
fn finalize_reply(
    mut content: String,
    mut tool_calls: Vec<AiToolCall>,
    allowed_tools: &[String],
) -> (String, Vec<AiToolCall>) {
    // Some models (lightweight tiers in practice) skip standard function calling and emit calls as text in the content.
    // Only fall back when standard parsing found nothing — otherwise normal explanatory text would be mistaken for calls.
    if tool_calls.is_empty() {
        let (inline_calls, cleaned) = extract_inline_tool_calls(&content);
        if !inline_calls.is_empty() {
            tool_calls = inline_calls;
            content = cleaned;
        }
    }

    // Return only tools **declared this round** (except read-only tools, see the constant above):
    // The model may write a call in the content that was never declared (the fallback above would parse it out).
    // The frontend blocks again before execution; both layers are needed.
    tool_calls.retain(|call| {
        allowed_tools.iter().any(|name| name == &call.name)
            || READ_ONLY_TOOL_NAMES.contains(&call.name.as_str())
    });
    (content, tool_calls)
}

/// The result after consuming a whole stream.
struct StreamOutcome {
    content: String,
    tool_calls: Vec<AiToolCall>,
    /// Chain of thought (only given by thinking-mode models). **Must be passed back verbatim** to the provider,
    /// see `AiChatMessage.reasoning_content`.
    reasoning: Option<String>,
}

/// Consumes the whole stream: feeds each delta to `on_delta`, returns accumulated content, tool calls, and chain of thought.
///
/// The timeout is an **idle timeout**, not a whole-stream timeout: slow long answers are normal; only "not a single byte arriving" should disconnect.
async fn consume_stream<F>(
    response: &mut reqwest::Response,
    protocol: AiProtocol,
    on_delta: &mut F,
) -> Result<StreamOutcome, String>
where
    F: FnMut(&str),
{
    let mut decoder = StreamDecoder::new(protocol);
    let mut full = String::new();

    loop {
        let next = match tokio::time::timeout(STREAM_IDLE_TIMEOUT, response.chunk()).await {
            Ok(result) => result.map_err(|error| format!("读取流式响应失败: {}", error))?,
            Err(_) => {
                return Err(format!(
                    "已经 {} 秒没有收到任何数据，连接已中断。可能是网络不稳定，或该服务商暂时没有响应。",
                    STREAM_IDLE_TIMEOUT_SECS
                ))
            }
        };
        // Stream ended normally
        let Some(chunk) = next else { break };

        // Feed bytes straight to the decoder: **do not decode per network chunk here** —
        // a network chunk boundary may fall in the middle of a multi-byte character, turning one Chinese character into two replacement characters,
        // and this text is both displayed live and written to the session record. Decoding by "complete lines" happens in the decoder.
        for delta in decoder.push_bytes(&chunk) {
            full.push_str(&delta);
            on_delta(&delta);
        }
        if decoder.finished {
            break;
        }
    }

    // When the provider keeps emitting data with no newlines, the decoder gives up buffering; report an explicit error here rather than silently truncating
    if decoder.overflowed {
        return Err("服务端返回的数据格式异常（长时间没有换行），连接已中断。".to_string());
    }
    for delta in decoder.finish() {
        full.push_str(&delta);
        on_delta(&delta);
    }

    // Take the chain of thought separately before checking: an empty string means this model had no thinking content this round
    let reasoning = decoder.take_reasoning();
    Ok(StreamOutcome {
        content: full,
        tool_calls: decoder.take_tool_calls(),
        reasoning: if reasoning.trim().is_empty() {
            None
        } else {
            Some(reasoning)
        },
    })
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Send one chat (non-streaming). Streaming output comes in a later phase; here we first ensure "ask and answer works".
pub async fn chat_once(request: AiChatRequest) -> Result<AiChatReply, String> {
    let (protocol, base_url, api_key, model) = resolve_connection()?;
    let model = request.model.clone().unwrap_or(model);
    if model.trim().is_empty() {
        return Err("还没有选择模型，请在设置里拉取模型列表后选择，或手动填写模型名。".to_string());
    }

    let client = http_client(CHAT_TIMEOUT_SECS, &base_url)?;
    let body = build_request_body(&request, protocol, &model, false);
    // Sending and non-2xx handling both happen inside (including "drop reasoning_effort and retry once if rejected")
    let response = send_with_reasoning_fallback(
        &client,
        protocol,
        &base_url,
        &api_key,
        request.session_id.as_deref(),
        &body,
    )
    .await?;

    let text = response
        .text()
        .await
        .map_err(|error| format!("读取响应内容失败: {}", error))?;

    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|error| format!("响应不是合法 JSON: {}", error))?;
    let parsed_content = match protocol {
        AiProtocol::OpenAi => parse_openai_reply(&value),
        AiProtocol::Anthropic => parse_anthropic_reply(&value),
    };
    // First note "whether content was parsed at all": the check below for "whether nothing was parsed" uses this fact,
    // not the result after fallback cleanup (cleanup only removes content; it should not turn "has content" into "no content").
    let had_content = parsed_content.is_some();
    let (content, tool_calls) = finalize_reply(
        parsed_content.unwrap_or_default(),
        parse_tool_calls(protocol, &value),
        &request.tools,
    );

    // When there are tool calls, content is usually empty — that is an action intent, not a failure;
    // only when both are absent did we truly parse out nothing.
    if !had_content && tool_calls.is_empty() {
        return Err(format!(
            "响应里没有找到回复正文，可能模型不支持该调用方式。原始响应片段: {}",
            text.chars().take(300).collect::<String>()
        ));
    }

    // Chain of thought in thinking mode (only the OpenAI-compatible protocol has it; our Anthropic path does not enable thinking)
    let reasoning_content = match protocol {
        AiProtocol::OpenAi => parse_openai_reasoning(&value),
        AiProtocol::Anthropic => None,
    };

    Ok(AiChatReply {
        content,
        model,
        reasoning_content,
        tool_calls,
    })
}

/// Fetches the available model list. The settings page's "Test Connection" uses this —
/// a successful fetch means address, key, and network are all fine.
pub async fn fetch_models() -> Result<Vec<String>, String> {
    let (protocol, base_url, api_key, _) = resolve_connection()?;
    let client = http_client(MODELS_TIMEOUT_SECS, &base_url)?;

    let path = match protocol {
        AiProtocol::OpenAi => "/models",
        AiProtocol::Anthropic => "/v1/models",
    };
    // Anthropic's version header is unrelated to auth and must always be sent; auth itself is handled by apply_auth.
    let mut request = client.get(join_url(&base_url, path));
    if protocol == AiProtocol::Anthropic {
        request = request.header("anthropic-version", "2023-06-01");
    }
    request = apply_auth(request, protocol, &api_key);

    let response = request.send().await.map_err(describe_request_error)?;
    let status = response.status().as_u16();
    let text = response
        .text()
        .await
        .map_err(|error| format!("读取响应内容失败: {}", error))?;
    if !(200..300).contains(&status) {
        return Err(describe_http_error(status, &text, api_key.is_empty()));
    }

    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|error| format!("响应不是合法 JSON: {}", error))?;
    let mut ids = parse_model_ids(&value);
    ids.sort();
    if ids.is_empty() {
        return Err("连上了，但这个服务商没有返回模型列表，请在设置里手动填写模型名。".to_string());
    }
    Ok(ids)
}

/// Streaming chat: calls `on_delta` on each delta received, then returns the full content.
///
/// Read incrementally with `reqwest::Response::chunk()` — no extra dependency and no feature flag needed
/// (`bytes_stream()` would pull in futures-util). SSE unpacking and protocol differences are handled in stream.rs.
pub async fn chat_stream<F>(request: AiChatRequest, mut on_delta: F) -> Result<AiChatReply, String>
where
    F: FnMut(&str),
{
    let (protocol, base_url, api_key, model) = resolve_connection()?;
    let model = request.model.clone().unwrap_or(model);
    if model.trim().is_empty() {
        return Err("还没有选择模型，请在设置里拉取模型列表后选择，或手动填写模型名。".to_string());
    }

    let client = http_client(STREAM_TIMEOUT_SECS, &base_url)?;
    let body = build_request_body(&request, protocol, &model, true);
    // Sending, retry, and non-2xx handling all happen inside (see send_with_reasoning_fallback)
    let mut response = send_with_reasoning_fallback(
        &client,
        protocol,
        &base_url,
        &api_key,
        request.session_id.as_deref(),
        &body,
    )
    .await?;

    // Stream as received: the user sees characters immediately instead of waiting for the whole response
    let outcome = consume_stream(&mut response, protocol, &mut on_delta).await?;
    let (content, tool_calls) = finalize_reply(outcome.content, outcome.tool_calls, &request.tools);

    // Only empty content **and** no tool calls counts as truly having no content:
    // when the model decides to call a tool, content may naturally be empty; that is not an error.
    if content.trim().is_empty() && tool_calls.is_empty() {
        return Err("服务端没有返回任何内容，请确认所选模型支持流式输出。".to_string());
    }
    Ok(AiChatReply {
        content,
        model,
        reasoning_content: outcome.reasoning,
        tool_calls,
    })
}

// ---------------------------------------------------------------------------
// Unit tests: cover only pure functions (no network requests).
// Request body shape is the most error-prone part here — especially Anthropic's system, which must be hoisted to the top level.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Local addresses must be recognized, otherwise they go through the system proxy and get its 502.
    #[test]
    fn local_endpoints_are_detected() {
        assert!(is_local_endpoint("http://127.0.0.1:11434/v1"));
        assert!(is_local_endpoint("http://localhost:1234/v1"));
        assert!(is_local_endpoint("http://[::1]:8000/v1"));
        assert!(!is_local_endpoint("https://api.deepseek.com/v1"));
        assert!(!is_local_endpoint("https://api.openai.com/v1"));
    }

    /// Local models have no key: must **send no Authorization at all**.
    /// Sending an empty `Bearer ` is worse — some services treat it as bad credentials and return 401.
    #[test]
    fn empty_api_key_sends_no_auth_header() {
        let client = reqwest::Client::new();

        let openai = apply_auth(
            client.get("http://127.0.0.1:11434/v1/models"),
            AiProtocol::OpenAi,
            "",
        )
        .build()
        .expect("构造请求失败");
        assert!(
            openai.headers().get("authorization").is_none(),
            "空密钥不该带 Authorization"
        );

        let anthropic = apply_auth(
            client.get("http://127.0.0.1:8000/v1/models"),
            AiProtocol::Anthropic,
            "",
        )
        .build()
        .expect("构造请求失败");
        assert!(
            anthropic.headers().get("x-api-key").is_none(),
            "空密钥不该带 x-api-key"
        );
    }

    /// A non-empty key must still be injected correctly per protocol; do not break the previously working path.
    #[test]
    fn non_empty_api_key_is_sent_per_protocol() {
        let client = reqwest::Client::new();

        let openai = apply_auth(
            client.get("https://api.deepseek.com/v1/models"),
            AiProtocol::OpenAi,
            "sk-test",
        )
        .build()
        .expect("构造请求失败");
        assert_eq!(
            openai
                .headers()
                .get("authorization")
                .map(|v| v.to_str().unwrap()),
            Some("Bearer sk-test")
        );

        let anthropic = apply_auth(
            client.get("https://api.anthropic.com/v1/models"),
            AiProtocol::Anthropic,
            "sk-ant",
        )
        .build()
        .expect("构造请求失败");
        assert_eq!(
            anthropic
                .headers()
                .get("x-api-key")
                .map(|v| v.to_str().unwrap()),
            Some("sk-ant")
        );
    }

    /// On a 401 with no key filled in, the hint must say "not filled in" rather than "filled in wrong".
    #[test]
    fn missing_key_401_asks_for_a_key() {
        let text = describe_http_error(401, r#"{"error":{"message":"unauthorized"}}"#, true);
        assert!(text.contains("要求填写 API Key"), "{}", text);
        assert!(!text.contains("请检查 API Key 是否正确"), "{}", text);
    }

    /// The session header goes only to OpenCode Console; it must not be fooled by look-alike domains;
    /// never sent without a session id or with an empty string; requests to other providers stay unchanged.
    #[test]
    fn opencode_session_header_only_targets_console_hosts() {
        let session = Some("ses_abc123");
        assert_eq!(
            opencode_session_header("https://opencode.ai/inference/go", session),
            Some(("x-opencode-session", "ses_abc123"))
        );
        assert_eq!(
            opencode_session_header("https://zen.opencode.ai/inference", session),
            Some(("x-opencode-session", "ses_abc123"))
        );
        assert!(opencode_session_header("https://opencode.ai:8443/v1", session).is_some());
        assert_eq!(
            opencode_session_header("https://opencode.ai.evil.com/v1", session),
            None
        );
        assert_eq!(
            opencode_session_header("https://api.deepseek.com", session),
            None
        );
        assert_eq!(
            opencode_session_header("https://opencode.ai/inference/go", None),
            None
        );
        assert_eq!(
            opencode_session_header("https://opencode.ai/inference/go", Some("  ")),
            None
        );
    }

    fn sample_request() -> AiChatRequest {
        AiChatRequest {
            messages: vec![
                AiChatMessage {
                    role: "system".to_string(),
                    content: "你是助手".to_string(),
                    images: Vec::new(),
                    reasoning_content: None,
                },
                AiChatMessage {
                    role: "user".to_string(),
                    content: "你好".to_string(),
                    images: Vec::new(),
                    reasoning_content: None,
                },
            ],
            model: None,
            temperature: Some(0.3),
            // No tools by default: tool declarations are decided by the frontend based on module capabilities
            tools: Vec::new(),
            session_id: None,
        }
    }

    #[test]
    fn joins_url_with_or_without_trailing_slash() {
        assert_eq!(
            join_url("https://api.deepseek.com/v1", "/chat/completions"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            join_url("https://api.deepseek.com/v1/", "/chat/completions"),
            "https://api.deepseek.com/v1/chat/completions"
        );
    }

    #[test]
    fn openai_body_keeps_system_inside_messages() {
        let body = build_openai_body(&sample_request(), "deepseek-chat");
        assert_eq!(body["model"], "deepseek-chat");
        assert_eq!(body["stream"], false);
        let messages = body["messages"].as_array().expect("messages 应为数组");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert!((body["temperature"].as_f64().unwrap() - 0.3).abs() < 1e-6);
    }

    #[test]
    fn images_become_parts_for_both_protocols() {
        // With images, content must change from a string to an array of parts, and the two providers use different shapes:
        // OpenAI uses a data URI, Anthropic uses source.base64 — mixing them gets rejected outright by the provider.
        let request = AiChatRequest {
            messages: vec![AiChatMessage {
                role: "user".to_string(),
                content: "这是什么".to_string(),
                images: vec![AiImage {
                    media_type: "image/jpeg".to_string(),
                    data_base64: "AAAA".to_string(),
                }],
                reasoning_content: None,
            }],
            model: None,
            temperature: None,
            tools: Vec::new(),
            session_id: None,
        };

        let openai = build_openai_body(&request, "m");
        let parts = openai["messages"][0]["content"]
            .as_array()
            .expect("应为分片数组");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/jpeg;base64,AAAA");

        let anthropic = build_anthropic_body(&request, "m");
        let parts = anthropic["messages"][0]["content"]
            .as_array()
            .expect("应为分片数组");
        assert_eq!(parts[1]["type"], "image");
        assert_eq!(parts[1]["source"]["media_type"], "image/jpeg");
        assert_eq!(parts[1]["source"]["data"], "AAAA");
    }

    #[test]
    fn plain_text_messages_still_send_a_plain_string() {
        // Without images, keep the original string form: no change to existing requests and one fewer provider-side branch
        let request = AiChatRequest {
            messages: vec![AiChatMessage {
                role: "user".to_string(),
                content: "你好".to_string(),
                images: Vec::new(),
                reasoning_content: None,
            }],
            model: None,
            temperature: None,
            tools: Vec::new(),
            session_id: None,
        };
        assert_eq!(
            build_openai_body(&request, "m")["messages"][0]["content"],
            "你好"
        );
        assert_eq!(
            build_anthropic_body(&request, "m")["messages"][0]["content"],
            "你好"
        );
    }

    #[test]
    fn provider_error_json_is_reduced_to_its_message() {
        // The provider returns a JSON blob; dumping it on the user is no help.
        // A real 503 was exactly like this: only the message line was useful.
        let body = r#"{"error":{"message":"Service is too busy. We advise users to temporarily switch to alternative LLM API service providers.","type":"service_unavailable_error","code":"service_unavailable_error"}}"#;
        let text = describe_http_error(503, body, false);
        assert!(
            text.contains("Service is too busy"),
            "应保留服务商的原文：{}",
            text
        );
        assert!(
            !text.contains("request_id"),
            "不该把整坨 JSON 带出来：{}",
            text
        );
        // 5xx must make clear "it is a provider-side problem", otherwise users keep changing settings
        assert!(text.contains("服务商暂时不可用"), "{}", text);
    }

    #[test]
    fn error_extraction_falls_back_gracefully() {
        // Not JSON: fall back to raw text, never return an empty string
        assert_eq!(
            extract_error_message("plain gateway error"),
            "plain gateway error"
        );
        // Only a code, no message
        assert_eq!(
            extract_error_message(r#"{"error":{"code":"rate_limited"}}"#),
            "错误代码：rate_limited"
        );
        // Top-level message
        assert_eq!(
            extract_error_message(r#"{"message":"bad request"}"#),
            "bad request"
        );
    }

    #[test]
    fn anthropic_body_hoists_system_and_sets_max_tokens() {
        let body = build_anthropic_body(&sample_request(), "claude-sonnet-4-5");
        // system must be hoisted to the top level, **must not** stay inside messages (otherwise Anthropic errors out)
        assert_eq!(body["system"], "你是助手");
        let messages = body["messages"].as_array().expect("messages 应为数组");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert!(body["max_tokens"].as_u64().unwrap_or(0) > 0);
    }

    #[test]
    fn parses_openai_reply() {
        let value = serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": "你好呀" } }]
        });
        assert_eq!(parse_openai_reply(&value), Some("你好呀".to_string()));
    }

    #[test]
    fn parses_anthropic_reply_and_ignores_non_text_blocks() {
        let value = serde_json::json!({
            "content": [
                { "type": "thinking", "thinking": "内部推理" },
                { "type": "text", "text": "前半" },
                { "type": "text", "text": "后半" }
            ]
        });
        assert_eq!(parse_anthropic_reply(&value), Some("前半后半".to_string()));
    }

    #[test]
    fn parses_model_ids_from_data_array() {
        let value = serde_json::json!({
            "data": [{ "id": "b-model" }, { "id": "a-model" }, { "object": "x" }]
        });
        assert_eq!(parse_model_ids(&value), vec!["b-model", "a-model"]);
    }

    #[test]
    fn only_a_named_reasoning_effort_error_triggers_the_retry() {
        // Only remove and retry when reasoning_effort is explicitly named — retrying other 400s just wastes a request
        // and masks the real error.
        assert!(error_blames_reasoning_effort(
            "{\"error\":{\"message\":\"Unrecognized request argument supplied: reasoning_effort\"}}"
        ));
        assert!(error_blames_reasoning_effort(
            "unknown parameter: REASONING_EFFORT"
        ));
        assert!(!error_blames_reasoning_effort(
            "{\"error\":{\"message\":\"Model Not Exist\"}}"
        ));
        assert!(!error_blames_reasoning_effort("Insufficient Balance"));
    }

    #[test]
    fn reasoning_content_is_attached_only_to_assistant_messages() {
        // Thinking mode: only assistant messages should carry reasoning_content.
        // Putting it on user/system is meaningless, and strict gateways may reject the whole request because of it.
        let request = AiChatRequest {
            messages: vec![
                AiChatMessage {
                    role: "system".to_string(),
                    content: "你是助手".to_string(),
                    images: Vec::new(),
                    reasoning_content: Some("不该被带上".to_string()),
                },
                AiChatMessage {
                    role: "assistant".to_string(),
                    content: "之前的回答".to_string(),
                    images: Vec::new(),
                    reasoning_content: Some("当时的思考".to_string()),
                },
            ],
            model: None,
            temperature: None,
            tools: Vec::new(),
            session_id: None,
        };
        let body = build_openai_body(&request, "test-model");
        let messages = body["messages"].as_array().expect("messages 应是数组");
        assert!(
            messages[0].get("reasoning_content").is_none(),
            "system 不该带"
        );
        assert_eq!(
            messages[1]
                .get("reasoning_content")
                .and_then(|value| value.as_str()),
            Some("当时的思考")
        );
    }

    #[test]
    fn read_only_tool_calls_survive_an_undeclared_round() {
        // Later rounds, to bypass the reasoning_content pass-back requirement of thinking mode, have `tools` empty;
        // so the model expresses read intent in text / DSML markup. If we blocked them across the board,
        // this round becomes "empty content + no tools" → the frontend reports "provider returned no content" (encountered in practice).
        let content =
            "<invoke name=\"read_items\"><parameter name=\"path\">D:\\models</parameter></invoke>";
        let (cleaned, calls) = finalize_reply(content.to_string(), Vec::new(), &[]);
        assert_eq!(calls.len(), 1, "只读工具不该被挡掉");
        assert_eq!(calls[0].name, "read_items");
        assert!(cleaned.trim().is_empty(), "调用块应当从正文里剔除");

        // ⚠️ But **tools that change data** must be blocked in undeclared rounds — this bottom line does not move
        let write_content =
            "<invoke name=\"select_items\"><parameter name=\"ids\">[\"a\"]</parameter></invoke>";
        let (_, write_calls) = finalize_reply(write_content.to_string(), Vec::new(), &[]);
        assert!(write_calls.is_empty(), "未声明的写工具必须被挡掉");

        // When declared, allow as before (do not tighten the opening too much)
        let declared = vec!["select_items".to_string()];
        let (_, declared_calls) = finalize_reply(write_content.to_string(), Vec::new(), &declared);
        assert_eq!(declared_calls.len(), 1);
    }
}
