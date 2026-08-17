//! The `anthropic-messages` adapter.
//!
//! Port of `packages/ai/src/api/anthropic-messages.ts`. Upstream drives the
//! official Anthropic SDK; this port speaks the same wire protocol directly over
//! [`pi_http`], because the SDK's client/beta-header plumbing is exactly the part
//! upstream overrides anyway.
//!
//! Contract reminder: request failures never come back as `Err` from
//! [`ApiClient::stream`]. They are encoded in the returned stream as an `Error`
//! event carrying an `AssistantMessage` with `stop_reason` `Error`/`Aborted`.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::Value;

use pi_core::api::ApiClient;
use pi_core::content::{AssistantContent, TextContent, ThinkingContent, ToolCall};
use pi_core::event::{
    AssistantMessageEvent, AssistantMessageEventSink, AssistantMessageEventStream,
};
use pi_core::message::{AssistantMessage, StopReason};
use pi_core::model::{Model, ThinkingLevel};
use pi_core::options::{ProviderHeaders, ProviderResponse, SimpleStreamOptions, StreamOptions};
use pi_core::tool::Context;
use pi_core::{AiError, DoneReason, ErrorReason};

use pi_http::client::{JsonRequest, SseResponse};
use pi_http::{retry_with_backoff, HttpClient, RetryPolicy};

use crate::options::{AnthropicEffort, AnthropicOptions};
use crate::request::{
    anthropic_compat, build_params, cache_control_for, from_claude_code_name, is_oauth_token,
    CLAUDE_CODE_VERSION,
};
use crate::ANTHROPIC_MESSAGES_API;
use pi_http::json_parse::{parse_json_with_repair, parse_streaming_json_object};
use pi_provider_common::cost::calculate_cost;
use pi_provider_common::simple_options::{
    adjust_max_tokens_for_thinking, build_base_options, clamp_max_tokens_to_context,
    MIN_ANSWER_TOKENS,
};

const ANTHROPIC_VERSION: &str = "2023-06-01";
const MESSAGE_EVENTS: [&str; 6] = [
    "message_start",
    "message_delta",
    "message_stop",
    "content_block_start",
    "content_block_delta",
    "content_block_stop",
];

/// `anthropic-messages` API adapter.
#[derive(Clone)]
pub struct AnthropicMessagesApi {
    http: Arc<HttpClient>,
}

impl Default for AnthropicMessagesApi {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for AnthropicMessagesApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AnthropicMessagesApi")
    }
}

impl AnthropicMessagesApi {
    pub fn new() -> Self {
        Self {
            http: HttpClient::shared(),
        }
    }

    /// Use a caller-provided HTTP client (proxy settings, timeouts, test doubles).
    pub fn with_http_client(http: Arc<HttpClient>) -> Self {
        Self { http }
    }
}

#[async_trait]
impl ApiClient for AnthropicMessagesApi {
    fn api(&self) -> &str {
        ANTHROPIC_MESSAGES_API
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
        // Upstream `streamSimple` asserts auth synchronously here and throws.
        // This port deliberately does not: `execute` re-checks the same
        // condition and encodes it as an in-stream `Error` event, and the
        // `ApiClient` contract in `pi-core` reserves `Err` for programmer
        // errors. Two failure paths for one condition is a trap for an FFI
        // caller, so a missing credential is a stream error here as it is in
        // `stream`. See the consolidation note in the crate docs.
        let mut stream_options = build_base_options(model, context, options, None);
        let mut anthropic = AnthropicOptions::from_stream_options(&options.stream);

        match options.reasoning {
            None => {
                anthropic.thinking_enabled = Some(false);
            }
            Some(reasoning) => {
                if anthropic_compat(model).force_adaptive_thinking {
                    // Adaptive thinking: Claude decides when and how much to think.
                    anthropic.thinking_enabled = Some(true);
                    anthropic.effort = Some(map_thinking_level_to_effort(model, reasoning));
                } else {
                    let adjusted = adjust_max_tokens_for_thinking(
                        stream_options.max_tokens,
                        model.max_tokens,
                        reasoning,
                        options.thinking_budgets.as_ref(),
                    );
                    let max_tokens =
                        clamp_max_tokens_to_context(model, context, adjusted.max_tokens);
                    stream_options.max_tokens = Some(max_tokens);
                    anthropic.thinking_enabled = Some(true);
                    anthropic.thinking_budget_tokens = Some(
                        adjusted
                            .thinking_budget
                            .min(max_tokens.saturating_sub(MIN_ANSWER_TOKENS)),
                    );
                }
            }
        }

        anthropic.apply(&mut stream_options);
        self.stream(model, context, &stream_options).await
    }
}

/// Map a unified thinking level onto an Anthropic effort value.
fn map_thinking_level_to_effort(model: &Model, level: ThinkingLevel) -> AnthropicEffort {
    let mapped = model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(&level.into()))
        .and_then(|value| value.as_deref())
        .and_then(AnthropicEffort::parse);
    if let Some(effort) = mapped {
        return effort;
    }
    match level {
        ThinkingLevel::Minimal | ThinkingLevel::Low => AnthropicEffort::Low,
        ThinkingLevel::Medium => AnthropicEffort::Medium,
        // `xhigh`/`max` need a model-specific mapping; otherwise they clamp to high.
        _ => AnthropicEffort::High,
    }
}

fn has_header(headers: &ProviderHeaders, name: &str) -> bool {
    headers.iter().any(|(key, value)| {
        key.eq_ignore_ascii_case(name) && value.as_ref().is_some_and(|v| !v.trim().is_empty())
    })
}

fn assert_request_auth(
    provider: &str,
    api_key: Option<&str>,
    headers: &ProviderHeaders,
) -> Result<(), AiError> {
    if api_key.is_some_and(|key| !key.is_empty()) {
        return Ok(());
    }
    if has_header(headers, "authorization")
        || has_header(headers, "x-api-key")
        || has_header(headers, "cf-aig-authorization")
    {
        return Ok(());
    }
    Err(AiError::auth(format!(
        "No API key for provider: {provider}"
    )))
}

/// Everything the request needs that is not the JSON body.
fn build_headers(
    model: &Model,
    options: &StreamOptions,
    api_key: Option<&str>,
    is_oauth: bool,
    beta_features: &[String],
) -> BTreeMap<String, String> {
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    headers.insert("accept".into(), "application/json".into());
    headers.insert("anthropic-version".into(), ANTHROPIC_VERSION.into());
    headers.insert(
        "anthropic-dangerous-direct-browser-access".into(),
        "true".into(),
    );

    if model.provider == "github-copilot" {
        // Copilot uses bearer auth; its dynamic vision/session headers are not
        // ported (see the crate report).
        if let Some(key) = api_key {
            headers.insert("authorization".into(), format!("Bearer {key}"));
        }
        if !beta_features.is_empty() {
            headers.insert("anthropic-beta".into(), beta_features.join(","));
        }
    } else if is_oauth {
        let key = api_key.unwrap_or_default();
        headers.insert("authorization".into(), format!("Bearer {key}"));
        let mut betas = vec![
            "claude-code-20250219".to_string(),
            "oauth-2025-04-20".to_string(),
        ];
        betas.extend(beta_features.iter().cloned());
        headers.insert("anthropic-beta".into(), betas.join(","));
        headers.insert(
            "user-agent".into(),
            format!("claude-cli/{CLAUDE_CODE_VERSION}"),
        );
        headers.insert("x-app".into(), "cli".into());
    } else {
        if let Some(key) = api_key {
            headers.insert("x-api-key".into(), key.to_string());
        }
        if !beta_features.is_empty() {
            headers.insert("anthropic-beta".into(), beta_features.join(","));
        }
        let (retention, _) =
            cache_control_for(model, options.cache_retention, &options.request.env);
        if anthropic_compat(model).send_session_affinity_headers
            && retention != pi_core::CacheRetention::None
        {
            if let Some(session_id) = &options.session_id {
                headers.insert("x-session-affinity".into(), session_id.clone());
            }
        }
    }

    if let Some(model_headers) = &model.headers {
        for (name, value) in model_headers {
            headers.insert(name.clone(), value.clone());
        }
    }

    let mut headers = pi_http::merge_headers(headers, &options.request.headers);

    // kimi-coding rejects the Claude Code user agent.
    if model.provider == "kimi-coding" {
        headers.retain(|name, _| !name.eq_ignore_ascii_case("user-agent"));
        headers.insert("User-Agent".into(), pi_http::client::default_user_agent());
    }

    headers
}

fn messages_url(base_url: &str) -> String {
    format!("{}/v1/messages", base_url.trim_end_matches('/'))
}

/// Producer side of [`ApiClient::stream`].
async fn run_stream(
    http: Arc<HttpClient>,
    model: Model,
    context: Context,
    options: StreamOptions,
    sink: AssistantMessageEventSink,
) {
    let mut state = StreamState::new(&model);
    if let Err(message) = execute(&http, &model, &context, &options, &sink, &mut state).await {
        let reason = if options.request.is_aborted() {
            ErrorReason::Aborted
        } else {
            ErrorReason::Error
        };
        state.output.stop_reason = reason.into();
        state.output.error_message = Some(message);
        sink.send(AssistantMessageEvent::Error {
            reason,
            error: state.output.clone(),
        })
        .await;
    }
}

/// The happy path. Any `Err(message)` becomes the stream's `Error` event.
async fn execute(
    http: &HttpClient,
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    sink: &AssistantMessageEventSink,
    state: &mut StreamState,
) -> Result<(), String> {
    let anthropic = AnthropicOptions::from_stream_options(options);
    let api_key = options.request.api_key.clone();
    assert_request_auth(
        &model.provider,
        api_key.as_deref(),
        &options.request.headers,
    )
    .map_err(|err| err.message())?;
    let is_oauth = api_key.as_deref().is_some_and(is_oauth_token);

    let built =
        build_params(model, context, is_oauth, options, &anthropic).map_err(|err| err.message())?;
    let mut body = built.body;
    if let Some(on_payload) = &options.request.on_payload {
        if let Some(replacement) = on_payload(&body, model) {
            body = replacement;
        }
    }

    let headers = build_headers(
        model,
        options,
        api_key.as_deref(),
        is_oauth,
        &built.beta_features,
    );
    let request = JsonRequest::post(messages_url(&model.base_url), body)
        .signal(options.request.signal.clone())
        .timeout_ms(options.request.timeout_ms);
    let request = JsonRequest { headers, ..request };

    let policy = RetryPolicy {
        max_attempts: options.request.max_retries.unwrap_or(0) + 1,
        max_server_delay_ms: options.request.max_retry_delay_ms.unwrap_or(60_000),
        ..Default::default()
    };
    let signal = options.request.signal.clone();
    let response: SseResponse = retry_with_backoff(&policy, signal.as_ref(), |_| {
        let request = request.clone();
        async move { http.post_sse(request).await }
    })
    .await
    .map_err(|err| AiError::from(err).message())?;

    if let Some(on_response) = &options.request.on_response {
        on_response(
            &ProviderResponse {
                status: response.status,
                headers: response.headers.clone().into_iter().collect(),
            },
            model,
        );
    }

    sink.send(AssistantMessageEvent::Start {
        partial: state.output.clone(),
    })
    .await;

    let tools = context.tools().to_vec();
    let mut body = response.body;
    let abort = options.request.signal();
    let mut saw_message_start = false;
    let mut saw_message_stop = false;

    loop {
        let next = tokio::select! {
            biased;
            _ = abort.aborted() => return Err("Request was aborted".to_string()),
            next = body.next() => next,
        };
        let Some(event) = next else { break };
        let event = event.map_err(|err| AiError::from(err).message())?;

        if event.event == "error" {
            return Err(event.data);
        }
        if !MESSAGE_EVENTS.contains(&event.event.as_str()) {
            continue;
        }
        let parsed: Value = parse_json_with_repair(&event.data).map_err(|err| {
            format!(
                "Could not parse Anthropic SSE event {}: {}; data={}",
                event.event, err, event.data
            )
        })?;
        match parsed.get("type").and_then(Value::as_str) {
            Some("message_start") => saw_message_start = true,
            Some("message_stop") => saw_message_stop = true,
            _ => {}
        }
        state
            .handle_event(&parsed, model, is_oauth, &tools, sink)
            .await?;
    }

    if saw_message_start && !saw_message_stop {
        return Err("Anthropic stream ended before message_stop".to_string());
    }
    if options.request.is_aborted() {
        return Err("Request was aborted".to_string());
    }

    let done_reason = match state.output.stop_reason {
        StopReason::Pending => {
            return Err("Anthropic stream ended without a stop reason".to_string())
        }
        StopReason::Aborted | StopReason::Error => {
            return Err(state
                .output
                .error_message
                .clone()
                .unwrap_or_else(|| "An unknown error occurred".to_string()))
        }
        StopReason::Stop => DoneReason::Stop,
        StopReason::Length => DoneReason::Length,
        StopReason::ToolUse => DoneReason::ToolUse,
        StopReason::Deferred => DoneReason::Deferred,
    };

    sink.send(AssistantMessageEvent::Done {
        reason: done_reason,
        message: state.output.clone(),
    })
    .await;
    Ok(())
}

/// Per-content-block streaming scratch state.
///
/// Upstream hangs `index`/`partialJson` off the content blocks themselves and
/// deletes them before the block is published; the typed port keeps them in a
/// parallel vector instead.
struct BlockState {
    /// Anthropic's `content_block` index, cleared once the block is closed.
    source_index: Option<i64>,
    partial_json: String,
}

struct StreamState {
    output: AssistantMessage,
    blocks: Vec<BlockState>,
}

impl StreamState {
    fn new(model: &Model) -> Self {
        Self {
            output: AssistantMessage::pending(
                model.api.as_str(),
                model.provider.clone(),
                model.id.clone(),
            ),
            blocks: Vec::new(),
        }
    }

    fn find(&self, source_index: i64) -> Option<usize> {
        self.blocks
            .iter()
            .position(|block| block.source_index == Some(source_index))
    }

    fn push_block(&mut self, source_index: i64, content: AssistantContent) -> usize {
        self.output.content.push(content);
        self.blocks.push(BlockState {
            source_index: Some(source_index),
            partial_json: String::new(),
        });
        self.output.content.len() - 1
    }

    async fn handle_event(
        &mut self,
        event: &Value,
        model: &Model,
        is_oauth: bool,
        tools: &[pi_core::tool::Tool],
        sink: &AssistantMessageEventSink,
    ) -> Result<(), String> {
        match event.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                let message = event.get("message").cloned().unwrap_or(Value::Null);
                self.output.response_id = message
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let usage = message.get("usage").cloned().unwrap_or(Value::Null);
                // Capture the initial counts so an early abort still reports input tokens.
                self.output.usage.input = token_at(&usage, "input_tokens");
                self.output.usage.output = token_at(&usage, "output_tokens");
                self.output.usage.cache_read = token_at(&usage, "cache_read_input_tokens");
                self.output.usage.cache_write = token_at(&usage, "cache_creation_input_tokens");
                self.output.usage.cache_write_1h = Some(
                    usage
                        .get("cache_creation")
                        .map(|c| token_at(c, "ephemeral_1h_input_tokens"))
                        .unwrap_or(0),
                );
                self.recompute_usage(model);
            }
            Some("content_block_start") => {
                let index = i64_at(event, "index");
                let block = event.get("content_block").cloned().unwrap_or(Value::Null);
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let content_index = self.push_block(
                            index,
                            AssistantContent::Text(TextContent {
                                text: str_at(&block, "text"),
                                text_signature: None,
                            }),
                        );
                        sink.send(AssistantMessageEvent::TextStart {
                            content_index,
                            partial: self.output.clone(),
                        })
                        .await;
                    }
                    Some("thinking") => {
                        let content_index = self.push_block(
                            index,
                            AssistantContent::Thinking(ThinkingContent {
                                thinking: str_at(&block, "thinking"),
                                thinking_signature: Some(str_at(&block, "signature")),
                                redacted: false,
                            }),
                        );
                        sink.send(AssistantMessageEvent::ThinkingStart {
                            content_index,
                            partial: self.output.clone(),
                        })
                        .await;
                    }
                    Some("redacted_thinking") => {
                        let content_index = self.push_block(
                            index,
                            AssistantContent::Thinking(ThinkingContent {
                                thinking: "[Reasoning redacted]".to_string(),
                                thinking_signature: Some(str_at(&block, "data")),
                                redacted: true,
                            }),
                        );
                        sink.send(AssistantMessageEvent::ThinkingStart {
                            content_index,
                            partial: self.output.clone(),
                        })
                        .await;
                    }
                    Some("tool_use") => {
                        let name = str_at(&block, "name");
                        let content_index = self.push_block(
                            index,
                            AssistantContent::ToolCall(ToolCall {
                                id: str_at(&block, "id"),
                                name: if is_oauth {
                                    from_claude_code_name(&name, tools)
                                } else {
                                    name
                                },
                                arguments: block
                                    .get("input")
                                    .and_then(Value::as_object)
                                    .cloned()
                                    .unwrap_or_default(),
                                thought_signature: None,
                                namespace: None,
                            }),
                        );
                        sink.send(AssistantMessageEvent::ToolCallStart {
                            content_index,
                            partial: self.output.clone(),
                        })
                        .await;
                    }
                    _ => {}
                }
            }
            Some("content_block_delta") => {
                let index = i64_at(event, "index");
                let delta = event.get("delta").cloned().unwrap_or(Value::Null);
                let Some(content_index) = self.find(index) else {
                    return Ok(());
                };
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let text = str_at(&delta, "text");
                        if let Some(AssistantContent::Text(block)) =
                            self.output.content.get_mut(content_index)
                        {
                            block.text.push_str(&text);
                            sink.send(AssistantMessageEvent::TextDelta {
                                content_index,
                                delta: text,
                                partial: self.output.clone(),
                            })
                            .await;
                        }
                    }
                    Some("thinking_delta") => {
                        let text = str_at(&delta, "thinking");
                        if let Some(AssistantContent::Thinking(block)) =
                            self.output.content.get_mut(content_index)
                        {
                            block.thinking.push_str(&text);
                            sink.send(AssistantMessageEvent::ThinkingDelta {
                                content_index,
                                delta: text,
                                partial: self.output.clone(),
                            })
                            .await;
                        }
                    }
                    Some("input_json_delta") => {
                        let fragment = str_at(&delta, "partial_json");
                        let is_tool_call = matches!(
                            self.output.content.get(content_index),
                            Some(AssistantContent::ToolCall(_))
                        );
                        if is_tool_call {
                            self.blocks[content_index].partial_json.push_str(&fragment);
                            let arguments = parse_streaming_json_object(Some(
                                &self.blocks[content_index].partial_json,
                            ));
                            if let Some(AssistantContent::ToolCall(block)) =
                                self.output.content.get_mut(content_index)
                            {
                                block.arguments = arguments;
                            }
                            sink.send(AssistantMessageEvent::ToolCallDelta {
                                content_index,
                                delta: fragment,
                                partial: self.output.clone(),
                            })
                            .await;
                        }
                    }
                    Some("signature_delta") => {
                        let signature = str_at(&delta, "signature");
                        if let Some(AssistantContent::Thinking(block)) =
                            self.output.content.get_mut(content_index)
                        {
                            block
                                .thinking_signature
                                .get_or_insert_with(String::new)
                                .push_str(&signature);
                        }
                    }
                    _ => {}
                }
            }
            Some("content_block_stop") => {
                let index = i64_at(event, "index");
                let Some(content_index) = self.find(index) else {
                    return Ok(());
                };
                self.blocks[content_index].source_index = None;
                let partial_json = std::mem::take(&mut self.blocks[content_index].partial_json);
                match self.output.content.get_mut(content_index) {
                    Some(AssistantContent::Text(block)) => {
                        let content = block.text.clone();
                        sink.send(AssistantMessageEvent::TextEnd {
                            content_index,
                            content,
                            partial: self.output.clone(),
                        })
                        .await;
                    }
                    Some(AssistantContent::Thinking(block)) => {
                        let content = block.thinking.clone();
                        sink.send(AssistantMessageEvent::ThinkingEnd {
                            content_index,
                            content,
                            partial: self.output.clone(),
                        })
                        .await;
                    }
                    Some(AssistantContent::ToolCall(block)) => {
                        block.arguments = parse_streaming_json_object(Some(&partial_json));
                        let tool_call = block.clone();
                        sink.send(AssistantMessageEvent::ToolCallEnd {
                            content_index,
                            tool_call,
                            partial: self.output.clone(),
                        })
                        .await;
                    }
                    None => {}
                }
            }
            Some("message_delta") => {
                let delta = event.get("delta").cloned().unwrap_or(Value::Null);
                if let Some(raw) = delta.get("stop_reason").and_then(Value::as_str) {
                    self.output.raw_stop_reason = Some(raw.to_string());
                    let (stop_reason, error_message) =
                        map_stop_reason(raw, delta.get("stop_details"))?;
                    self.output.stop_reason = stop_reason;
                    if let Some(message) = error_message {
                        self.output.error_message = Some(message);
                    }
                }
                // Only overwrite usage fields the provider actually sent: proxies
                // routinely omit input_tokens here, and message_start already has it.
                if let Some(usage) = event.get("usage").filter(|u| !u.is_null()) {
                    if let Some(value) = opt_token_at(usage, "input_tokens") {
                        self.output.usage.input = value;
                    }
                    if let Some(value) = opt_token_at(usage, "output_tokens") {
                        self.output.usage.output = value;
                    }
                    if let Some(value) = opt_token_at(usage, "cache_read_input_tokens") {
                        self.output.usage.cache_read = value;
                    }
                    if let Some(value) = opt_token_at(usage, "cache_creation_input_tokens") {
                        self.output.usage.cache_write = value;
                    }
                    // Reasoning tokens are a subset of output_tokens.
                    if let Some(thinking) = usage
                        .get("output_tokens_details")
                        .and_then(|details| opt_token_at(details, "thinking_tokens"))
                    {
                        self.output.usage.reasoning = Some(thinking);
                    }
                }
                self.recompute_usage(model);
            }
            _ => {}
        }
        Ok(())
    }

    /// Anthropic does not report a total, so derive it and reprice.
    fn recompute_usage(&mut self, model: &Model) {
        let usage = &mut self.output.usage;
        usage.total_tokens = usage.input + usage.output + usage.cache_read + usage.cache_write;
        calculate_cost(model, usage);
    }
}

fn map_stop_reason(
    reason: &str,
    stop_details: Option<&Value>,
) -> Result<(StopReason, Option<String>), String> {
    Ok(match reason {
        "end_turn" => (StopReason::Stop, None),
        "max_tokens" => (StopReason::Length, None),
        "tool_use" => (StopReason::ToolUse, None),
        "refusal" => {
            let explanation = stop_details
                .and_then(|details| details.get("explanation"))
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| "The model refused to complete the request".to_string());
            (StopReason::Error, Some(explanation))
        }
        // Resubmitting is the caller's job; `stop` is good enough.
        "pause_turn" => (StopReason::Stop, None),
        // We never supply stop sequences, so this should not happen.
        "stop_sequence" => (StopReason::Stop, None),
        "sensitive" => (
            StopReason::Error,
            Some("Provider stopped with: sensitive".to_string()),
        ),
        other => return Err(format!("Unhandled stop reason: {other}")),
    })
}

fn token_at(value: &Value, key: &str) -> i64 {
    opt_token_at(value, key).unwrap_or(0)
}

/// Token counts are signed: corrective usage records carry negative deltas, so
/// a value below zero is legal wire data and must not be clamped away here.
fn opt_token_at(value: &Value, key: &str) -> Option<i64> {
    match value.get(key) {
        Some(Value::Number(number)) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|f| f as i64)),
        _ => None,
    }
}

fn i64_at(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(-1)
}

fn str_at(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::model::{Api, ModelThinkingLevel, ThinkingLevelMap};

    fn model() -> Model {
        Model::new(
            "claude-opus-4-8",
            Api::AnthropicMessages,
            "anthropic",
            "https://api.anthropic.com",
        )
    }

    #[test]
    fn builds_messages_url() {
        assert_eq!(
            messages_url("https://api.anthropic.com"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            messages_url("https://proxy.test/v1/"),
            "https://proxy.test/v1/v1/messages"
        );
    }

    #[test]
    fn effort_falls_back_to_high_without_a_mapping() {
        let model = model();
        assert_eq!(
            map_thinking_level_to_effort(&model, ThinkingLevel::Low),
            AnthropicEffort::Low
        );
        assert_eq!(
            map_thinking_level_to_effort(&model, ThinkingLevel::Xhigh),
            AnthropicEffort::High
        );
    }

    #[test]
    fn effort_uses_the_model_thinking_level_map() {
        let mut model = model();
        let mut map = ThinkingLevelMap::new();
        map.insert(ModelThinkingLevel::Xhigh, Some("xhigh".to_string()));
        model.thinking_level_map = Some(map);
        assert_eq!(
            map_thinking_level_to_effort(&model, ThinkingLevel::Xhigh),
            AnthropicEffort::Xhigh
        );
    }

    #[test]
    fn unknown_stop_reasons_error() {
        assert!(map_stop_reason("time_travel", None).is_err());
        assert_eq!(
            map_stop_reason("refusal", None).unwrap().1.as_deref(),
            Some("The model refused to complete the request")
        );
    }

    #[test]
    fn auth_accepts_header_owned_credentials() {
        let mut headers: ProviderHeaders = Default::default();
        assert!(assert_request_auth("anthropic", None, &headers).is_err());
        headers.insert("Authorization".into(), Some("Bearer x".into()));
        assert!(assert_request_auth("anthropic", None, &headers).is_ok());
    }
}
