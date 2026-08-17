//! Port of `api/openai-codex-responses.ts` — **SSE transport only**.
//!
//! Upstream also has a WebSocket transport with a per-session connection cache
//! and `previous_response_id` continuation. That is deliberately not ported yet
//! (see the crate report); `transport: websocket*` falls through to SSE here.
//! Upstream's optional zstd request-body compression is also skipped — it is a
//! best-effort optimisation there too, with the same plain-JSON fallback.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use pi_core::event::{AssistantMessageEventSink, DoneReason, ErrorReason};
use pi_core::options::{SimpleStreamOptions, StreamOptions};
use pi_core::{
    AiError, ApiClient, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream,
    CacheRetention, Context, Model, StopReason,
};
use pi_http::client::JsonRequest;
use pi_http::HttpClient;
use serde_json::{json, Map, Value};

use crate::openai_prompt_cache::clamp_openai_prompt_cache_key;
use crate::openai_responses_shared::{
    convert_responses_messages, convert_responses_tools, run_responses_sse,
    ConvertResponsesMessagesOptions, ConvertResponsesToolsOptions, MappedEvent,
    ResponsesStreamOptions, ResponsesStreamProcessor,
};
use crate::options::{provider_opt_str, provider_opt_value, ProviderOptionKey};
use crate::transport::{
    apply_on_payload, assign_all, assign_header, base_json_sse_headers, finalize_headers,
    model_headers,
};
use crate::util::{format_provider_error, pi_user_agent, MappedLevel};
use pi_provider_common::constrained_sampling::create_grammar_tool_input_properties;
use pi_provider_common::simple_options::build_base_options;

pub const API: &str = "openai-codex-responses";

const DEFAULT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";
const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";
const DEFAULT_MAX_RETRIES: u32 = 0;
const BASE_DELAY_MS: u64 = 1000;
const DEFAULT_MAX_RETRY_DELAY_MS: u64 = 60_000;
const CODEX_TOOL_CALL_PROVIDERS: [&str; 3] = ["openai", "openai-codex", "opencode"];

const CODEX_RESPONSE_STATUSES: [&str; 6] = [
    "completed",
    "incomplete",
    "failed",
    "cancelled",
    "queued",
    "in_progress",
];

/// The `openai-codex-responses` [`ApiClient`].
#[derive(Clone)]
pub struct OpenAiCodexResponsesClient {
    http: Arc<HttpClient>,
}

impl Default for OpenAiCodexResponsesClient {
    fn default() -> Self {
        Self {
            http: HttpClient::shared(),
        }
    }
}

impl std::fmt::Debug for OpenAiCodexResponsesClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OpenAiCodexResponsesClient")
    }
}

impl OpenAiCodexResponsesClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_http_client(http: Arc<HttpClient>) -> Self {
        Self { http }
    }
}

#[async_trait]
impl ApiClient for OpenAiCodexResponsesClient {
    fn api(&self) -> &str {
        API
    }

    async fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: &StreamOptions,
    ) -> Result<AssistantMessageEventStream, AiError> {
        let (sink, stream) = AssistantMessageEventStream::channel(64);
        let http = self.http.clone();
        let model = model.clone();
        let context = context.clone();
        let options = options.clone();
        tokio::spawn(async move {
            run_stream(http, model, context, options, sink).await;
        });
        Ok(stream)
    }

    async fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, AiError> {
        // A missing credential is reported as an in-stream `Error` event, not
        // as `Err`. `stream_inner` already checks the same condition on the
        // request path; upstream's `streamSimple` additionally throws
        // synchronously here, but the `ApiClient` contract in `pi-core`
        // reserves `Err` for programmer errors and an FFI caller should not
        // have two failure paths for one condition.
        let mut base = build_base_options(
            model,
            context,
            options,
            options.stream.request.api_key.as_deref(),
        );
        let clamped = options
            .reasoning
            .map(|level| model.clamp_thinking_level(level.into()));
        if let Some(effort) = clamped.and_then(|l| l.level()) {
            base.provider_options.insert(
                ProviderOptionKey::ReasoningEffort.as_str().to_string(),
                json!(effort.as_str()),
            );
        }
        self.stream(model, context, &base).await
    }
}

async fn run_stream(
    http: Arc<HttpClient>,
    model: Model,
    context: Context,
    options: StreamOptions,
    sink: AssistantMessageEventSink,
) {
    let mut output = AssistantMessage::pending(API, &model.provider, &model.id);

    match stream_inner(&http, &model, &context, &options, &mut output, &sink).await {
        Ok(reason) => {
            let _ = sink
                .send(AssistantMessageEvent::Done {
                    reason,
                    message: output,
                })
                .await;
        }
        Err(err) => {
            let aborted = options.request.is_aborted() || err.is_aborted();
            output.stop_reason = if aborted {
                StopReason::Aborted
            } else {
                StopReason::Error
            };
            output.error_message = Some(format_provider_error(&err, None));
            let _ = sink
                .send(AssistantMessageEvent::Error {
                    reason: if aborted {
                        ErrorReason::Aborted
                    } else {
                        ErrorReason::Error
                    },
                    error: output,
                })
                .await;
        }
    }
}

async fn stream_inner(
    http: &HttpClient,
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    output: &mut AssistantMessage,
    sink: &AssistantMessageEventSink,
) -> Result<DoneReason, AiError> {
    let Some(api_key) = options.request.api_key.clone().filter(|k| !k.is_empty()) else {
        return Err(AiError::auth(format!(
            "No API key for provider: {}",
            model.provider
        )));
    };

    let account_id = extract_account_id(&api_key)?;
    let grammar_props = create_grammar_tool_input_properties(
        context.tools.as_deref(),
        model
            .compat
            .as_ref()
            .and_then(|c| c.supports_openai_grammar_tools)
            .unwrap_or(false),
    )?;
    let cache_session_id = match options.cache_retention {
        Some(CacheRetention::None) => None,
        _ => options.session_id.clone(),
    };
    let codex_session_id = clamp_openai_prompt_cache_key(cache_session_id.as_deref());

    let body = build_codex_request_body(model, context, options, codex_session_id.as_deref())?;
    let body = apply_on_payload(body, model, &options.request);
    let headers = build_sse_headers(
        model,
        &options.request.headers,
        &account_id,
        &api_key,
        codex_session_id.as_deref(),
    );

    let url = resolve_codex_url(&model.base_url);
    let mut response = send_with_codex_retry(http, url, body, headers, model, options).await?;

    let _ = sink
        .send(AssistantMessageEvent::Start {
            partial: output.clone(),
        })
        .await;

    let mut processor = ResponsesStreamProcessor::new(ResponsesStreamOptions {
        service_tier: provider_opt_str(options, ProviderOptionKey::ServiceTier),
        grammar_tool_input_properties: grammar_props,
        codex_service_tier_resolution: true,
        apply_service_tier_pricing: true,
    });

    run_responses_sse(
        &mut response,
        &options.request.signal,
        &mut processor,
        output,
        model,
        sink,
        Some(&map_codex_event),
    )
    .await?;

    if options.request.is_aborted() {
        return Err(AiError::Aborted);
    }
    // Port of `assertSuccessfulOutput`.
    if output.stop_reason == StopReason::Pending {
        return Err(AiError::protocol(
            "Codex stream ended without a stop reason",
        ));
    }
    if matches!(output.stop_reason, StopReason::Error | StopReason::Aborted) {
        return Err(AiError::other(
            output
                .error_message
                .clone()
                .unwrap_or_else(|| "An unknown error occurred".to_string()),
        ));
    }

    Ok(match output.stop_reason {
        StopReason::Length => DoneReason::Length,
        StopReason::ToolUse => DoneReason::ToolUse,
        _ => DoneReason::Stop,
    })
}

// ============================================================================
// Codex event dialect
// ============================================================================

/// Port of `mapCodexEvents`.
fn map_codex_event(event: Value, output: &mut AssistantMessage) -> Result<MappedEvent, AiError> {
    let Some(event_type) = event
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return Ok(MappedEvent::Skip);
    };

    if event_type == "error" {
        let (code, message) = extract_codex_event_error(&event);
        let detail = message.or(code).unwrap_or_else(|| event.to_string());
        return Err(AiError::other(format!("Codex error: {detail}")));
    }

    if event_type == "response.failed" {
        let message = event
            .pointer("/response/error/message")
            .and_then(Value::as_str)
            .unwrap_or("Codex response failed")
            .to_string();
        return Err(AiError::other(message));
    }

    if matches!(
        event_type.as_str(),
        "response.done" | "response.completed" | "response.incomplete"
    ) {
        if let Some(end_turn) = event.pointer("/response/end_turn").and_then(Value::as_bool) {
            output.end_turn = Some(end_turn);
        }
        let mut normalized = event.clone();
        if let Some(obj) = normalized.as_object_mut() {
            obj.insert("type".into(), json!("response.completed"));
            if let Some(Value::Object(response)) = obj.get_mut("response") {
                let status = response
                    .get("status")
                    .and_then(Value::as_str)
                    .filter(|s| CODEX_RESPONSE_STATUSES.contains(s))
                    .map(|s| json!(s))
                    .unwrap_or(Value::Null);
                response.insert("status".into(), status);
            }
        }
        return Ok(MappedEvent::EmitAndStop(normalized));
    }

    Ok(MappedEvent::Emit(event))
}

fn extract_codex_event_error(event: &Value) -> (Option<String>, Option<String>) {
    let nested = event.get("error").filter(|e| e.is_object());
    let code = event
        .get("code")
        .and_then(Value::as_str)
        .or_else(|| nested.and_then(|n| n.get("code")).and_then(Value::as_str))
        .map(str::to_string);
    let message = event
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| {
            nested
                .and_then(|n| n.get("message"))
                .and_then(Value::as_str)
        })
        .map(str::to_string);
    (code, message)
}

// ============================================================================
// Retry (Codex has its own classification, not pi-http's)
// ============================================================================

fn is_terminal_rate_limit_error(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "gousagelimiterror",
        "freeusagelimiterror",
        "monthly usage limit reached",
        "available balance",
        "insufficient_quota",
        "out of budget",
        "quota exceeded",
        "billing",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn is_retryable_error(status: u16, text: &str) -> bool {
    if status == 429 && is_terminal_rate_limit_error(text) {
        return false;
    }
    if matches!(status, 429 | 500 | 502 | 503 | 504) {
        return true;
    }
    let lower = text.to_lowercase();
    [
        "rate limit",
        "rate-limit",
        "ratelimit",
        "overloaded",
        "service unavailable",
        "service-unavailable",
        "upstream connect",
        "upstream-connect",
        "connection refused",
        "connection-refused",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

async fn send_with_codex_retry(
    http: &HttpClient,
    url: String,
    body: Value,
    headers: pi_http::HeaderMap,
    _model: &Model,
    options: &StreamOptions,
) -> Result<pi_http::SseResponse, AiError> {
    let max_retries = options.request.max_retries.unwrap_or(DEFAULT_MAX_RETRIES);
    let max_retry_delay_ms = options
        .request
        .max_retry_delay_ms
        .unwrap_or(DEFAULT_MAX_RETRY_DELAY_MS);

    let mut last_error: Option<AiError> = None;
    for attempt in 0..=max_retries {
        if options.request.is_aborted() {
            return Err(AiError::Aborted);
        }

        let mut request = JsonRequest::post(url.clone(), body.clone());
        request.headers = headers.clone();
        request.timeout_ms = options.request.timeout_ms;
        request.signal = options.request.signal.clone();

        match http.post_sse(request).await {
            Ok(response) => {
                if let Some(on_response) = &options.request.on_response {
                    on_response(
                        &pi_core::options::ProviderResponse {
                            status: response.status,
                            headers: response.headers.clone(),
                        },
                        _model,
                    );
                }
                return Ok(response);
            }
            Err(err) => {
                let err = AiError::from(err);
                if err.is_aborted() {
                    return Err(err);
                }
                let (status, retry_after_ms, text) = match &err {
                    AiError::Provider {
                        status,
                        message,
                        body,
                        retry_after_ms,
                    } => (
                        *status,
                        *retry_after_ms,
                        body.as_ref()
                            .map(|b| b.to_string())
                            .unwrap_or_else(|| message.clone()),
                    ),
                    AiError::Auth { message } => (401, None, message.clone()),
                    other => (0, None, other.message()),
                };

                let friendly = codex_friendly_message(status, &text);
                let err = match friendly {
                    Some(message) => AiError::other(message),
                    None => err,
                };

                if attempt < max_retries && is_retryable_error(status, &text) {
                    let delay_ms = match retry_after_ms {
                        Some(delay) => {
                            if max_retry_delay_ms > 0 && delay > max_retry_delay_ms {
                                return Err(AiError::other(format!(
                                    "Server requested {}s retry delay (max: {}s)",
                                    delay.div_ceil(1000),
                                    max_retry_delay_ms.div_ceil(1000)
                                )));
                            }
                            delay
                        }
                        None => BASE_DELAY_MS * 2u64.pow(attempt),
                    };
                    match &options.request.signal {
                        Some(signal) => {
                            tokio::select! {
                                biased;
                                _ = signal.aborted() => return Err(AiError::Aborted),
                                _ = tokio::time::sleep(Duration::from_millis(delay_ms)) => {}
                            }
                        }
                        None => tokio::time::sleep(Duration::from_millis(delay_ms)).await,
                    }
                    last_error = Some(err);
                    continue;
                }
                return Err(err);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| AiError::other("Failed after retries")))
}

/// Port of the usage-limit half of `parseErrorResponse`.
fn codex_friendly_message(status: u16, text: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(text).ok()?;
    let error = parsed.get("error")?;
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .or_else(|| error.get("type").and_then(Value::as_str))
        .unwrap_or("");
    let lower = code.to_lowercase();
    let is_limit = lower.contains("usage_limit_reached")
        || lower.contains("usage_not_included")
        || lower.contains("rate_limit_exceeded")
        || status == 429;
    if !is_limit {
        return None;
    }
    let plan = error
        .get("plan_type")
        .and_then(Value::as_str)
        .map(|p| format!(" ({} plan)", p.to_lowercase()))
        .unwrap_or_default();
    let when = error
        .get("resets_at")
        .and_then(Value::as_i64)
        .map(|resets_at| {
            let minutes = ((resets_at * 1000 - pi_core::now_ms()) as f64 / 60000.0)
                .round()
                .max(0.0) as i64;
            format!(" Try again in ~{minutes} min.")
        })
        .unwrap_or_default();
    Some(
        format!("You have hit your ChatGPT usage limit{plan}.{when}")
            .trim()
            .to_string(),
    )
}

// ============================================================================
// Request building
// ============================================================================

/// Port of `resolveCodexUrl`.
pub fn resolve_codex_url(base_url: &str) -> String {
    let raw = if base_url.trim().is_empty() {
        DEFAULT_CODEX_BASE_URL
    } else {
        base_url
    };
    let normalized = raw.trim_end_matches('/');
    if normalized.ends_with("/codex/responses") {
        return normalized.to_string();
    }
    if normalized.ends_with("/codex") {
        return format!("{normalized}/responses");
    }
    format!("{normalized}/codex/responses")
}

/// Port of `buildRequestBody`.
pub fn build_codex_request_body(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    cache_session_id: Option<&str>,
) -> Result<Value, AiError> {
    let compat = model.compat.as_ref();
    let supports_strict_mode = compat.and_then(|c| c.supports_strict_mode).unwrap_or(true);
    let supports_grammar = compat
        .and_then(|c| c.supports_openai_grammar_tools)
        .unwrap_or(false);
    let deferred_tools_mode = if compat
        .and_then(|c| c.supports_additional_tools)
        .unwrap_or(false)
    {
        Some(crate::compat::DeferredToolsMode::AdditionalTools)
    } else if compat.and_then(|c| c.supports_tool_search).unwrap_or(false) {
        Some(crate::compat::DeferredToolsMode::ToolSearch)
    } else {
        None
    };

    let grammar_props =
        create_grammar_tool_input_properties(context.tools.as_deref(), supports_grammar)?;
    let (immediate, deferred) = crate::openai_responses_shared::split_deferred_tools(
        context,
        deferred_tools_mode.is_some(),
    );

    // Codex sends `strict: null` rather than the `false` default.
    let tool_options = ConvertResponsesToolsOptions::new(supports_strict_mode, supports_grammar)
        .with_explicit_null_strict();

    let messages = convert_responses_messages(
        model,
        context,
        &HashSet::from(CODEX_TOOL_CALL_PROVIDERS),
        &ConvertResponsesMessagesOptions {
            // Codex takes the system prompt as top-level `instructions`.
            include_system_prompt: false,
            grammar_tool_input_properties: Some(&grammar_props),
            deferred_tools: Some(&deferred),
            deferred_tools_mode,
            tool_options: tool_options.clone(),
        },
    )?;

    let mut body = Map::new();
    body.insert("model".into(), json!(model.id));
    body.insert("store".into(), json!(false));
    body.insert("stream".into(), json!(true));
    body.insert(
        "instructions".into(),
        json!(context
            .system_prompt
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "You are a helpful assistant.".to_string())),
    );
    body.insert("input".into(), Value::Array(messages));
    body.insert(
        "text".into(),
        json!({
            "verbosity": provider_opt_str(options, ProviderOptionKey::TextVerbosity)
                .unwrap_or_else(|| "low".to_string())
        }),
    );
    body.insert("include".into(), json!(["reasoning.encrypted_content"]));
    if let Some(session_id) = cache_session_id {
        body.insert("prompt_cache_key".into(), json!(session_id));
    }
    body.insert(
        "tool_choice".into(),
        provider_opt_value(options, ProviderOptionKey::ToolChoice).unwrap_or_else(|| json!("auto")),
    );
    body.insert("parallel_tool_calls".into(), json!(true));

    if let Some(temperature) = options.temperature {
        body.insert("temperature".into(), json!(temperature));
    }
    if let Some(tier) = provider_opt_value(options, ProviderOptionKey::ServiceTier) {
        body.insert("service_tier".into(), tier);
    }
    if !immediate.is_empty() {
        body.insert(
            "tools".into(),
            Value::Array(convert_responses_tools(&immediate, &tool_options)?),
        );
    }

    if let Some(effort) = provider_opt_str(options, ProviderOptionKey::ReasoningEffort) {
        // Codex additionally accepts "none", which maps through thinkingLevelMap.off.
        let resolved = if effort == "none" {
            MappedLevel::lookup(model, pi_core::model::ModelThinkingLevel::Off)
        } else {
            match crate::openai_responses::parse_level(&effort) {
                Some(level) => match MappedLevel::lookup(model, level.into()) {
                    MappedLevel::Missing => MappedLevel::Value(effort.clone()),
                    other => other,
                },
                None => MappedLevel::Value(effort.clone()),
            }
        };
        let resolved = match resolved {
            MappedLevel::Missing => Some("none".to_string()),
            MappedLevel::Value(value) => Some(value),
            // An explicit `null` suppresses the reasoning block entirely.
            MappedLevel::Null => None,
        };
        if let Some(resolved) = resolved {
            body.insert(
                "reasoning".into(),
                json!({
                    "effort": resolved,
                    "summary": provider_opt_str(options, ProviderOptionKey::ReasoningSummary)
                        .unwrap_or_else(|| "auto".to_string())
                }),
            );
        }
    }

    Ok(Value::Object(body))
}

// ============================================================================
// Auth & headers
// ============================================================================

/// Port of `extractAccountId`: read `chatgpt_account_id` from the JWT payload.
pub fn extract_account_id(token: &str) -> Result<String, AiError> {
    let failed = || AiError::auth("Failed to extract accountId from token");
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(failed());
    }
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(parts[1]))
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(parts[1]))
        .map_err(|_| failed())?;
    let payload: Value = serde_json::from_slice(&decoded).map_err(|_| failed())?;
    payload
        .get(JWT_CLAIM_PATH)
        .and_then(|claim| claim.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(failed)
}

/// Port of `buildSSEHeaders` (on top of `buildBaseCodexHeaders`).
fn build_sse_headers(
    model: &Model,
    option_headers: &pi_core::options::ProviderHeaders,
    account_id: &str,
    token: &str,
    session_id: Option<&str>,
) -> pi_http::HeaderMap {
    let mut headers = model_headers(model);
    assign_all(&mut headers, option_headers);
    assign_header(
        &mut headers,
        "Authorization",
        Some(format!("Bearer {token}")),
    );
    assign_header(
        &mut headers,
        "chatgpt-account-id",
        Some(account_id.to_string()),
    );
    assign_header(&mut headers, "originator", Some("pi".to_string()));
    assign_header(&mut headers, "User-Agent", Some(pi_user_agent()));
    assign_header(
        &mut headers,
        "OpenAI-Beta",
        Some("responses=experimental".to_string()),
    );
    assign_header(
        &mut headers,
        "accept",
        Some("text/event-stream".to_string()),
    );
    assign_header(
        &mut headers,
        "content-type",
        Some("application/json".to_string()),
    );
    if let Some(session_id) = session_id {
        assign_header(&mut headers, "session-id", Some(session_id.to_string()));
        assign_header(
            &mut headers,
            "x-client-request-id",
            Some(session_id.to_string()),
        );
    }
    finalize_headers(base_json_sse_headers(token), &headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_url_is_idempotent() {
        assert_eq!(
            resolve_codex_url(""),
            "https://chatgpt.com/backend-api/codex/responses"
        );
        assert_eq!(
            resolve_codex_url("https://host/backend-api"),
            "https://host/backend-api/codex/responses"
        );
        assert_eq!(
            resolve_codex_url("https://host/backend-api/codex"),
            "https://host/backend-api/codex/responses"
        );
        assert_eq!(
            resolve_codex_url("https://host/backend-api/codex/responses/"),
            "https://host/backend-api/codex/responses"
        );
    }

    #[test]
    fn account_id_comes_from_the_jwt_payload() {
        let payload = json!({ JWT_CLAIM_PATH: { "chatgpt_account_id": "acct_123" } });
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&payload).unwrap());
        let token = format!("header.{encoded}.signature");
        assert_eq!(extract_account_id(&token).unwrap(), "acct_123");
    }

    #[test]
    fn malformed_tokens_are_auth_errors() {
        assert_eq!(extract_account_id("not-a-jwt").unwrap_err().code(), "auth");
        assert_eq!(extract_account_id("a.!!!.c").unwrap_err().code(), "auth");
    }

    #[test]
    fn terminal_rate_limits_are_not_retried() {
        assert!(!is_retryable_error(429, "Monthly usage limit reached"));
        assert!(is_retryable_error(429, "slow down"));
        assert!(is_retryable_error(503, ""));
        assert!(!is_retryable_error(400, "bad request"));
        assert!(is_retryable_error(400, "upstream connect failure"));
    }
}
