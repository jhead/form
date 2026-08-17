//! Provider error body extraction and normalization.
//! Port of `packages/ai/src/utils/error-body.ts`.
//!
//! Upstream's job is to dig a usable message out of whichever SDK error object
//! it was handed (Mistral's `statusCode`/`body`, the `openai` SDK's `.error`,
//! Bedrock's `$metadata`/`$response`, …). This port talks HTTP directly, so
//! there is no SDK error shape to probe — but the *same* problem reappears one
//! layer down, in the JSON envelopes themselves. Every provider nests the human
//! message somewhere different, and getting it wrong is what produced upstream's
//! `"403 status code (no body)"` bug reports.
//!
//! [`extract_error`] therefore knows the real envelopes. The shapes below are
//! taken from the upstream adapters and their regression fixtures; add to them
//! rather than special-casing at the call site.
//!
//! [`normalize_provider_error`] / [`format_provider_error`] are the composition
//! half of upstream's module, kept so adapters build their `errorMessage`
//! strings the same way and never double-print a body.

use crate::client::OwnedHeaders;
use crate::HttpError;

/// Bodies longer than this are truncated before being shown or stored.
pub const MAX_PROVIDER_ERROR_BODY_CHARS: usize = 4000;

/// Cap on the fallback message taken from a non-JSON body.
const MAX_PLAIN_MESSAGE_CHARS: usize = 500;

/// Turn a non-2xx response into an [`HttpError::Status`], pulling the most
/// human-readable message out of the provider's error envelope and honouring
/// `retry-after`.
pub fn extract_error(status: u16, headers: &OwnedHeaders, body: &str) -> HttpError {
    let parsed: Option<serde_json::Value> = serde_json::from_str(body).ok();
    let message = parsed
        .as_ref()
        .and_then(extract_message)
        .unwrap_or_else(|| plain_message(body, status));
    HttpError::Status {
        status,
        message,
        body: parsed,
        retry_after_ms: retry_after_ms(headers),
    }
}

/// Dig the human-readable message out of a parsed provider error envelope.
///
/// Handles, in order of specificity:
///
/// | shape | providers |
/// |---|---|
/// | `[{error: {...}}]` | Google Vertex (array-wrapped) |
/// | `{error: {message}}` | OpenAI, Anthropic, Google, Azure, OpenRouter, Copilot |
/// | `{error: {message, metadata: {raw}}}` | OpenRouter (upstream detail in `raw`) |
/// | `{error: "..."}` | Ollama, llama.cpp, assorted gateways |
/// | `{message}` | Mistral, Bedrock, Cohere |
/// | `{detail}` / `{detail: [{msg}]}` | FastAPI-based servers (vLLM, TGI) |
/// | `{errors: [...]}` | Cloudflare, GraphQL-style gateways |
pub fn extract_message(value: &serde_json::Value) -> Option<String> {
    if let Some(array) = value.as_array() {
        return array.iter().find_map(extract_message);
    }
    let obj = value.as_object()?;

    if let Some(error) = obj.get("error") {
        if let Some(text) = error.as_str() {
            return non_empty(text.to_string());
        }
        if let Some(message) = nested_message(error) {
            // OpenRouter routes to a backend and puts that backend's verbatim
            // error in `metadata.raw`, which is usually the only useful part.
            if let Some(raw) = error
                .get("metadata")
                .and_then(|m| m.get("raw"))
                .and_then(scalar_text)
                .filter(|raw| !message.contains(raw.as_str()))
            {
                return non_empty(format!("{message}\n{raw}"));
            }
            return non_empty(message);
        }
        // `{"error": {...}}` with no recognisable message: show the envelope
        // rather than falling through to a status-only message.
        if error.is_object() {
            return non_empty(serde_json::to_string(error).unwrap_or_default());
        }
    }

    if let Some(message) = nested_message(value) {
        return non_empty(message);
    }

    if let Some(first) = obj
        .get("errors")
        .and_then(|e| e.as_array())
        .and_then(|a| a.first())
    {
        return extract_message(first).or_else(|| first.as_str().map(str::to_string));
    }

    None
}

/// `message` / `detail` / `error_message` / `msg` on an object, including
/// FastAPI's `detail: [{msg, loc}]` list form.
fn nested_message(value: &serde_json::Value) -> Option<String> {
    let obj = value.as_object()?;
    for key in ["message", "detail", "error_message", "error_msg", "msg"] {
        match obj.get(key) {
            Some(serde_json::Value::String(text)) if !text.is_empty() => return Some(text.clone()),
            // FastAPI validation errors: a list of {loc, msg, type}.
            Some(serde_json::Value::Array(items)) if key == "detail" => {
                let joined: Vec<String> = items
                    .iter()
                    .filter_map(|item| {
                        let msg = item.get("msg").and_then(|m| m.as_str())?;
                        match item.get("loc").and_then(|l| l.as_array()) {
                            Some(loc) if !loc.is_empty() => {
                                let path: Vec<String> = loc.iter().map(scalar_string).collect();
                                Some(format!("{}: {msg}", path.join(".")))
                            }
                            _ => Some(msg.to_string()),
                        }
                    })
                    .collect();
                if !joined.is_empty() {
                    return Some(joined.join("; "));
                }
            }
            // Some gateways nest one more level: {"message": {"detail": "..."}}.
            Some(nested @ serde_json::Value::Object(_)) => {
                if let Some(text) = nested_message(nested) {
                    return Some(text);
                }
            }
            _ => {}
        }
    }
    None
}

fn scalar_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) if !text.is_empty() => Some(text.clone()),
        serde_json::Value::Null => None,
        other if other.is_object() || other.is_array() => serde_json::to_string(other).ok(),
        other => Some(other.to_string()),
    }
}

fn scalar_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn non_empty(text: String) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Fallback for a body that is not JSON (an HTML gateway page, a bare string,
/// or nothing at all).
fn plain_message(body: &str, status: u16) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        // Matches the wording upstream's overflow detector keys on for Cerebras.
        return format!("{status} status code (no body)");
    }
    // An HTML error page has no useful first line; report the status and a
    // marker instead of `<!DOCTYPE html>`.
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("<!doctype") || lower.starts_with("<html") {
        return format!("{status} status code (html error page)");
    }
    let line = trimmed.lines().next().unwrap_or(trimmed);
    truncate_error_text(line, MAX_PLAIN_MESSAGE_CHARS)
}

/// Truncate on a character boundary, noting how much was dropped.
///
/// Slicing by byte index would panic on a multi-byte character straddling the
/// cut, which is reachable from any provider that returns non-ASCII error text.
pub fn truncate_error_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(max_chars).collect();
    let dropped = text.chars().count() - max_chars;
    format!("{kept}... [truncated {dropped} chars]")
}

/// A provider error reduced to the parts a display string is built from.
/// Port of upstream's `NormalizedProviderError`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NormalizedProviderError {
    /// HTTP status, when one is known.
    pub status: Option<u16>,
    /// Raw body reason, trimmed and truncated to the cap.
    pub body: Option<String>,
    /// The error's own message.
    pub message: String,
    /// True when `message` already contains `body`, so printing both would
    /// duplicate it. Upstream's Anthropic / `@google/genai` happy path.
    pub message_carries_body: bool,
}

/// Normalize an explicit status/body/message triple.
pub fn normalize_provider_error(
    status: Option<u16>,
    body: Option<&str>,
    message: &str,
) -> NormalizedProviderError {
    let body = body
        .map(str::trim)
        .filter(|b| !b.is_empty() && *b != "{}" && *b != "[]")
        .map(|b| truncate_error_text(b, MAX_PROVIDER_ERROR_BODY_CHARS));
    let message_carries_body = match &body {
        None => true,
        Some(body) => message.contains(body.as_str()),
    };
    NormalizedProviderError {
        status,
        body,
        message: message.to_string(),
        message_carries_body,
    }
}

impl From<&HttpError> for NormalizedProviderError {
    fn from(error: &HttpError) -> Self {
        match error {
            HttpError::Status {
                status,
                message,
                body,
                ..
            } => {
                let body_text = body.as_ref().and_then(|b| serde_json::to_string(b).ok());
                normalize_provider_error(Some(*status), body_text.as_deref(), message)
            }
            other => normalize_provider_error(None, None, &other.to_string()),
        }
    }
}

/// Compose a display string from a normalized error.
///
/// - no prefix: `"<status>: <body>"`
/// - prefix: `"<prefix> (<status>): <body>"`
///
/// When the message already carries the body, or no body/status was extracted,
/// the message is used instead of the body so nothing is printed twice.
pub fn format_provider_error(norm: &NormalizedProviderError, prefix: Option<&str>) -> String {
    match (&norm.body, norm.status, prefix) {
        (Some(body), Some(status), Some(prefix)) if !norm.message_carries_body => {
            format!("{prefix} ({status}): {body}")
        }
        (Some(body), Some(status), None) if !norm.message_carries_body => {
            format!("{status}: {body}")
        }
        (_, Some(status), Some(prefix)) => format!("{prefix} ({status}): {}", norm.message),
        _ => norm.message.clone(),
    }
}

/// `retry-after` in seconds or as an HTTP-date, plus the provider-specific
/// millisecond headers that take precedence over it.
pub fn retry_after_ms(headers: &OwnedHeaders) -> Option<u64> {
    // Anthropic and OpenAI send a millisecond header alongside `retry-after`;
    // it is more precise, so it wins.
    for key in ["retry-after-ms", "x-ratelimit-reset-after-ms"] {
        if let Some(ms) = headers.get(key).and_then(|v| v.trim().parse::<f64>().ok()) {
            if ms.is_finite() {
                return Some(ms.max(0.0) as u64);
            }
        }
    }

    let raw = headers.get("retry-after")?.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(seconds) = raw.parse::<f64>() {
        if seconds.is_finite() {
            return Some((seconds.max(0.0) * 1000.0) as u64);
        }
    }
    // HTTP-date form: the delay is the difference from now, floored at zero.
    http_date_delay_ms(raw)
}

/// Delay until an HTTP-date, in ms. Returns `None` when the date is unparseable.
fn http_date_delay_ms(raw: &str) -> Option<u64> {
    let parsed = chrono::DateTime::parse_from_rfc2822(raw)
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(raw))
        .ok()
        // IMF-fixdate uses "GMT" where RFC 2822 wants a numeric offset.
        .or_else(|| {
            let swapped = raw.strip_suffix("GMT").map(|head| format!("{head}+0000"))?;
            chrono::DateTime::parse_from_rfc2822(swapped.trim()).ok()
        })?;
    let delta = parsed.timestamp_millis() - chrono::Utc::now().timestamp_millis();
    Some(delta.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message_of(err: &HttpError) -> &str {
        match err {
            HttpError::Status { message, .. } => message,
            _ => panic!("expected a status error, got {err:?}"),
        }
    }

    fn extract(status: u16, body: &str) -> HttpError {
        extract_error(status, &Default::default(), body)
    }

    fn headers(pairs: &[(&str, &str)]) -> OwnedHeaders {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    // --- real provider envelopes ------------------------------------------

    #[test]
    fn reads_the_anthropic_envelope() {
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 213462 tokens > 200000 maximum"}}"#;
        assert_eq!(
            message_of(&extract(400, body)),
            "prompt is too long: 213462 tokens > 200000 maximum"
        );
    }

    #[test]
    fn reads_the_openai_envelope() {
        let body = r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","param":null,"code":"insufficient_quota"}}"#;
        assert_eq!(
            message_of(&extract(429, body)),
            "You exceeded your current quota"
        );
    }

    #[test]
    fn reads_the_google_envelope_in_object_and_array_form() {
        let object = r#"{"error":{"code":400,"message":"The input token count (1196265) exceeds the maximum","status":"INVALID_ARGUMENT"}}"#;
        assert_eq!(
            message_of(&extract(400, object)),
            "The input token count (1196265) exceeds the maximum"
        );
        // Vertex wraps the same envelope in an array.
        let array = format!("[{object}]");
        assert_eq!(
            message_of(&extract(400, &array)),
            "The input token count (1196265) exceeds the maximum"
        );
    }

    #[test]
    fn appends_the_openrouter_upstream_detail() {
        let body = r#"{"error":{"code":429,"message":"Provider returned error","metadata":{"provider_name":"Together","raw":"rate limit exceeded on upstream"}}}"#;
        assert_eq!(
            message_of(&extract(429, body)),
            "Provider returned error\nrate limit exceeded on upstream"
        );
    }

    #[test]
    fn does_not_duplicate_openrouter_detail_already_in_the_message() {
        let body = r#"{"error":{"code":429,"message":"Provider returned error: rate limited","metadata":{"raw":"rate limited"}}}"#;
        assert_eq!(
            message_of(&extract(429, body)),
            "Provider returned error: rate limited"
        );
    }

    #[test]
    fn reads_the_mistral_and_bedrock_top_level_message() {
        let mistral = r#"{"object":"error","message":"Prompt contains 300000 tokens","type":"invalid_request_error","code":null}"#;
        assert_eq!(
            message_of(&extract(400, mistral)),
            "Prompt contains 300000 tokens"
        );
        let bedrock = r#"{"message":"Input is too long for requested model."}"#;
        assert_eq!(
            message_of(&extract(400, bedrock)),
            "Input is too long for requested model."
        );
    }

    #[test]
    fn reads_the_ollama_string_error() {
        let body = r#"{"error":"prompt too long; exceeded max context length by 100918 tokens"}"#;
        assert_eq!(
            message_of(&extract(400, body)),
            "prompt too long; exceeded max context length by 100918 tokens"
        );
    }

    #[test]
    fn reads_fastapi_detail_in_string_and_list_form() {
        assert_eq!(
            message_of(&extract(422, r#"{"detail":"model not found"}"#)),
            "model not found"
        );
        let list = r#"{"detail":[{"loc":["body","messages",0],"msg":"field required","type":"value_error.missing"}]}"#;
        assert_eq!(
            message_of(&extract(422, list)),
            "body.messages.0: field required"
        );
    }

    #[test]
    fn reads_the_cloudflare_errors_array() {
        let body =
            r#"{"errors":[{"code":10000,"message":"Authentication error"}],"success":false}"#;
        assert_eq!(message_of(&extract(403, body)), "Authentication error");
    }

    #[test]
    fn falls_back_to_the_error_object_when_it_has_no_message_field() {
        let body = r#"{"error":{"code":"content_filter","reason":"violence"}}"#;
        let message = message_of(&extract(400, body)).to_string();
        assert!(message.contains("content_filter"), "{message}");
    }

    // --- non-JSON bodies ---------------------------------------------------

    #[test]
    fn falls_back_to_the_first_line_of_a_plain_body() {
        assert_eq!(
            message_of(&extract(500, "upstream exploded\nstack trace")),
            "upstream exploded"
        );
    }

    #[test]
    fn an_empty_body_reports_the_status() {
        // The wording matters: the overflow detector matches on it for Cerebras.
        assert_eq!(message_of(&extract(413, "")), "413 status code (no body)");
        assert_eq!(
            message_of(&extract(400, "   \n ")),
            "400 status code (no body)"
        );
        assert!(crate::overflow::is_context_overflow_message(message_of(
            &extract(413, "")
        )));
    }

    #[test]
    fn an_html_gateway_page_is_not_reported_as_doctype() {
        let body = "<!DOCTYPE html>\n<html><head><title>502 Bad Gateway</title></head></html>";
        assert_eq!(
            message_of(&extract(502, body)),
            "502 status code (html error page)"
        );
    }

    #[test]
    fn a_very_long_line_is_truncated_on_a_character_boundary() {
        // Multi-byte characters at the cut used to panic on a byte-index slice.
        let body = "é".repeat(MAX_PLAIN_MESSAGE_CHARS + 20);
        let message = message_of(&extract(500, &body)).to_string();
        assert!(message.contains("[truncated 20 chars]"), "{message}");
        assert!(message.chars().count() < body.chars().count() + 40);
    }

    #[test]
    fn the_parsed_body_is_attached_to_the_error() {
        let err = extract(429, r#"{"error":{"message":"slow down"}}"#);
        match err {
            HttpError::Status { body, status, .. } => {
                assert_eq!(status, 429);
                assert_eq!(body.unwrap()["error"]["message"], "slow down");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    // --- retry-after -------------------------------------------------------

    #[test]
    fn reads_retry_after_seconds() {
        assert_eq!(
            retry_after_ms(&headers(&[("retry-after", "2")])),
            Some(2000)
        );
        assert_eq!(
            retry_after_ms(&headers(&[("retry-after", "0.5")])),
            Some(500)
        );
        // Negative values are clamped rather than wrapping.
        assert_eq!(retry_after_ms(&headers(&[("retry-after", "-5")])), Some(0));
    }

    #[test]
    fn millisecond_headers_win_over_retry_after() {
        let h = headers(&[("retry-after", "60"), ("retry-after-ms", "1500")]);
        assert_eq!(retry_after_ms(&h), Some(1500));
        let h = headers(&[("retry-after", "60"), ("x-ratelimit-reset-after-ms", "250")]);
        assert_eq!(retry_after_ms(&h), Some(250));
    }

    #[test]
    fn reads_retry_after_as_an_http_date() {
        let future = chrono::Utc::now() + chrono::Duration::seconds(30);
        let imf = future.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        let delay = retry_after_ms(&headers(&[("retry-after", &imf)])).expect("http-date parses");
        assert!((25_000..=31_000).contains(&delay), "{delay}");

        // A date in the past means "retry now", not a huge or negative delay.
        let past = chrono::Utc::now() - chrono::Duration::seconds(30);
        let imf = past.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        assert_eq!(retry_after_ms(&headers(&[("retry-after", &imf)])), Some(0));
    }

    #[test]
    fn an_unparseable_or_absent_retry_after_is_none() {
        assert_eq!(retry_after_ms(&headers(&[])), None);
        assert_eq!(retry_after_ms(&headers(&[("retry-after", "soon")])), None);
        assert_eq!(retry_after_ms(&headers(&[("retry-after", "")])), None);
    }

    // --- normalize / format ------------------------------------------------

    #[test]
    fn normalization_flags_a_message_that_already_carries_the_body() {
        let norm = normalize_provider_error(
            Some(500),
            Some("upstream exploded"),
            "500: upstream exploded",
        );
        assert!(norm.message_carries_body);
        assert_eq!(format_provider_error(&norm, None), "500: upstream exploded");
    }

    #[test]
    fn normalization_surfaces_a_body_the_message_hides() {
        let norm = normalize_provider_error(
            Some(403),
            Some(r#"{"error":"blocked by gateway WAF"}"#),
            "403 status code (no body)",
        );
        assert!(!norm.message_carries_body);
        let formatted = format_provider_error(&norm, None);
        assert!(formatted.contains("403") && formatted.contains("blocked by gateway WAF"));
        assert_eq!(
            format_provider_error(&norm, Some("OpenAI API error")),
            r#"OpenAI API error (403): {"error":"blocked by gateway WAF"}"#
        );
    }

    #[test]
    fn an_empty_parsed_body_counts_as_no_body() {
        for body in ["{}", "[]", "", "   "] {
            let norm = normalize_provider_error(Some(403), Some(body), "403 status code (no body)");
            assert!(norm.body.is_none(), "{body}");
            assert!(norm.message_carries_body);
        }
    }

    #[test]
    fn a_body_is_truncated_at_the_cap() {
        let long = "x".repeat(MAX_PROVIDER_ERROR_BODY_CHARS + 50);
        let norm = normalize_provider_error(Some(500), Some(&long), "failed");
        let body = norm.body.unwrap();
        assert!(body.contains("... [truncated 50 chars]"));
        assert!(body.len() < long.len() + 40);
    }

    #[test]
    fn formatting_keeps_the_prefix_and_status_when_the_message_carries_the_body() {
        let body = r#"{"error":{"message":"Permission denied"}}"#;
        let norm = normalize_provider_error(Some(403), Some(body), body);
        assert_eq!(
            format_provider_error(&norm, Some("Google API error")),
            format!("Google API error (403): {body}")
        );
    }

    #[test]
    fn formatting_a_statusless_error_returns_the_bare_message() {
        let norm = normalize_provider_error(None, None, "connection reset");
        assert_eq!(format_provider_error(&norm, None), "connection reset");
        assert_eq!(
            format_provider_error(&norm, Some("Prefix")),
            "connection reset"
        );
    }

    #[test]
    fn normalizing_an_http_error_round_trips_status_and_body() {
        let error = extract(403, r#"{"error":"blocked by gateway WAF"}"#);
        let norm = NormalizedProviderError::from(&error);
        assert_eq!(norm.status, Some(403));
        assert_eq!(norm.message, "blocked by gateway WAF");
        // The extractor already lifted the reason into the message.
        assert!(norm
            .body
            .as_deref()
            .unwrap()
            .contains("blocked by gateway WAF"));

        let norm = NormalizedProviderError::from(&HttpError::Transport("reset".into()));
        assert_eq!(norm.status, None);
        assert_eq!(norm.message, "transport error: reset");
    }
}
