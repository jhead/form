//! HTTP transport and shared utilities for every provider adapter.
//!
//! Port target: all of `packages/ai/src/utils/`, plus the `fetch` wrappers the
//! adapters build on top of it. Module names match the upstream file names so
//! the two trees can be diffed (`utils/json-parse.ts` → [`json_parse`]).
//!
//! ## What lives here
//!
//! **Transport** — [`client`] (the `reqwest` wrapper), [`sse`] (event-stream
//! parsing), [`headers`], [`error_body`] (provider error envelopes), [`retry`],
//! [`node_http_proxy`] and [`provider_env`].
//!
//! **Adapter utilities** — [`json_parse`] (partial JSON for tool-call argument
//! deltas), [`validation`] (JSON Schema validation of tool arguments),
//! [`estimate`] (token estimation), [`overflow`] (context-overflow detection),
//! [`abort_signals`], [`sanitize_unicode`], [`text`], [`hash`], [`uuid`] and
//! [`diagnostics`].
//!
//! ## Not ported
//!
//! - `utils/event-stream.ts` — its `AssistantMessageEventStream` is already
//!   [`pi_core::event`]. The generic `EventStream<T, R>` underneath it is a
//!   channel in Rust, and a generic public type would not bridge to Swift.
//! - `utils/typebox-helpers.ts` — TypeBox-specific; schemas are plain
//!   `serde_json::Value` here.
//! - `utils/deferred-tools.ts` — belongs with the agent runtime, not transport.

pub mod abort_signals;
pub mod client;
pub mod diagnostics;
pub mod error_body;
pub mod estimate;
pub mod hash;
pub mod headers;
pub mod json_parse;
pub mod node_http_proxy;
pub mod overflow;
pub mod provider_env;
pub mod retry;
pub mod sanitize_unicode;
pub mod sse;
pub mod text;
pub mod uuid;
pub mod validation;

pub use abort_signals::{
    combine_abort_signals, operation_signal, race_with_abort_signal, sleep_unless_aborted,
    CombinedAbortSignal,
};
pub use client::{
    HttpClient, HttpClientConfig, HttpResponse, JsonRequest, Method, OwnedHeaders, SseResponse,
};
pub use error_body::{
    extract_error, format_provider_error, normalize_provider_error, retry_after_ms,
    NormalizedProviderError, MAX_PROVIDER_ERROR_BODY_CHARS,
};
pub use estimate::{
    estimate_context_tokens, estimate_message_tokens, estimate_text_tokens, ContextUsageEstimate,
};
pub use hash::short_hash;
pub use headers::{force_pi_user_agent, merge_headers, pi_user_agent, redact_headers, HeaderMap};
pub use json_parse::{
    parse_json_with_repair, parse_partial_json, parse_streaming_json, parse_streaming_json_object,
    repair_json,
};
pub use node_http_proxy::{
    resolve_http_proxy_url_for_target, ProxyError, UNSUPPORTED_PROXY_PROTOCOL_MESSAGE,
};
pub use overflow::{is_context_overflow, is_context_overflow_message, is_recoverable_length};
pub use provider_env::get_provider_env_value;
pub use retry::{
    is_retryable_assistant_error, is_retryable_error_message, is_retryable_provider_error,
    retry_assistant_call, retry_attempts, retry_with_backoff, AssistantRetryPolicy, AttemptFailure,
    RetryObserver, RetryPolicy,
};
pub use sanitize_unicode::{
    has_unpaired_surrogates, sanitize_surrogate_escapes, sanitize_surrogates_utf16,
};
pub use sse::{sse_stream, SseEvent, SseParser};
pub use text::{assistant_content_text, input_content_text, message_text};
pub use validation::{validate_tool_arguments, validate_tool_call, ToolValidationError};

/// Transport-level failure. Adapters convert this into `pi_core::AiError`.
///
/// Note this deliberately carries no response headers: adapters match on it
/// exhaustively, so it stays narrow. Where retry classification needs headers
/// (`x-should-retry`), they travel alongside in [`retry::AttemptFailure`].
#[derive(Debug, Clone, thiserror::Error)]
pub enum HttpError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("request timed out after {0}ms")]
    Timeout(u64),
    #[error("aborted")]
    Aborted,
    #[error("http {status}: {message}")]
    Status {
        status: u16,
        message: String,
        body: Option<serde_json::Value>,
        retry_after_ms: Option<u64>,
    },
    #[error("invalid request: {0}")]
    InvalidRequest(String),
}

impl From<HttpError> for pi_core::AiError {
    fn from(err: HttpError) -> Self {
        match err {
            HttpError::Transport(message) => pi_core::AiError::Transport { message },
            HttpError::Timeout(timeout_ms) => pi_core::AiError::Timeout { timeout_ms },
            HttpError::Aborted => pi_core::AiError::Aborted,
            HttpError::Status {
                status,
                message,
                body,
                retry_after_ms,
            } => {
                if status == 401 || status == 403 {
                    pi_core::AiError::Auth { message }
                } else {
                    pi_core::AiError::Provider {
                        status,
                        message,
                        body,
                        retry_after_ms,
                    }
                }
            }
            HttpError::InvalidRequest(message) => pi_core::AiError::InvalidRequest { message },
        }
    }
}

impl HttpError {
    /// Whether a retry could plausibly succeed, from the error alone.
    ///
    /// Prefer [`retry::is_retryable_provider_error`] when the response headers
    /// are available: the OpenAI and Anthropic SDKs send `x-should-retry`,
    /// which overrides the status.
    pub fn is_retryable(&self) -> bool {
        match self {
            HttpError::Transport(_) | HttpError::Timeout(_) => true,
            // 409 included to match upstream's `isRetryableProviderError`.
            HttpError::Status { status, .. } => {
                *status == 408 || *status == 409 || *status == 429 || *status >= 500
            }
            _ => false,
        }
    }

    /// Stable machine-readable code, mirroring [`pi_core::AiError::code`].
    pub fn code(&self) -> &'static str {
        match self {
            HttpError::Transport(_) => "transport",
            HttpError::Timeout(_) => "timeout",
            HttpError::Aborted => "aborted",
            HttpError::Status { .. } => "provider",
            HttpError::InvalidRequest(_) => "invalid_request",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_errors_map_onto_the_core_error_type() {
        let auth: pi_core::AiError = HttpError::Status {
            status: 401,
            message: "bad key".into(),
            body: None,
            retry_after_ms: None,
        }
        .into();
        assert_eq!(auth.code(), "auth");

        let provider: pi_core::AiError = HttpError::Status {
            status: 429,
            message: "slow down".into(),
            body: None,
            retry_after_ms: Some(1_000),
        }
        .into();
        assert_eq!(provider.code(), "provider");
        assert!(provider.is_retryable());
    }

    #[test]
    fn retryability_matches_the_upstream_status_set() {
        let status = |status| HttpError::Status {
            status,
            message: String::new(),
            body: None,
            retry_after_ms: None,
        };
        for code in [408, 409, 429, 500, 503] {
            assert!(status(code).is_retryable(), "{code}");
        }
        for code in [400, 401, 403, 404, 422] {
            assert!(!status(code).is_retryable(), "{code}");
        }
        assert!(HttpError::Transport("reset".into()).is_retryable());
        assert!(HttpError::Timeout(1).is_retryable());
        assert!(!HttpError::Aborted.is_retryable());
        assert!(!HttpError::InvalidRequest("bad".into()).is_retryable());
    }

    #[test]
    fn error_codes_are_stable() {
        assert_eq!(HttpError::Aborted.code(), "aborted");
        assert_eq!(HttpError::Timeout(1).code(), "timeout");
        assert_eq!(HttpError::Transport(String::new()).code(), "transport");
        assert_eq!(
            HttpError::InvalidRequest(String::new()).code(),
            "invalid_request"
        );
    }
}
