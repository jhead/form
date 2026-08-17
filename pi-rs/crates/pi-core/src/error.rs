//! Error type shared by the SDK.
//!
//! Kept flat and code-tagged rather than deeply structured: FFI consumers
//! (Swift) match on [`AiError::code`], which is a stable `&'static str`, and
//! surface [`AiError::message`] to users.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum AiError {
    /// Network/transport failure before or during the request.
    #[error("transport error: {message}")]
    Transport { message: String },

    /// Non-2xx response from the provider.
    #[error("provider error ({status}): {message}")]
    Provider {
        status: u16,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<Value>,
        /// Retry delay the server asked for, in ms, when it supplied one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_ms: Option<u64>,
    },

    /// Provider auth is missing, expired, or was rejected.
    #[error("authentication error: {message}")]
    Auth { message: String },

    /// Malformed or unexpected provider payload.
    #[error("protocol error: {message}")]
    Protocol { message: String },

    /// Caller passed something invalid (bad model, bad options, bad schema).
    #[error("invalid request: {message}")]
    InvalidRequest { message: String },

    /// The requested capability does not exist on this API/provider.
    #[error("unsupported: {message}")]
    Unsupported { message: String },

    /// Request was cancelled through an `AbortSignal`.
    #[error("aborted")]
    Aborted,

    /// Deadline exceeded.
    #[error("timed out after {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },

    /// Anything that does not fit the cases above.
    #[error("{message}")]
    Other { message: String },
}

impl AiError {
    pub fn transport(message: impl Into<String>) -> Self {
        AiError::Transport {
            message: message.into(),
        }
    }

    pub fn provider(status: u16, message: impl Into<String>) -> Self {
        AiError::Provider {
            status,
            message: message.into(),
            body: None,
            retry_after_ms: None,
        }
    }

    pub fn auth(message: impl Into<String>) -> Self {
        AiError::Auth {
            message: message.into(),
        }
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        AiError::Protocol {
            message: message.into(),
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        AiError::InvalidRequest {
            message: message.into(),
        }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        AiError::Unsupported {
            message: message.into(),
        }
    }

    pub fn other(message: impl Into<String>) -> Self {
        AiError::Other {
            message: message.into(),
        }
    }

    /// Stable machine-readable code. Do not change these strings: FFI callers
    /// and telemetry attributes depend on them.
    pub fn code(&self) -> &'static str {
        match self {
            AiError::Transport { .. } => "transport",
            AiError::Provider { .. } => "provider",
            AiError::Auth { .. } => "auth",
            AiError::Protocol { .. } => "protocol",
            AiError::InvalidRequest { .. } => "invalid_request",
            AiError::Unsupported { .. } => "unsupported",
            AiError::Aborted => "aborted",
            AiError::Timeout { .. } => "timeout",
            AiError::Other { .. } => "other",
        }
    }

    pub fn message(&self) -> String {
        self.to_string()
    }

    /// Whether a retry could plausibly succeed.
    pub fn is_retryable(&self) -> bool {
        match self {
            AiError::Transport { .. } | AiError::Timeout { .. } => true,
            AiError::Provider { status, .. } => *status == 408 || *status == 429 || *status >= 500,
            _ => false,
        }
    }

    pub fn is_aborted(&self) -> bool {
        matches!(self, AiError::Aborted)
    }
}
