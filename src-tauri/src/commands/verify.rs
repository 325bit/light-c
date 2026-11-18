// ============================================================================
// File integrity verification commands
// ============================================================================

use crate::runtime::{detect_distribution_channel, DistributionChannel};
use base64::Engine;
use minisign_verify::{PublicKey, Signature};
use serde::Serialize;
use std::path::PathBuf;
const INSTALLER_EXE_SIGNATURE_ASSET: &str = "LightC_installer_exe.sig";
const WEBVIEW2_OFFLINE_EXE_SIGNATURE_ASSET: &str = "LightC_webview2_offline_exe.sig";
const PORTABLE_EXE_SIGNATURE_ASSET: &str = "LightC_portable_exe.sig";
const OFFICIAL_RELEASE_URL: &str = "https://github.com/Chunyu33/light-c/releases";

/// M13 fix: the updater public key used to be hard-coded here *and* in
/// `tauri.conf.json` (`plugins.updater.pubkey`). Two spellings of a security
/// constant drift silently — the updater would install with one key while
/// `verify_integrity` checked with another if only one side was rotated.
/// The key is now read from `tauri.conf.json` at compile time, so the updater
/// and the integrity check share one source of truth by construction.
fn updater_public_key() -> &'static str {
    static KEY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    KEY.get_or_init(|| {
        serde_json::from_str::<serde_json::Value>(include_str!("../../tauri.conf.json"))
            .ok()
            .and_then(|config| {
                config["plugins"]["updater"]["pubkey"]
                    .as_str()
                    .map(str::to_owned)
            })
            .expect("tauri.conf.json must contain plugins.updater.pubkey")
    })
    .as_str()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyIntegrityStatus {
    Verified,
    Failed,
    NetworkError,
    ReleaseUnavailable,
    SignatureError,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifyIntegrityResult {
    pub verified: bool,
    pub status: VerifyIntegrityStatus,
    pub version: String,
    pub channel: String,
    pub message: String,
    pub official_url: String,
}

struct SignatureSource {
    asset_name: &'static str,
    signature: String,
}

/// Verify whether the currently running LightC.exe is officially signed.
#[tauri::command]
pub async fn verify_integrity() -> VerifyIntegrityResult {
    match verify_integrity_inner().await {
        Ok(result) => result,
        Err(VerifyError::Network(message)) => build_result(
            false,
            VerifyIntegrityStatus::NetworkError,
            current_version(),
            current_channel_label(),
            format!("无法连接到 GitHub：{}", message),
        ),
        Err(VerifyError::ReleaseUnavailable(message)) => build_result(
            false,
            VerifyIntegrityStatus::ReleaseUnavailable,
            current_version(),
            current_channel_label(),
            message,
        ),
        Err(VerifyError::InvalidSignature(message)) => build_result(
            false,
            VerifyIntegrityStatus::Failed,
            current_version(),
            current_channel_label(),
            message,
        ),
        Err(VerifyError::SignatureFormat(message)) => build_result(
            false,
            VerifyIntegrityStatus::SignatureError,
            current_version(),
            current_channel_label(),
            message,
        ),
        Err(VerifyError::Local(message)) => build_result(
            false,
            VerifyIntegrityStatus::Failed,
            current_version(),
            current_channel_label(),
            message,
        ),
    }
}

async fn verify_integrity_inner() -> Result<VerifyIntegrityResult, VerifyError> {
    let exe_path = std::env::current_exe()
        .map_err(|error| VerifyError::Local(format!("无法读取当前程序路径：{}", error)))?;
    let channel = detect_distribution_channel(&exe_path);
    let exe_bytes = std::fs::read(&exe_path)
        .map_err(|error| VerifyError::Local(format!("无法读取当前程序文件：{}", error)))?;

    let app_version = current_version();
    let signature_sources = fetch_signatures(&channel, &app_version).await?;
    verify_exe_signatures(&exe_bytes, &signature_sources)?;

    Ok(build_result(
        true,
        VerifyIntegrityStatus::Verified,
        app_version.clone(),
        channel.label().to_string(),
        format!("当前为官方原版 v{}", app_version),
    ))
}

async fn fetch_signatures(
    channel: &DistributionChannel,
    app_version: &str,
) -> Result<Vec<SignatureSource>, VerifyError> {
    let asset_names: &[&'static str] = match channel {
        // The installed build may come from a regular installer or from the built-in WebView2 offline installer; the two are produced by different packaging steps, so the exe bytes are not necessarily identical.
        DistributionChannel::Installer => &[
            INSTALLER_EXE_SIGNATURE_ASSET,
            WEBVIEW2_OFFLINE_EXE_SIGNATURE_ASSET,
        ],
        DistributionChannel::Portable => &[PORTABLE_EXE_SIGNATURE_ASSET],
    };

    let mut signatures = Vec::new();
    let mut last_error: Option<VerifyError> = None;
    for asset_name in asset_names {
        match fetch_signature_asset(app_version, asset_name).await {
            Ok(signature) => signatures.push(signature),
            Err(VerifyError::ReleaseUnavailable(error)) => {
                // New and old Release assets may not be fully consistent; one missing candidate should not block verification of other official signatures.
                last_error = Some(VerifyError::ReleaseUnavailable(error));
            }
            Err(error) => return Err(error),
        }
    }

    if signatures.is_empty() {
        return Err(last_error.unwrap_or_else(|| {
            VerifyError::ReleaseUnavailable(format!(
                "当前版本 v{} 尚未发布可用的官方签名资产。",
                app_version
            ))
        }));
    }

    Ok(signatures)
}

async fn fetch_signature_asset(
    app_version: &str,
    asset_name: &'static str,
) -> Result<SignatureSource, VerifyError> {
    let signature_url = release_asset_url(app_version, asset_name);
    let response = reqwest::Client::new()
        .get(&signature_url)
        .header("User-Agent", "LightC-integrity-check")
        .send()
        .await
        .map_err(|error| VerifyError::Network(error.to_string()))?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(VerifyError::ReleaseUnavailable(format!(
            "当前版本 v{} 尚未发布官方签名资产 {}，请在 GitHub Release 完成后再校验。",
            app_version, asset_name
        )));
    }

    if !response.status().is_success() {
        return Err(VerifyError::Network(format!(
            "下载签名资产失败：HTTP {}",
            response.status()
        )));
    }

    let signature = response
        .text()
        .await
        .map_err(|error| VerifyError::Network(error.to_string()))?;

    Ok(SignatureSource {
        asset_name,
        signature,
    })
}

fn release_asset_url(app_version: &str, asset_name: &str) -> String {
    // Request the current version Release assets directly, avoiding GitHub API rate limits and also preventing older versions from mistakenly picking up the latest signature.
    format!(
        "https://github.com/Chunyu33/light-c/releases/download/v{}/{}",
        app_version, asset_name
    )
}

fn verify_exe_signatures(
    exe_bytes: &[u8],
    signature_sources: &[SignatureSource],
) -> Result<(), VerifyError> {
    let mut invalid_errors = Vec::new();
    let mut format_errors = Vec::new();

    for source in signature_sources {
        match verify_exe_signature(exe_bytes, &source.signature) {
            Ok(()) => return Ok(()),
            Err(VerifyError::InvalidSignature(message)) => {
                invalid_errors.push(format!("{}: {}", source.asset_name, message));
            }
            Err(VerifyError::SignatureFormat(message)) => {
                format_errors.push(format!("{}: {}", source.asset_name, message));
            }
            Err(error) => return Err(error),
        }
    }

    if !invalid_errors.is_empty() {
        return Err(VerifyError::InvalidSignature(format!(
            "当前 LightC.exe 未匹配到该版本的官方 exe 签名资产。可能是文件来源不一致、文件被修改，或 Release 签名资产需要修复：{}",
            invalid_errors.join("；")
        )));
    }

    Err(VerifyError::SignatureFormat(format!(
        "所有候选签名文件格式均异常：{}",
        format_errors.join("；")
    )))
}

fn verify_exe_signature(exe_bytes: &[u8], signature_text: &str) -> Result<(), VerifyError> {
    let public_key_text = decode_base64_text(updater_public_key())
        .map_err(|message| VerifyError::Local(format!("官方公钥解析失败：{}", message)))?;
    let signature_text = normalize_signature_text(signature_text)?;
    let public_key = PublicKey::decode(&public_key_text)
        .map_err(|error| VerifyError::Local(format!("官方公钥解析失败：{}", error)))?;
    let signature = Signature::decode(&signature_text)
        .map_err(|error| VerifyError::SignatureFormat(format!("签名文件格式异常：{}", error)))?;

    public_key
        .verify(exe_bytes, &signature, true)
        .map_err(|error| {
            VerifyError::InvalidSignature(format!("签名与当前 LightC.exe 不一致：{}", error))
        })
}

fn normalize_signature_text(signature_text: &str) -> Result<String, VerifyError> {
    let mut current = signature_text.trim().to_string();

    for _ in 0..3 {
        if current.starts_with("untrusted comment:") {
            return Ok(current);
        }

        // Support legacy Releases where signature assets were wrapped in an extra layer of base64; only keep unwrapping inward when it decodes as UTF-8.
        current = decode_base64_text(&current).map_err(|message| {
            VerifyError::SignatureFormat(format!("签名文件解析失败：{}", message))
        })?;
    }

    if current.starts_with("untrusted comment:") {
        Ok(current)
    } else {
        Err(VerifyError::SignatureFormat(
            "签名文件不是 minisign 文本格式".to_string(),
        ))
    }
}

fn decode_base64_text(base64_text: &str) -> Result<String, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_text.trim())
        .map_err(|error| error.to_string())?;
    String::from_utf8(bytes).map_err(|error| error.to_string())
}

fn current_channel_label() -> String {
    std::env::current_exe()
        .ok()
        .map(|path: PathBuf| detect_distribution_channel(&path).label().to_string())
        .unwrap_or_else(|| "未知版本".to_string())
}

fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn build_result(
    verified: bool,
    status: VerifyIntegrityStatus,
    version: String,
    channel: String,
    message: String,
) -> VerifyIntegrityResult {
    VerifyIntegrityResult {
        verified,
        status,
        version,
        channel,
        message,
        official_url: OFFICIAL_RELEASE_URL.to_string(),
    }
}

#[derive(Debug)]
enum VerifyError {
    Network(String),
    ReleaseUnavailable(String),
    SignatureFormat(String),
    InvalidSignature(String),
    Local(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_MINISIGN_TEXT: &str = "untrusted comment: signature from tauri secret key\nRUQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\ntrusted comment: timestamp:1782718516\tfile:LightC.exe\nAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==\n";

    #[test]
    fn normalize_signature_text_accepts_raw_minisign_text() {
        let normalized = normalize_signature_text(SAMPLE_MINISIGN_TEXT).unwrap();
        assert!(normalized.starts_with("untrusted comment:"));
    }

    #[test]
    fn normalize_signature_text_accepts_single_base64_signature() {
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(SAMPLE_MINISIGN_TEXT.as_bytes());
        let normalized = normalize_signature_text(&encoded).unwrap();
        assert!(normalized.starts_with("untrusted comment:"));
    }

    #[test]
    fn normalize_signature_text_accepts_legacy_double_base64_signature() {
        let encoded_once =
            base64::engine::general_purpose::STANDARD.encode(SAMPLE_MINISIGN_TEXT.as_bytes());
        let encoded_twice =
            base64::engine::general_purpose::STANDARD.encode(encoded_once.as_bytes());
        let normalized = normalize_signature_text(&encoded_twice).unwrap();
        assert!(normalized.starts_with("untrusted comment:"));
    }
}
