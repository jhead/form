//! `pi-messages` API implementation. Port of `packages/ai/src/api/pi-messages.ts`
//! (plus the trivial `.lazy.ts`, which has no Rust equivalent — there is no
//! module loading to defer).
//!
//! pi's own message protocol streamed straight to a backend: one POST of
//! `{ model, context, options }` to `<baseUrl>/messages`, answered with an SSE
//! stream of serialized assistant-message events and a terminal `done`/`error`.
//! The Radius gateway speaks it, and so does any custom provider declaring
//! `"api": "pi-messages"`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pi_core::api::{ApiClient, ApiClientRef};
use pi_core::content::{AssistantContent, TextContent, ThinkingContent, ToolCall};
use pi_core::error::AiError;
use pi_core::event::{
    AssistantMessageEvent, AssistantMessageEventSink, AssistantMessageEventStream, DoneReason,
    ErrorReason,
};
use pi_core::message::{
    now_ms, AssistantMessage, AssistantMessageDiagnostic, DiagnosticSeverity, StopReason, Usage,
};
use pi_core::model::{CacheRetention, Model};
use pi_core::options::{ProviderResponse, SimpleStreamOptions, StreamOptions};
use pi_core::tool::Context;
use pi_core::ThinkingLevel;
use pi_http::HttpClient;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::provider::{ProviderDescriptor, ProviderRegistration};
use crate::support::http_stream::{self, SseRequest, TransportFailure};
use pi_http::json_parse::parse_streaming_json_object as parse_streaming_json;

pub const PI_MESSAGES_API: &str = "pi-messages";

/// Provider-option keys understood by this adapter, read from
/// [`StreamOptions::provider_options`].
pub mod option_keys {
    /// `"auto" | "none" | "required" | {type:"function",function:{name}}`.
    pub const TOOL_CHOICE: &str = "toolChoice";
    /// Ask the backend for debug metadata (routing response headers).
    pub const DEBUG: &str = "debug";
    /// Thinking level, when calling `stream` rather than `stream_simple`.
    pub const REASONING: &str = "reasoning";
}

const MAX_DIAGNOSTIC_BODY_CHARS: usize = 8192;

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// Impact summary of a server-side message rewrite (a gateway policy, say).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiMessagesRewriteImpact {
    pub policy_id: String,
    pub policy_version: i64,
    pub changed: bool,
    pub token_count_change: i64,
    pub message_count_change: i64,
    pub system_prompt_changed: bool,
}

/// One serialized assistant-message event as sent by a pi-messages backend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PiMessagesEvent {
    Start,
    TextStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
    },
    TextDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
    },
    TextEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        content: String,
        #[serde(default, rename = "contentSignature")]
        content_signature: Option<String>,
    },
    ThinkingStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
    },
    ThinkingDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
    },
    ThinkingEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        content: String,
        #[serde(default, rename = "contentSignature")]
        content_signature: Option<String>,
        #[serde(default)]
        redacted: Option<bool>,
    },
    #[serde(rename = "toolcall_start")]
    ToolCallStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
    },
    #[serde(rename = "toolcall_delta")]
    ToolCallDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
    },
    #[serde(rename = "toolcall_end")]
    ToolCallEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        #[serde(rename = "toolCall")]
        tool_call: ToolCall,
    },
    Done {
        reason: PiMessagesDoneReason,
        usage: Usage,
        #[serde(default, rename = "responseId")]
        response_id: Option<String>,
        #[serde(default)]
        rewrite: Option<PiMessagesRewriteImpact>,
    },
    Error {
        reason: PiMessagesErrorReason,
        #[serde(default)]
        usage: Usage,
        #[serde(default, rename = "errorMessage")]
        error_message: Option<String>,
        #[serde(default, rename = "responseId")]
        response_id: Option<String>,
        #[serde(default)]
        rewrite: Option<PiMessagesRewriteImpact>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PiMessagesDoneReason {
    Stop,
    Length,
    ToolUse,
}

impl From<PiMessagesDoneReason> for DoneReason {
    fn from(reason: PiMessagesDoneReason) -> Self {
        match reason {
            PiMessagesDoneReason::Stop => DoneReason::Stop,
            PiMessagesDoneReason::Length => DoneReason::Length,
            PiMessagesDoneReason::ToolUse => DoneReason::ToolUse,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PiMessagesErrorReason {
    Aborted,
    Error,
}

impl From<PiMessagesErrorReason> for ErrorReason {
    fn from(reason: PiMessagesErrorReason) -> Self {
        match reason {
            PiMessagesErrorReason::Aborted => ErrorReason::Aborted,
            PiMessagesErrorReason::Error => ErrorReason::Error,
        }
    }
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// `pi-messages` adapter.
#[derive(Clone)]
pub struct PiMessagesApi {
    http: Arc<HttpClient>,
}

impl std::fmt::Debug for PiMessagesApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PiMessagesApi")
    }
}

impl Default for PiMessagesApi {
    fn default() -> Self {
        Self::new()
    }
}

impl PiMessagesApi {
    pub fn new() -> Self {
        Self {
            http: HttpClient::shared(),
        }
    }

    pub fn with_http_client(http: Arc<HttpClient>) -> Self {
        Self { http }
    }

    pub fn client(&self) -> ApiClientRef {
        Arc::new(self.clone())
    }

    fn start(
        &self,
        model: &Model,
        context: &Context,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        let (sink, stream) = AssistantMessageEventStream::channel(16);
        let http = self.http.clone();
        let model = model.clone();
        let context = context.clone();
        tokio::spawn(async move { run(http, sink, model, context, options).await });
        stream
    }
}

#[async_trait]
impl ApiClient for PiMessagesApi {
    fn api(&self) -> &str {
        PI_MESSAGES_API
    }

    async fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: &StreamOptions,
    ) -> Result<AssistantMessageEventStream, AiError> {
        Ok(self.start(model, context, options.clone()))
    }

    async fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, AiError> {
        let mut stream_options = options.stream.clone();
        if let Some(reasoning) = options.reasoning {
            stream_options
                .provider_options
                .insert(option_keys::REASONING.to_string(), json!(reasoning));
        }
        Ok(self.start(model, context, stream_options))
    }
}

/// Descriptor for a gateway speaking `pi-messages` (Radius by default).
pub fn pi_messages_provider(
    id: impl Into<String>,
    name: impl Into<String>,
    base_url: impl Into<String>,
) -> ProviderRegistration {
    let name = name.into();
    ProviderRegistration {
        descriptor: ProviderDescriptor::new(id, name.clone(), PI_MESSAGES_API)
            .base_url(base_url)
            .api_key(format!("{name} API key"), &[]),
        client: PiMessagesApi::new().client(),
    }
}

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

fn build_url(model: &Model, debug: bool) -> String {
    let base = model.base_url.trim_end_matches('/');
    if debug {
        format!("{base}/messages?debug=1")
    } else {
        format!("{base}/messages")
    }
}

/// `cacheRetention` falls back to the legacy `PI_CACHE_RETENTION=long` opt-in;
/// otherwise the backend default applies.
fn resolve_cache_retention(options: &StreamOptions) -> Option<CacheRetention> {
    if let Some(retention) = options.cache_retention {
        return Some(retention);
    }
    let env = options
        .request
        .env
        .get("PI_CACHE_RETENTION")
        .cloned()
        .or_else(|| std::env::var("PI_CACHE_RETENTION").ok());
    match env.as_deref() {
        Some("long") => Some(CacheRetention::Long),
        _ => None,
    }
}

fn build_payload(model: &Model, context: &Context, options: &StreamOptions) -> Value {
    // Only keys with a value are sent: upstream relies on `JSON.stringify`
    // dropping `undefined`, and the backend distinguishes absent from null.
    let mut request_options = Map::new();
    if let Some(temperature) = options.temperature {
        request_options.insert("temperature".into(), json!(temperature));
    }
    if let Some(max_tokens) = options.max_tokens {
        request_options.insert("maxTokens".into(), json!(max_tokens));
    }
    if let Some(reasoning) = options.provider_option::<ThinkingLevel>(option_keys::REASONING) {
        request_options.insert("reasoning".into(), json!(reasoning));
    }
    if let Some(retention) = resolve_cache_retention(options) {
        request_options.insert("cacheRetention".into(), json!(retention));
    }
    if let Some(session_id) = &options.session_id {
        request_options.insert("sessionId".into(), json!(session_id));
    }
    if let Some(tool_choice) = options.provider_options.get(option_keys::TOOL_CHOICE) {
        request_options.insert("toolChoice".into(), tool_choice.clone());
    }

    json!({
        "model": model.id,
        "context": context,
        "options": Value::Object(request_options),
    })
}

fn build_headers(
    api_key: &str,
    options: &StreamOptions,
) -> std::collections::BTreeMap<String, String> {
    let defaults: std::collections::BTreeMap<String, String> = [
        ("authorization".to_string(), format!("Bearer {api_key}")),
        ("accept".to_string(), "text/event-stream".to_string()),
        ("content-type".to_string(), "application/json".to_string()),
    ]
    .into_iter()
    .collect();
    pi_http::merge_headers(defaults, &options.request.headers)
}

// ---------------------------------------------------------------------------
// Response
// ---------------------------------------------------------------------------

/// A non-2xx response, carrying the details upstream puts in the diagnostic.
struct PiMessagesResponseError {
    message: String,
    details: Value,
}

fn response_error(model: &Model, url: &str, failure: &TransportFailure) -> PiMessagesResponseError {
    let TransportFailure::Status {
        status,
        status_text,
        body,
        ..
    } = failure
    else {
        return PiMessagesResponseError {
            message: failure.message(),
            details: Value::Null,
        };
    };

    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let error_object = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .filter(|error| error.is_object());
    let message = error_object
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str);
    let code = error_object
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str);

    let suffix = message.unwrap_or(body.as_str());
    let code_suffix = code.map(|code| format!(" ({code})")).unwrap_or_default();

    let mut details = Map::new();
    details.insert("version".into(), json!(1));
    details.insert("provider".into(), json!(model.provider));
    details.insert("model".into(), json!(model.id));
    details.insert("url".into(), json!(url));
    details.insert("status".into(), json!(status));
    details.insert("statusText".into(), json!(status_text));
    match error_object {
        Some(error) => {
            details.insert("error".into(), error.clone());
        }
        None => {
            details.insert("body".into(), json!(truncate(body)));
        }
    }
    details.insert("timestampMs".into(), json!(now_ms()));

    PiMessagesResponseError {
        message: format!("{status} {status_text}: {suffix}{code_suffix}"),
        details: Value::Object(details),
    }
}

fn truncate(text: &str) -> String {
    if text.chars().count() <= MAX_DIAGNOSTIC_BODY_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_DIAGNOSTIC_BODY_CHARS).collect();
    format!("{head}…")
}

/// Accumulates the assistant message as backend events arrive.
struct EventConverter {
    partial: AssistantMessage,
    tool_json: std::collections::HashMap<usize, String>,
}

impl EventConverter {
    fn new(model: &Model) -> Self {
        Self {
            partial: AssistantMessage::pending(model.api.as_str(), &model.provider, &model.id),
            tool_json: Default::default(),
        }
    }

    fn set_content(&mut self, index: usize, block: AssistantContent) {
        while self.partial.content.len() <= index {
            self.partial
                .content
                .push(AssistantContent::Text(TextContent::default()));
        }
        self.partial.content[index] = block;
    }

    fn convert(&mut self, event: PiMessagesEvent) -> AssistantMessageEvent {
        match event {
            PiMessagesEvent::Start => AssistantMessageEvent::Start {
                partial: self.partial.clone(),
            },
            PiMessagesEvent::TextStart { content_index } => {
                self.set_content(
                    content_index,
                    AssistantContent::Text(TextContent::default()),
                );
                AssistantMessageEvent::TextStart {
                    content_index,
                    partial: self.partial.clone(),
                }
            }
            PiMessagesEvent::TextDelta {
                content_index,
                delta,
            } => {
                if let Some(AssistantContent::Text(text)) =
                    self.partial.content.get_mut(content_index)
                {
                    text.text.push_str(&delta);
                }
                AssistantMessageEvent::TextDelta {
                    content_index,
                    delta,
                    partial: self.partial.clone(),
                }
            }
            PiMessagesEvent::TextEnd {
                content_index,
                content,
                content_signature,
            } => {
                self.set_content(
                    content_index,
                    AssistantContent::Text(TextContent {
                        text: content.clone(),
                        text_signature: content_signature,
                    }),
                );
                AssistantMessageEvent::TextEnd {
                    content_index,
                    content,
                    partial: self.partial.clone(),
                }
            }
            PiMessagesEvent::ThinkingStart { content_index } => {
                self.set_content(
                    content_index,
                    AssistantContent::Thinking(ThinkingContent::default()),
                );
                AssistantMessageEvent::ThinkingStart {
                    content_index,
                    partial: self.partial.clone(),
                }
            }
            PiMessagesEvent::ThinkingDelta {
                content_index,
                delta,
            } => {
                if let Some(AssistantContent::Thinking(thinking)) =
                    self.partial.content.get_mut(content_index)
                {
                    thinking.thinking.push_str(&delta);
                }
                AssistantMessageEvent::ThinkingDelta {
                    content_index,
                    delta,
                    partial: self.partial.clone(),
                }
            }
            PiMessagesEvent::ThinkingEnd {
                content_index,
                content,
                content_signature,
                redacted,
            } => {
                self.set_content(
                    content_index,
                    AssistantContent::Thinking(ThinkingContent {
                        thinking: content.clone(),
                        thinking_signature: content_signature,
                        redacted: redacted.unwrap_or(false),
                    }),
                );
                AssistantMessageEvent::ThinkingEnd {
                    content_index,
                    content,
                    partial: self.partial.clone(),
                }
            }
            PiMessagesEvent::ToolCallStart {
                content_index,
                id,
                tool_name,
            } => {
                self.set_content(
                    content_index,
                    AssistantContent::ToolCall(ToolCall::new(id, tool_name)),
                );
                self.tool_json.insert(content_index, String::new());
                AssistantMessageEvent::ToolCallStart {
                    content_index,
                    partial: self.partial.clone(),
                }
            }
            PiMessagesEvent::ToolCallDelta {
                content_index,
                delta,
            } => {
                let json = self.tool_json.entry(content_index).or_default();
                json.push_str(&delta);
                let arguments = parse_streaming_json(Some(json));
                if let Some(AssistantContent::ToolCall(call)) =
                    self.partial.content.get_mut(content_index)
                {
                    call.arguments = arguments;
                }
                AssistantMessageEvent::ToolCallDelta {
                    content_index,
                    delta,
                    partial: self.partial.clone(),
                }
            }
            PiMessagesEvent::ToolCallEnd {
                content_index,
                tool_call,
            } => {
                // `Object.assign` upstream: the incoming call wins field by
                // field, anything it omits survives.
                let merged = match self.partial.content.get(content_index) {
                    Some(AssistantContent::ToolCall(existing)) => ToolCall {
                        id: tool_call.id,
                        name: tool_call.name,
                        arguments: tool_call.arguments,
                        thought_signature: tool_call
                            .thought_signature
                            .or_else(|| existing.thought_signature.clone()),
                        namespace: tool_call.namespace.or_else(|| existing.namespace.clone()),
                    },
                    _ => tool_call,
                };
                self.set_content(content_index, AssistantContent::ToolCall(merged.clone()));
                self.tool_json.remove(&content_index);
                AssistantMessageEvent::ToolCallEnd {
                    content_index,
                    tool_call: merged,
                    partial: self.partial.clone(),
                }
            }
            PiMessagesEvent::Done {
                reason,
                usage,
                response_id,
                rewrite,
            } => {
                self.partial.stop_reason = match reason {
                    PiMessagesDoneReason::Stop => StopReason::Stop,
                    PiMessagesDoneReason::Length => StopReason::Length,
                    PiMessagesDoneReason::ToolUse => StopReason::ToolUse,
                };
                self.partial.usage = usage;
                self.partial.response_id = response_id;
                append_rewrite_diagnostic(&mut self.partial, rewrite);
                AssistantMessageEvent::Done {
                    reason: reason.into(),
                    message: self.partial.clone(),
                }
            }
            PiMessagesEvent::Error {
                reason,
                usage,
                error_message,
                response_id,
                rewrite,
            } => {
                self.partial.stop_reason = match reason {
                    PiMessagesErrorReason::Aborted => StopReason::Aborted,
                    PiMessagesErrorReason::Error => StopReason::Error,
                };
                self.partial.usage = usage;
                self.partial.error_message = error_message;
                self.partial.response_id = response_id;
                append_rewrite_diagnostic(&mut self.partial, rewrite);
                AssistantMessageEvent::Error {
                    reason: reason.into(),
                    error: self.partial.clone(),
                }
            }
        }
    }
}

fn append_rewrite_diagnostic(
    message: &mut AssistantMessage,
    rewrite: Option<PiMessagesRewriteImpact>,
) {
    let Some(rewrite) = rewrite else {
        return;
    };
    message.push_diagnostic(AssistantMessageDiagnostic {
        code: "pi_messages_rewrite".to_string(),
        message: format!("request rewritten by policy {}", rewrite.policy_id),
        severity: Some(DiagnosticSeverity::Info),
        detail: serde_json::to_value(&rewrite).ok(),
        timestamp: Some(now_ms()),
    });
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

async fn run(
    http: Arc<HttpClient>,
    sink: AssistantMessageEventSink,
    model: Model,
    context: Context,
    options: StreamOptions,
) {
    let mut converter = EventConverter::new(&model);

    if let Err(error) = drive(&http, &sink, &model, &context, &options, &mut converter).await {
        let aborted = options.request.is_aborted() || error.aborted;
        let reason = if aborted {
            ErrorReason::Aborted
        } else {
            ErrorReason::Error
        };
        let mut message = AssistantMessage::pending(model.api.as_str(), &model.provider, &model.id);
        message.stop_reason = reason.into();
        message.error_message = Some(error.message.clone());
        if !aborted {
            if let Some(details) = error.details {
                message.push_diagnostic(AssistantMessageDiagnostic {
                    code: "pi_messages_response_failure".to_string(),
                    message: error.message,
                    severity: Some(DiagnosticSeverity::Error),
                    detail: Some(details),
                    timestamp: Some(now_ms()),
                });
            }
        }
        sink.send(AssistantMessageEvent::Error {
            reason,
            error: message,
        })
        .await;
    }
}

struct StreamError {
    message: String,
    aborted: bool,
    details: Option<Value>,
}

impl StreamError {
    fn plain(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            aborted: false,
            details: None,
        }
    }
}

async fn drive(
    http: &HttpClient,
    sink: &AssistantMessageEventSink,
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    converter: &mut EventConverter,
) -> Result<(), StreamError> {
    let Some(api_key) = options.request.api_key.clone() else {
        return Err(StreamError::plain(format!(
            "No API key provided for provider \"{}\"",
            model.provider
        )));
    };

    let debug = options
        .provider_option::<bool>(option_keys::DEBUG)
        .unwrap_or(false);
    let url = build_url(model, debug);

    let mut payload = build_payload(model, context, options);
    if let Some(on_payload) = &options.request.on_payload {
        if let Some(replacement) = on_payload(&payload, model) {
            payload = replacement;
        }
    }

    let request = SseRequest {
        url: url.clone(),
        headers: build_headers(&api_key, options),
        body: payload,
        signal: options.request.signal.clone(),
        timeout: options.request.timeout_ms.map(Duration::from_millis),
    };

    let mut response = match http_stream::post_sse(http, request).await {
        Ok(response) => response,
        Err(failure) => {
            notify_response(options, model, &failure);
            if failure.is_aborted() {
                return Err(StreamError {
                    message: "Request was aborted".to_string(),
                    aborted: true,
                    details: None,
                });
            }
            let error = response_error(model, &url, &failure);
            return Err(StreamError {
                message: error.message,
                aborted: false,
                details: match error.details {
                    Value::Null => None,
                    details => Some(details),
                },
            });
        }
    };

    if let Some(on_response) = &options.request.on_response {
        on_response(
            &ProviderResponse {
                status: response.status,
                headers: response.headers.clone(),
            },
            model,
        );
    }

    while let Some(event) = response.next_event().await {
        let event = match event {
            Ok(event) => event,
            Err(failure) => {
                return Err(StreamError {
                    message: failure.message(),
                    aborted: failure.is_aborted(),
                    details: None,
                })
            }
        };
        if event.is_done_sentinel() || event.data.trim().is_empty() {
            continue;
        }
        let parsed: PiMessagesEvent = event
            .json()
            .map_err(|error| StreamError::plain(format!("invalid pi-messages event: {error}")))?;
        let converted = converter.convert(parsed);
        let terminal = converted.is_terminal();
        sink.send(converted).await;
        if terminal {
            return Ok(());
        }
    }

    Err(StreamError::plain(format!(
        "{} stream ended without a terminal event",
        model.provider
    )))
}

/// Upstream reports the response to `onResponse` even for non-2xx statuses.
fn notify_response(options: &StreamOptions, model: &Model, failure: &TransportFailure) {
    let Some(on_response) = &options.request.on_response else {
        return;
    };
    if let TransportFailure::Status {
        status, headers, ..
    } = failure
    {
        on_response(
            &ProviderResponse {
                status: *status,
                headers: headers.clone(),
            },
            model,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::model::Api;

    fn model() -> Model {
        Model::new("auto", Api::PiMessages, "radius", "http://127.0.0.1:1/v1")
    }

    #[test]
    fn builds_the_messages_url() {
        assert_eq!(build_url(&model(), false), "http://127.0.0.1:1/v1/messages");
        assert_eq!(
            build_url(&model(), true),
            "http://127.0.0.1:1/v1/messages?debug=1"
        );
        let mut trailing = model();
        trailing.base_url = "http://host/v1///".into();
        assert_eq!(build_url(&trailing, false), "http://host/v1/messages");
    }

    #[test]
    fn omits_absent_options_from_the_payload() {
        let mut options = StreamOptions {
            max_tokens: Some(100),
            session_id: Some("session-1".into()),
            ..Default::default()
        };
        options
            .provider_options
            .insert(option_keys::TOOL_CHOICE.into(), json!("auto"));

        let payload = build_payload(&model(), &Context::default(), &options);
        assert_eq!(
            payload["options"],
            json!({ "maxTokens": 100, "sessionId": "session-1", "toolChoice": "auto" })
        );
        assert_eq!(payload["model"], "auto");
    }

    #[test]
    fn parses_backend_events() {
        let event: PiMessagesEvent = serde_json::from_str(
            r#"{"type":"toolcall_start","contentIndex":1,"id":"c","toolName":"read"}"#,
        )
        .unwrap();
        assert_eq!(
            event,
            PiMessagesEvent::ToolCallStart {
                content_index: 1,
                id: "c".into(),
                tool_name: "read".into()
            }
        );
    }

    #[test]
    fn formats_error_responses_with_code() {
        let failure = TransportFailure::Status {
            status: 401,
            status_text: "Unauthorized".into(),
            headers: Default::default(),
            body: r#"{"error":{"message":"Token expired","code":"unauthorized"}}"#.into(),
        };
        let error = response_error(&model(), "http://host/messages", &failure);
        assert_eq!(
            error.message,
            "401 Unauthorized: Token expired (unauthorized)"
        );
        assert_eq!(error.details["status"], 401);
    }

    #[test]
    fn falls_back_to_the_raw_body() {
        let failure = TransportFailure::Status {
            status: 500,
            status_text: "Internal Server Error".into(),
            headers: Default::default(),
            body: "boom".into(),
        };
        let error = response_error(&model(), "http://host/messages", &failure);
        assert_eq!(error.message, "500 Internal Server Error: boom");
        assert_eq!(error.details["body"], "boom");
    }
}
