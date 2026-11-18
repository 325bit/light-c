// ============================================================================
// AI provider presets
//
// Current situation: mainstream providers at home and abroad (DeepSeek / Tongyi / Kimi / Zhipu / Doubao)
// and local inference (Ollama / LM Studio / vLLM) **have all converged on the OpenAI-compatible protocol**,
// so only two protocol implementations are needed here: OpenAi and Anthropic.
//
// Adding a provider usually just means adding a row in the table below; no calling code needs to change.
// ============================================================================

use serde::Serialize;

/// Protocol family used by a provider, determining how request and response bodies are parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AiProtocol {
    /// OpenAI-compatible: POST {base}/chat/completions, `Authorization: Bearer`
    OpenAi,
    /// Anthropic：POST {base}/v1/messages，`x-api-key` + `anthropic-version`
    Anthropic,
}

/// A provider's integration info.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProviderPreset {
    /// Stable identifier, this is what is stored in config
    pub id: &'static str,
    /// Display name in the settings page
    pub name: &'static str,
    pub protocol: AiProtocol,
    /// Default endpoint (**without trailing slash**, added uniformly when joining paths)
    pub base_url: &'static str,
    /// Default model: **deliberately all left empty**.
    ///
    /// Hard-coded model names carry real risk — providers renaming or retiring models is common, and the fetched list may no longer contain that name,
    /// so users saving it leaves an invalid model name and calls fail immediately. Leaving empty forces selection from fetched results.
    /// The UX does not get worse: the settings page auto-selects the first item after "Test connection" succeeds;
    /// and if the list cannot be fetched, the UI degrades to a manual input box.
    pub default_model: &'static str,
    /// Whether it is local inference; the UI uses this to hint "data never leaves this machine"
    pub local: bool,
}

/// Built-in preset table. Array order = display order in the settings page.
pub const PROVIDER_PRESETS: &[AiProviderPreset] = &[
    AiProviderPreset {
        id: "deepseek",
        name: "DeepSeek",
        protocol: AiProtocol::OpenAi,
        base_url: "https://api.deepseek.com/v1",
        default_model: "",
        local: false,
    },
    AiProviderPreset {
        id: "openai",
        name: "OpenAI",
        protocol: AiProtocol::OpenAi,
        base_url: "https://api.openai.com/v1",
        default_model: "",
        local: false,
    },
    AiProviderPreset {
        id: "moonshot",
        name: "Kimi（月之暗面）",
        protocol: AiProtocol::OpenAi,
        base_url: "https://api.moonshot.cn/v1",
        default_model: "",
        local: false,
    },
    AiProviderPreset {
        id: "dashscope",
        name: "通义千问（Qwen）",
        protocol: AiProtocol::OpenAi,
        // Bailian's dedicated OpenAI-compatible path, different from the native DashScope endpoint
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        default_model: "",
        local: false,
    },
    AiProviderPreset {
        id: "zhipu",
        name: "智谱 GLM",
        protocol: AiProtocol::OpenAi,
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        default_model: "",
        local: false,
    },
    AiProviderPreset {
        id: "anthropic",
        name: "Anthropic Claude",
        protocol: AiProtocol::Anthropic,
        base_url: "https://api.anthropic.com",
        // Claude model names iterate quickly; only give a hint here, the fetched result is authoritative
        default_model: "",
        local: false,
    },
    AiProviderPreset {
        id: "ollama",
        name: "本地 Ollama",
        protocol: AiProtocol::OpenAi,
        // Ollama ships an OpenAI-compatible endpoint
        base_url: "http://127.0.0.1:11434/v1",
        default_model: "",
        local: true,
    },
    AiProviderPreset {
        id: "custom",
        name: "自定义（OpenAI 兼容）",
        protocol: AiProtocol::OpenAi,
        base_url: "",
        default_model: "",
        local: false,
    },
];

/// Render dropdowns for the settings page.
pub fn list_providers() -> Vec<AiProviderPreset> {
    PROVIDER_PRESETS.to_vec()
}

/// Look up a preset by id; fall back to "custom" when not found, so old config cannot break the UI.
pub fn find_preset(id: &str) -> &'static AiProviderPreset {
    PROVIDER_PRESETS
        .iter()
        .find(|preset| preset.id == id)
        .unwrap_or_else(|| {
            PROVIDER_PRESETS
                .iter()
                .find(|preset| preset.id == "custom")
                .expect("预设表必须包含 custom 兜底项")
        })
}
