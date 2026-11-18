// ============================================================================
// Typed error contract (M16)
//
// Every `#[tauri::command]` returns `Result<T, AppError>`. The value that crosses
// the IPC boundary is an object, not a string:
//
//     { "code": "permission_denied",
//       "message": "delete failed: access denied",
//       "detail": "icacls /setowner: Access is denied. (os error 5)",
//       "args": { "path": "C:\\Program Files\\Foo" } }
//
// Why the split:
//
// 1. `code` is a closed set and the only field the frontend switches on. Previously
//    the UI branched on error *text* (`failure_reason.includes('拒绝访问')` in
//    LeftoversModule.tsx), so rewording one message silently disabled a branch.
// 2. `message` is an English diagnostic string. It is for logs and for the
//    untranslated-code fallback, never the primary UI text.
// 3. `detail` carries the raw OS / external-tool output (icacls, dism, io::Error).
//    It cannot be translated, so it must not be concatenated into a sentence.
// 4. `args` carries interpolation values only. Rust must not build sentences:
//    word order differs between en / ja / zh, and a sentence baked in Rust can
//    never be translated correctly.
//
// User-facing copy lives in `src/i18n/locales/<lang>/errors.json`, keyed by code.
// Rust-side `Display` output is log-only — see the note on `impl Display`.
// ============================================================================

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Closed set of error categories.
///
/// Granularity is decided by "how many different things can the frontend do",
/// not by "how many places can fail". A new variant is only justified when the
/// UI needs a new action (retry / elevate / refresh list / do nothing), and it
/// must land together with the four `errors.json` files and the exhaustive
/// switch in `src/api/errors.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Needs elevation, or the OS refused access.
    PermissionDenied,
    /// Target does not exist any more.
    NotFound,
    /// Empty path, relative components, or outside the allowed scope.
    InvalidPath,
    /// A drive letter, extension, or other argument is malformed.
    InvalidArgument,
    /// Platform or format not supported by this build.
    Unsupported,
    /// File is locked by another process.
    TargetInUse,
    /// Matched a protection rule (system path, user whitelist).
    Protected,
    /// Exceeds a size or count limit.
    TooLarge,
    /// The user cancelled the operation.
    Cancelled,
    /// dism / icacls / PowerShell / ShellExecute returned an error.
    ExternalFailed,
    /// Anything else; `detail` must be non-empty.
    Internal,
}

impl ErrorCode {
    /// Stable wire identifier. Kept in sync with the `snake_case` serde output
    /// and with the `errors.<code>` keys in the locale files.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PermissionDenied => "permission_denied",
            Self::NotFound => "not_found",
            Self::InvalidPath => "invalid_path",
            Self::InvalidArgument => "invalid_argument",
            Self::Unsupported => "unsupported",
            Self::TargetInUse => "target_in_use",
            Self::Protected => "protected",
            Self::TooLarge => "too_large",
            Self::Cancelled => "cancelled",
            Self::ExternalFailed => "external_failed",
            Self::Internal => "internal",
        }
    }

    /// Default English diagnostic used when a call site has nothing more specific.
    fn default_message(self) -> &'static str {
        match self {
            Self::PermissionDenied => "permission denied",
            Self::NotFound => "target not found",
            Self::InvalidPath => "invalid path",
            Self::InvalidArgument => "invalid argument",
            Self::Unsupported => "unsupported operation",
            Self::TargetInUse => "target is in use",
            Self::Protected => "target is protected",
            Self::TooLarge => "target exceeds the size limit",
            Self::Cancelled => "operation cancelled",
            Self::ExternalFailed => "external tool failed",
            Self::Internal => "internal error",
        }
    }
}

/// Structured error returned by every Tauri command.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppError {
    /// Closed-set category; the only field the frontend switches on.
    pub code: ErrorCode,
    /// English diagnostic. Log-only, plus the fallback when a code has no
    /// translation yet. Never the primary UI text.
    pub message: String,
    /// Raw OS / external-tool output. Cannot be translated, so it is kept in
    /// its own field and shown only in tooltips and logs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Interpolation values for the frontend message. Values only, no sentences.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, String>,
}

impl AppError {
    /// Build an error with the code's default diagnostic.
    pub fn new(code: ErrorCode) -> Self {
        Self {
            code,
            message: code.default_message().to_string(),
            detail: None,
            args: BTreeMap::new(),
        }
    }

    /// Build an error with a specific diagnostic. `message` must stay English:
    /// it is read by developers in logs, not by users.
    pub fn with_message(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
            args: BTreeMap::new(),
        }
    }

    /// Attach the raw OS / tool output.
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        if !detail.is_empty() {
            self.detail = Some(detail);
        }
        self
    }

    /// Attach an interpolation value for the frontend message.
    pub fn arg(mut self, key: &str, value: impl Into<String>) -> Self {
        self.args.insert(key.to_string(), value.into());
        self
    }
}

impl From<String> for AppError {
    /// Migration bridge: an untyped message becomes `internal`. This is what
    /// lets the command layer switch to the object ABI before every inner
    /// helper has been converted — the user-visible text is unchanged.
    fn from(message: String) -> Self {
        Self {
            code: ErrorCode::Internal,
            message,
            detail: None,
            args: BTreeMap::new(),
        }
    }
}

impl From<&str> for AppError {
    fn from(message: &str) -> Self {
        Self::from(message.to_string())
    }
}

impl From<std::io::Error> for AppError {
    fn from(error: std::io::Error) -> Self {
        let code = match error.kind() {
            std::io::ErrorKind::NotFound => ErrorCode::NotFound,
            std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
            _ => ErrorCode::Internal,
        };
        Self::with_message(code, error.to_string())
    }
}

/// Log-only representation. Never send this across the IPC boundary — the
/// frontend reads the structured fields instead.
impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)?;
        if let Some(detail) = &self.detail {
            write!(f, " ({detail})")?;
        }
        Ok(())
    }
}

impl std::error::Error for AppError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_to_the_documented_wire_shape() {
        let error = AppError::new(ErrorCode::InvalidPath)
            .detail(r"path contains ..: C:\Temp\..\Windows")
            .arg("path", r"C:\Temp\..\Windows");
        let json = serde_json::to_value(&error).expect("AppError must serialize");

        assert_eq!(json["code"], "invalid_path");
        assert_eq!(json["args"]["path"], r"C:\Temp\..\Windows");
        assert!(json["detail"].as_str().unwrap().contains(".."));
    }

    #[test]
    fn optional_fields_are_omitted_when_empty() {
        let json = serde_json::to_value(AppError::new(ErrorCode::Cancelled))
            .expect("AppError must serialize");

        assert_eq!(json["code"], "cancelled");
        // The frontend treats a missing field and an empty value the same, so
        // omitting keeps the payload small on the hot error path.
        assert!(json.get("detail").is_none());
        assert!(json.get("args").is_none());
    }

    #[test]
    fn untyped_messages_degrade_to_internal_without_losing_text() {
        let error = AppError::from("目录不存在: C:\\temp".to_string());

        assert_eq!(error.code, ErrorCode::Internal);
        assert_eq!(error.message, "目录不存在: C:\\temp");
    }

    #[test]
    fn io_errors_map_to_actionable_codes() {
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(AppError::from(denied).code, ErrorCode::PermissionDenied);

        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(AppError::from(missing).code, ErrorCode::NotFound);
    }

    #[test]
    fn every_code_has_a_unique_wire_identifier() {
        let codes = [
            ErrorCode::PermissionDenied,
            ErrorCode::NotFound,
            ErrorCode::InvalidPath,
            ErrorCode::InvalidArgument,
            ErrorCode::Unsupported,
            ErrorCode::TargetInUse,
            ErrorCode::Protected,
            ErrorCode::TooLarge,
            ErrorCode::Cancelled,
            ErrorCode::ExternalFailed,
            ErrorCode::Internal,
        ];

        let mut seen = std::collections::HashSet::new();
        for code in codes {
            assert!(
                seen.insert(code.as_str()),
                "duplicate code: {}",
                code.as_str()
            );
        }
        assert_eq!(seen.len(), 11, "the closed set is 11 codes");
    }
}
