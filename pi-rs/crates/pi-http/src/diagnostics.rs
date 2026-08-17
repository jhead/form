//! Assistant-message diagnostics. Port of `packages/ai/src/utils/diagnostics.ts`.
//!
//! Upstream's diagnostic carries a JS `Error` (`name` / `message` / `stack` /
//! `code`). `pi_core::AssistantMessageDiagnostic` is the settled shape here:
//! a stable `code`, a human `message`, a severity and a free-form `detail`
//! payload. These constructors map the error sources this crate produces onto
//! that shape, so adapters attach diagnostics consistently rather than each
//! inventing its own `code` strings.
//!
//! Diagnostics are surfaced to users and written into sessions, so they must
//! not carry credentials. [`from_http_error`] deliberately records the status
//! and body but never headers.

use pi_core::message::{now_ms, AssistantMessageDiagnostic, DiagnosticSeverity};
use serde_json::{json, Value};

use crate::HttpError;

/// Diagnostic codes this crate emits. Adapters should reuse these rather than
/// inventing near-duplicates, because telemetry aggregates on them.
pub mod codes {
    /// Non-2xx response from the provider.
    pub const PROVIDER_HTTP_ERROR: &str = "provider_http_error";
    /// Network/transport failure before or during the request.
    pub const TRANSPORT_ERROR: &str = "transport_error";
    /// The request deadline elapsed.
    pub const TIMEOUT: &str = "timeout";
    /// A retry was scheduled after a retryable failure.
    pub const STREAM_RETRY: &str = "stream_retry";
    /// The provider payload did not match the expected schema.
    pub const MALFORMED_PAYLOAD: &str = "malformed_payload";
    /// A tool call's arguments failed schema validation.
    pub const TOOL_ARGUMENT_VALIDATION: &str = "tool_argument_validation";
}

/// Build a diagnostic with the current timestamp.
pub fn diagnostic(
    code: impl Into<String>,
    message: impl Into<String>,
    severity: DiagnosticSeverity,
    detail: Option<Value>,
) -> AssistantMessageDiagnostic {
    AssistantMessageDiagnostic {
        code: code.into(),
        message: message.into(),
        severity: Some(severity),
        detail,
        timestamp: Some(now_ms()),
    }
}

/// Diagnostic for any `std::error::Error`, following the `source()` chain into
/// `detail.causes` — the closest analogue to upstream's stack capture.
pub fn from_error(
    code: impl Into<String>,
    error: &dyn std::error::Error,
) -> AssistantMessageDiagnostic {
    let mut causes = Vec::new();
    let mut source = error.source();
    while let Some(current) = source {
        causes.push(Value::String(current.to_string()));
        source = current.source();
    }
    let detail = if causes.is_empty() {
        None
    } else {
        Some(json!({ "causes": causes }))
    };
    diagnostic(code, error.to_string(), DiagnosticSeverity::Error, detail)
}

/// Diagnostic for a transport-layer failure, with the code chosen from the
/// variant and the status/body preserved for triage.
pub fn from_http_error(error: &HttpError) -> AssistantMessageDiagnostic {
    match error {
        HttpError::Status {
            status,
            message,
            body,
            retry_after_ms,
        } => {
            let mut detail = serde_json::Map::new();
            detail.insert("status".into(), json!(status));
            if let Some(body) = body {
                detail.insert("body".into(), body.clone());
            }
            if let Some(retry_after_ms) = retry_after_ms {
                detail.insert("retryAfterMs".into(), json!(retry_after_ms));
            }
            diagnostic(
                codes::PROVIDER_HTTP_ERROR,
                format!("{status}: {message}"),
                DiagnosticSeverity::Error,
                Some(Value::Object(detail)),
            )
        }
        HttpError::Timeout(timeout_ms) => diagnostic(
            codes::TIMEOUT,
            error.to_string(),
            DiagnosticSeverity::Error,
            Some(json!({ "timeoutMs": timeout_ms })),
        ),
        HttpError::Transport(_) | HttpError::Aborted | HttpError::InvalidRequest(_) => diagnostic(
            codes::TRANSPORT_ERROR,
            error.to_string(),
            DiagnosticSeverity::Error,
            None,
        ),
    }
}

/// Diagnostic recording that a retry was scheduled. Warning severity: the turn
/// may still succeed, so this must not read as a failure.
pub fn retry_scheduled(
    attempt: u32,
    max_attempts: u32,
    delay_ms: u64,
    reason: &str,
) -> AssistantMessageDiagnostic {
    diagnostic(
        codes::STREAM_RETRY,
        format!("retrying after {delay_ms}ms (attempt {attempt}/{max_attempts}): {reason}"),
        DiagnosticSeverity::Warning,
        Some(json!({ "attempt": attempt, "maxAttempts": max_attempts, "delayMs": delay_ms })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::message::AssistantMessage;

    #[test]
    fn http_status_errors_keep_status_and_body() {
        let error = HttpError::Status {
            status: 429,
            message: "slow down".into(),
            body: Some(json!({ "error": { "message": "slow down" } })),
            retry_after_ms: Some(2_000),
        };
        let d = from_http_error(&error);
        assert_eq!(d.code, codes::PROVIDER_HTTP_ERROR);
        assert_eq!(d.message, "429: slow down");
        assert_eq!(d.severity, Some(DiagnosticSeverity::Error));
        let detail = d.detail.unwrap();
        assert_eq!(detail["status"], 429);
        assert_eq!(detail["retryAfterMs"], 2_000);
        assert_eq!(detail["body"]["error"]["message"], "slow down");
    }

    #[test]
    fn timeouts_and_transport_failures_get_their_own_codes() {
        assert_eq!(
            from_http_error(&HttpError::Timeout(500)).code,
            codes::TIMEOUT
        );
        assert_eq!(
            from_http_error(&HttpError::Transport("reset".into())).code,
            codes::TRANSPORT_ERROR
        );
        assert_eq!(
            from_http_error(&HttpError::Aborted).code,
            codes::TRANSPORT_ERROR
        );
    }

    #[test]
    fn error_diagnostics_follow_the_source_chain() {
        #[derive(Debug, thiserror::Error)]
        #[error("outer")]
        struct Outer(#[source] Inner);
        #[derive(Debug, thiserror::Error)]
        #[error("inner")]
        struct Inner;

        let d = from_error("x", &Outer(Inner));
        assert_eq!(d.message, "outer");
        assert_eq!(d.detail.unwrap()["causes"][0], "inner");
    }

    #[test]
    fn a_sourceless_error_has_no_detail() {
        let d = from_error(codes::MALFORMED_PAYLOAD, &HttpError::Aborted);
        assert!(d.detail.is_none());
        assert!(d.timestamp.is_some());
    }

    #[test]
    fn retry_diagnostics_are_warnings_and_attach_to_a_message() {
        let d = retry_scheduled(2, 3, 1_000, "overloaded");
        assert_eq!(d.severity, Some(DiagnosticSeverity::Warning));
        assert_eq!(d.detail.as_ref().unwrap()["attempt"], 2);

        let mut message = AssistantMessage::pending("a", "p", "m");
        message.push_diagnostic(d);
        message.push_diagnostic(from_http_error(&HttpError::Timeout(1)));
        assert_eq!(message.diagnostics.unwrap().len(), 2);
    }
}
