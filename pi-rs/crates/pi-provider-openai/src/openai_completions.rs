//! Port of `api/openai-completions.ts` — the OpenAI Chat Completions adapter.
//!
//! This is the widest adapter in the tree: everything that speaks
//! "OpenAI-compatible" goes through it, so almost every branch is driven by
//! [`CompletionsCompat`] rather than by the provider id. See `compat.rs` for how
//! those flags are detected and overridden.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use pi_core::content::{AssistantContent, TextContent, ThinkingContent};
use pi_core::event::{AssistantMessageEventSink, DoneReason, ErrorReason};
use pi_core::message::{Message, UserContent};
use pi_core::model::{MaxTokensField, SessionAffinityFormat, ThinkingFormat, ThinkingLevel};
use pi_core::options::{ProviderHeaders, SimpleStreamOptions, StreamOptions};
use pi_core::{
    AiError, ApiClient, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream,
    CacheRetention, Context, InputContent, Model, StopReason, Tool, ToolCall, Usage,
};
use pi_http::json_parse::parse_streaming_json_object;
use pi_http::HttpClient;
use serde_json::{json, Map, Value};

use crate::compat::{completions_compat, resolve_cache_retention, CompletionsCompat};
use crate::github_copilot_headers::{build_copilot_dynamic_headers, has_copilot_vision_input};
use crate::openai_prompt_cache::clamp_openai_prompt_cache_key;
use crate::options::{
    provider_opt_str, provider_opt_value, thinking_budgets_from, ProviderOptionKey,
};
use crate::transport::{
    apply_on_payload, assign_all, assign_header, base_json_sse_headers, client_api_key,
    finalize_headers, json_request, model_headers, next_sse, post_sse_with_retry, SsePump,
};
use crate::util::{
    calculate_cost, force_pi_user_agent, format_provider_error, sanitize_surrogates, short_hash,
    MappedLevel,
};
use pi_provider_common::constrained_sampling::{
    append_grammar_tool_input_json_delta, create_grammar_tool_input_properties, grammar_tool_input,
    json_schema_tool_parameters, resolve_grammar_constrained_sampling,
    resolve_json_schema_strict_sampling, GrammarToolInputJsonBuffer, GrammarToolInputProperties,
};
use pi_provider_common::simple_options::{
    build_base_options, clamp_reasoning, thinking_budget_for, MIN_ANSWER_TOKENS,
};
use pi_provider_common::transform_messages::transform_messages;

pub const API: &str = "openai-completions";

/// The `openai-completions` [`ApiClient`].
#[derive(Clone)]
pub struct OpenAiCompletionsClient {
    http: Arc<HttpClient>,
}

impl Default for OpenAiCompletionsClient {
    fn default() -> Self {
        Self {
            http: HttpClient::shared(),
        }
    }
}

impl std::fmt::Debug for OpenAiCompletionsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OpenAiCompletionsClient")
    }
}

impl OpenAiCompletionsClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_http_client(http: Arc<HttpClient>) -> Self {
        Self { http }
    }
}

#[async_trait]
impl ApiClient for OpenAiCompletionsClient {
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
        if let Some(budgets) = &options.thinking_budgets {
            base.provider_options.insert(
                ProviderOptionKey::ThinkingBudgets.as_str().to_string(),
                serde_json::to_value(budgets).unwrap_or(Value::Null),
            );
        }
        // `toolChoice` passes straight through from the raw options bag.
        if let Some(tool_choice) = options
            .stream
            .provider_options
            .get(ProviderOptionKey::ToolChoice.as_str())
        {
            base.provider_options.insert(
                ProviderOptionKey::ToolChoice.as_str().to_string(),
                tool_choice.clone(),
            );
        }

        self.stream(model, context, &base).await
    }
}

// ============================================================================
// Streaming
// ============================================================================

/// Streaming scratch state for one tool-call block, kept beside
/// `output.content` because `AssistantContent` has nowhere to stash it.
#[derive(Debug, Default)]
struct ToolScratch {
    partial_args: Option<String>,
    custom_input: Option<(String, GrammarToolInputJsonBuffer)>,
    stream_index: Option<i64>,
}

struct StreamState {
    output: AssistantMessage,
    text_index: Option<usize>,
    thinking_index: Option<usize>,
    tool_by_stream_index: HashMap<i64, usize>,
    tool_by_id: HashMap<String, usize>,
    scratch: HashMap<usize, ToolScratch>,
    pending_reasoning_details: HashMap<String, String>,
    has_finish_reason: bool,
}

async fn run_stream(
    http: Arc<HttpClient>,
    model: Model,
    context: Context,
    options: StreamOptions,
    sink: AssistantMessageEventSink,
) {
    let mut state = StreamState {
        output: AssistantMessage::pending(API, &model.provider, &model.id),
        text_index: None,
        thinking_index: None,
        tool_by_stream_index: HashMap::new(),
        tool_by_id: HashMap::new(),
        scratch: HashMap::new(),
        pending_reasoning_details: HashMap::new(),
        has_finish_reason: false,
    };

    match stream_inner(&http, &model, &context, &options, &mut state, &sink).await {
        Ok(reason) => {
            state.output.stop_reason = reason.into();
            let _ = sink
                .send(AssistantMessageEvent::Done {
                    reason,
                    message: state.output.clone(),
                })
                .await;
        }
        Err(err) => {
            let aborted = options.request.is_aborted() || err.is_aborted();
            state.output.stop_reason = if aborted {
                StopReason::Aborted
            } else {
                StopReason::Error
            };
            state.output.error_message = Some(format_provider_error(&err, None));
            let _ = sink
                .send(AssistantMessageEvent::Error {
                    reason: if aborted {
                        ErrorReason::Aborted
                    } else {
                        ErrorReason::Error
                    },
                    error: state.output.clone(),
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
    state: &mut StreamState,
    sink: &AssistantMessageEventSink,
) -> Result<DoneReason, AiError> {
    let api_key = client_api_key(
        &model.provider,
        options.request.api_key.as_deref(),
        &options.request.headers,
    )?;
    let compat = completions_compat(model);
    let grammar_props = create_grammar_tool_input_properties(
        context.tools.as_deref(),
        compat.supports_openai_grammar_tools,
    )?;
    let cache_retention = resolve_cache_retention(options.cache_retention, &options.request.env);
    let cache_session_id = match cache_retention {
        CacheRetention::None => None,
        _ => options.session_id.clone(),
    };

    let headers = build_headers(
        model,
        context,
        &api_key,
        &options.request.headers,
        cache_session_id.as_deref(),
        &compat,
    );
    let body = build_completions_body(
        model,
        context,
        options,
        &compat,
        cache_retention,
        &grammar_props,
    )?;
    let body = apply_on_payload(body, model, &options.request);

    let url = crate::transport::join_url(&model.base_url, "chat/completions");
    let request = json_request(url, body, headers, &options.request);
    let mut response = post_sse_with_retry(http, request, model, &options.request).await?;

    let _ = sink
        .send(AssistantMessageEvent::Start {
            partial: state.output.clone(),
        })
        .await;

    loop {
        match next_sse(&mut response, &options.request.signal).await {
            SsePump::Event(event) => {
                if event.is_done_sentinel() {
                    break;
                }
                if event.data.trim().is_empty() {
                    continue;
                }
                let chunk: Value = match serde_json::from_str(&event.data) {
                    Ok(value) => value,
                    // The upstream SDK skips anything that is not an object.
                    Err(_) => continue,
                };
                if !chunk.is_object() {
                    continue;
                }
                handle_chunk(&chunk, model, state, &grammar_props, sink).await?;
            }
            SsePump::Done => break,
            SsePump::Aborted => return Err(AiError::Aborted),
            SsePump::Failed(err) => return Err(err),
        }
    }

    finish_blocks(state, sink).await?;

    if options.request.is_aborted() || state.output.stop_reason == StopReason::Aborted {
        return Err(AiError::Aborted);
    }
    if !state.has_finish_reason && !compat.supports_finish_reason {
        // Providers that never emit finish_reason: infer it from the content.
        state.output.stop_reason = if state.output.tool_calls().next().is_some() {
            StopReason::ToolUse
        } else {
            StopReason::Stop
        };
    }
    if state.output.stop_reason == StopReason::Error {
        return Err(AiError::other(
            state
                .output
                .error_message
                .clone()
                .unwrap_or_else(|| "Provider returned an error stop reason".to_string()),
        ));
    }
    if (compat.supports_finish_reason && !state.has_finish_reason)
        || state.output.stop_reason == StopReason::Pending
    {
        return Err(AiError::protocol("Stream ended without finish_reason"));
    }

    Ok(match state.output.stop_reason {
        StopReason::Length => DoneReason::Length,
        StopReason::ToolUse => DoneReason::ToolUse,
        _ => DoneReason::Stop,
    })
}

async fn handle_chunk(
    chunk: &Value,
    model: &Model,
    state: &mut StreamState,
    grammar_props: &GrammarToolInputProperties,
    sink: &AssistantMessageEventSink,
) -> Result<(), AiError> {
    if state.output.response_id.is_none() {
        if let Some(id) = chunk.get("id").and_then(Value::as_str) {
            if !id.is_empty() {
                state.output.response_id = Some(id.to_string());
            }
        }
    }
    if let Some(chunk_model) = chunk.get("model").and_then(Value::as_str) {
        if !chunk_model.is_empty()
            && chunk_model != model.id
            && state.output.response_model.is_none()
        {
            state.output.response_model = Some(chunk_model.to_string());
        }
    }
    if let Some(usage) = chunk.get("usage").filter(|u| !u.is_null()) {
        state.output.usage = parse_chunk_usage(usage, model);
    }

    let Some(choice) = chunk
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
    else {
        return Ok(());
    };

    // Moonshot returns usage on the choice rather than the chunk.
    if chunk.get("usage").filter(|u| !u.is_null()).is_none() {
        if let Some(usage) = choice.get("usage").filter(|u| !u.is_null()) {
            state.output.usage = parse_chunk_usage(usage, model);
        }
    }

    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
        state.output.raw_stop_reason = Some(reason.to_string());
        let (stop_reason, error_message) = map_stop_reason(reason);
        state.output.stop_reason = stop_reason;
        if let Some(message) = error_message {
            state.output.error_message = Some(message);
        }
        state.has_finish_reason = true;
    }

    let Some(delta) = choice.get("delta").filter(|d| d.is_object()) else {
        return Ok(());
    };

    if let Some(content) = delta.get("content").and_then(Value::as_str) {
        if !content.is_empty() {
            let index = ensure_text_block(state, sink).await;
            if let AssistantContent::Text(text) = &mut state.output.content[index] {
                text.text.push_str(content);
            }
            let _ = sink
                .send(AssistantMessageEvent::TextDelta {
                    content_index: index,
                    delta: content.to_string(),
                    partial: state.output.clone(),
                })
                .await;
        }
    }

    // llama.cpp uses `reasoning_content`; other OpenAI-compatible endpoints use
    // `reasoning` or `reasoning_text`. Take the first non-empty one, because
    // chutes.ai sends the same text in two of them.
    let reasoning_field = ["reasoning_content", "reasoning", "reasoning_text"]
        .into_iter()
        .find(|field| {
            delta
                .get(*field)
                .and_then(Value::as_str)
                .is_some_and(|v| !v.is_empty())
        });
    if let Some(field) = reasoning_field {
        let text = delta
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let signature = if model.provider == "opencode-go" && field == "reasoning" {
            "reasoning_content"
        } else {
            field
        };
        let index = ensure_thinking_block(state, signature, sink).await;
        if let AssistantContent::Thinking(thinking) = &mut state.output.content[index] {
            thinking.thinking.push_str(&text);
        }
        let _ = sink
            .send(AssistantMessageEvent::ThinkingDelta {
                content_index: index,
                delta: text,
                partial: state.output.clone(),
            })
            .await;
    }

    if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
        for tool_call in tool_calls {
            let index = ensure_tool_call_block(state, tool_call, grammar_props, sink).await?;

            if let Some(id) = tool_call
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                if let AssistantContent::ToolCall(block) = &mut state.output.content[index] {
                    if block.id.is_empty() {
                        block.id = id.to_string();
                    }
                }
                state.tool_by_id.insert(id.to_string(), index);
            }
            let name = tool_call
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .or_else(|| {
                    tool_call
                        .get("custom")
                        .and_then(|c| c.get("name"))
                        .and_then(Value::as_str)
                });
            if let Some(name) = name.filter(|n| !n.is_empty()) {
                if let AssistantContent::ToolCall(block) = &mut state.output.content[index] {
                    if block.name.is_empty() {
                        block.name = name.to_string();
                    }
                }
            }

            let function_arguments = tool_call
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            let custom_input = tool_call
                .get("custom")
                .and_then(|c| c.get("input"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());

            let delta_text = if let Some(arguments) = function_arguments {
                let scratch = state.scratch.entry(index).or_default();
                let partial = scratch.partial_args.get_or_insert_with(String::new);
                partial.push_str(arguments);
                let parsed = parse_streaming_json_object(Some(partial.as_str()));
                if let AssistantContent::ToolCall(block) = &mut state.output.content[index] {
                    block.arguments = parsed;
                }
                arguments.to_string()
            } else if let Some(input) = custom_input {
                let next_input = format!("{}{}", custom_tool_call_input(state, index), input);
                append_custom_tool_call_input(state, index, &next_input, false)?.unwrap_or_default()
            } else {
                String::new()
            };

            let _ = sink
                .send(AssistantMessageEvent::ToolCallDelta {
                    content_index: index,
                    delta: delta_text,
                    partial: state.output.clone(),
                })
                .await;
        }
    }

    // OpenRouter attaches encrypted reasoning to the tool call it belongs to.
    if let Some(details) = delta.get("reasoning_details").and_then(Value::as_array) {
        for detail in details {
            let Some(id) = encrypted_reasoning_detail_id(detail) else {
                continue;
            };
            let serialized = detail.to_string();
            match state.tool_by_id.get(&id).copied() {
                Some(index) => {
                    if let AssistantContent::ToolCall(block) = &mut state.output.content[index] {
                        block.thought_signature = Some(serialized);
                    }
                }
                None => {
                    state.pending_reasoning_details.insert(id, serialized);
                }
            }
        }
    }

    Ok(())
}

fn encrypted_reasoning_detail_id(detail: &Value) -> Option<String> {
    let obj = detail.as_object()?;
    if obj.get("type").and_then(Value::as_str) != Some("reasoning.encrypted") {
        return None;
    }
    let id = obj
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    obj.get("data")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    Some(id.to_string())
}

async fn ensure_text_block(state: &mut StreamState, sink: &AssistantMessageEventSink) -> usize {
    if let Some(index) = state.text_index {
        return index;
    }
    state
        .output
        .content
        .push(AssistantContent::Text(TextContent::default()));
    let index = state.output.content.len() - 1;
    state.text_index = Some(index);
    let _ = sink
        .send(AssistantMessageEvent::TextStart {
            content_index: index,
            partial: state.output.clone(),
        })
        .await;
    index
}

async fn ensure_thinking_block(
    state: &mut StreamState,
    signature: &str,
    sink: &AssistantMessageEventSink,
) -> usize {
    if let Some(index) = state.thinking_index {
        return index;
    }
    state
        .output
        .content
        .push(AssistantContent::Thinking(ThinkingContent {
            thinking: String::new(),
            thinking_signature: Some(signature.to_string()),
            redacted: false,
        }));
    let index = state.output.content.len() - 1;
    state.thinking_index = Some(index);
    let _ = sink
        .send(AssistantMessageEvent::ThinkingStart {
            content_index: index,
            partial: state.output.clone(),
        })
        .await;
    index
}

fn custom_tool_call_input(state: &StreamState, index: usize) -> String {
    let Some(scratch) = state.scratch.get(&index) else {
        return String::new();
    };
    let Some((property, _)) = &scratch.custom_input else {
        return String::new();
    };
    match &state.output.content[index] {
        AssistantContent::ToolCall(block) => block
            .arguments
            .get(property)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

fn append_custom_tool_call_input(
    state: &mut StreamState,
    index: usize,
    next_input: &str,
    close: bool,
) -> Result<Option<String>, AiError> {
    let Some(scratch) = state.scratch.get_mut(&index) else {
        return Ok(None);
    };
    let Some((property, buffer)) = &mut scratch.custom_input else {
        return Ok(None);
    };
    let property = property.clone();
    let delta = append_grammar_tool_input_json_delta(buffer, &property, next_input, close)?;
    if let AssistantContent::ToolCall(block) = &mut state.output.content[index] {
        let mut arguments = Map::new();
        arguments.insert(property, json!(next_input));
        block.arguments = arguments;
    }
    Ok(delta)
}

async fn ensure_tool_call_block(
    state: &mut StreamState,
    tool_call: &Value,
    grammar_props: &GrammarToolInputProperties,
    sink: &AssistantMessageEventSink,
) -> Result<usize, AiError> {
    let stream_index = tool_call.get("index").and_then(Value::as_i64);
    let name = tool_call
        .get("function")
        .and_then(|f| f.get("name"))
        .and_then(Value::as_str)
        .or_else(|| {
            tool_call
                .get("custom")
                .and_then(|c| c.get("name"))
                .and_then(Value::as_str)
        })
        .unwrap_or_default()
        .to_string();
    let id = tool_call
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let has_custom = tool_call.get("custom").is_some() && tool_call.get("function").is_none();

    let mut index = stream_index.and_then(|i| state.tool_by_stream_index.get(&i).copied());
    if index.is_none() {
        if let Some(id) = &id {
            index = state.tool_by_id.get(id).copied();
        }
    }

    let index = match index {
        Some(index) => index,
        None => {
            // The `"input"` fallback is defensive: it only fires when the model
            // invents a custom tool we never declared, and gives its text
            // somewhere to land.
            let custom_property = has_custom.then(|| {
                grammar_props
                    .get(&name)
                    .cloned()
                    .unwrap_or_else(|| "input".into())
            });
            let mut arguments = Map::new();
            if let Some(property) = &custom_property {
                arguments.insert(property.clone(), json!(""));
            }
            state
                .output
                .content
                .push(AssistantContent::ToolCall(ToolCall {
                    id: id.clone().unwrap_or_default(),
                    name: name.clone(),
                    arguments,
                    thought_signature: None,
                    namespace: None,
                }));
            let index = state.output.content.len() - 1;
            state.scratch.insert(
                index,
                ToolScratch {
                    partial_args: if custom_property.is_some() {
                        None
                    } else {
                        Some(String::new())
                    },
                    custom_input: custom_property
                        .map(|property| (property, GrammarToolInputJsonBuffer::default())),
                    stream_index,
                },
            );
            if let Some(i) = stream_index {
                state.tool_by_stream_index.insert(i, index);
            }
            if let Some(id) = &id {
                state.tool_by_id.insert(id.clone(), index);
            }
            let _ = sink
                .send(AssistantMessageEvent::ToolCallStart {
                    content_index: index,
                    partial: state.output.clone(),
                })
                .await;
            index
        }
    };

    if let Some(i) = stream_index {
        let scratch = state.scratch.entry(index).or_default();
        if scratch.stream_index.is_none() {
            scratch.stream_index = Some(i);
            state.tool_by_stream_index.insert(i, index);
        }
    }
    if let Some(id) = &id {
        state.tool_by_id.insert(id.clone(), index);
    }
    if let AssistantContent::ToolCall(block) = &mut state.output.content[index] {
        if block.name.is_empty() && !name.is_empty() {
            block.name = name.clone();
        }
    }

    // A tool call that only later reveals itself as `custom` switches encoding.
    if has_custom {
        let needs_custom = state
            .scratch
            .get(&index)
            .is_none_or(|s| s.custom_input.is_none());
        if needs_custom {
            let block_name = match &state.output.content[index] {
                AssistantContent::ToolCall(block) => block.name.clone(),
                _ => name.clone(),
            };
            let property = grammar_props
                .get(&block_name)
                .cloned()
                .unwrap_or_else(|| "input".into());
            if let AssistantContent::ToolCall(block) = &mut state.output.content[index] {
                let mut arguments = Map::new();
                arguments.insert(property.clone(), json!(""));
                block.arguments = arguments;
            }
            let scratch = state.scratch.entry(index).or_default();
            scratch.custom_input = Some((property, GrammarToolInputJsonBuffer::default()));
            scratch.partial_args = None;
        }
    }

    apply_pending_reasoning_detail(state, index);
    Ok(index)
}

fn apply_pending_reasoning_detail(state: &mut StreamState, index: usize) {
    let id = match &state.output.content[index] {
        AssistantContent::ToolCall(block) if !block.id.is_empty() => block.id.clone(),
        _ => return,
    };
    if let Some(detail) = state.pending_reasoning_details.remove(&id) {
        if let AssistantContent::ToolCall(block) = &mut state.output.content[index] {
            block.thought_signature = Some(detail);
        }
    }
}

async fn finish_blocks(
    state: &mut StreamState,
    sink: &AssistantMessageEventSink,
) -> Result<(), AiError> {
    for index in 0..state.output.content.len() {
        match &state.output.content[index] {
            AssistantContent::Text(text) => {
                let content = text.text.clone();
                let _ = sink
                    .send(AssistantMessageEvent::TextEnd {
                        content_index: index,
                        content,
                        partial: state.output.clone(),
                    })
                    .await;
            }
            AssistantContent::Thinking(thinking) => {
                let content = thinking.thinking.clone();
                let _ = sink
                    .send(AssistantMessageEvent::ThinkingEnd {
                        content_index: index,
                        content,
                        partial: state.output.clone(),
                    })
                    .await;
            }
            AssistantContent::ToolCall(_) => {
                let has_custom = state
                    .scratch
                    .get(&index)
                    .is_some_and(|s| s.custom_input.is_some());
                if has_custom {
                    let current = custom_tool_call_input(state, index);
                    if let Some(delta) =
                        append_custom_tool_call_input(state, index, &current, true)?
                    {
                        let _ = sink
                            .send(AssistantMessageEvent::ToolCallDelta {
                                content_index: index,
                                delta,
                                partial: state.output.clone(),
                            })
                            .await;
                    }
                } else {
                    let partial = state
                        .scratch
                        .get(&index)
                        .and_then(|s| s.partial_args.clone());
                    let parsed = parse_streaming_json_object(partial.as_deref());
                    if let AssistantContent::ToolCall(block) = &mut state.output.content[index] {
                        block.arguments = parsed;
                    }
                }
                // Drop the scratch so a replayed message only carries parsed args.
                state.scratch.remove(&index);
                let tool_call = match &state.output.content[index] {
                    AssistantContent::ToolCall(block) => block.clone(),
                    _ => unreachable!("index was a tool call"),
                };
                let _ = sink
                    .send(AssistantMessageEvent::ToolCallEnd {
                        content_index: index,
                        tool_call,
                        partial: state.output.clone(),
                    })
                    .await;
            }
        }
    }
    Ok(())
}

/// Port of `parseChunkUsage`.
pub fn parse_chunk_usage(raw: &Value, model: &Model) -> Usage {
    // Token counts are signed: corrective usage records carry negative deltas.
    let num = |value: Option<&Value>| value.and_then(Value::as_i64).unwrap_or(0);
    let prompt_tokens = num(raw.get("prompt_tokens"));
    let details = raw.get("prompt_tokens_details");
    // Providers disagree on placement: OpenAI/OpenRouter use
    // `prompt_tokens_details.cached_tokens`, DeepSeek `prompt_cache_hit_tokens`,
    // Kimi a top-level `cached_tokens`. All of them mean cache *reads*; writes
    // are never subtracted from them or spec-compliant providers under-report.
    let cache_read = details
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_i64)
        .or_else(|| raw.get("prompt_cache_hit_tokens").and_then(Value::as_i64))
        .or_else(|| raw.get("cached_tokens").and_then(Value::as_i64))
        .unwrap_or(0);
    let cache_write = num(details.and_then(|d| d.get("cache_write_tokens")));

    // Upstream is `Math.max(0, promptTokens - cacheRead - cacheWrite)`: an
    // explicit clamp at zero, not signed saturation.
    let input = (prompt_tokens - cache_read - cache_write).max(0);
    // OpenAI's completion_tokens already includes reasoning_tokens.
    let output = num(raw.get("completion_tokens"));
    let reasoning = num(raw
        .get("completion_tokens_details")
        .and_then(|d| d.get("reasoning_tokens")));

    let mut usage = Usage {
        input,
        output,
        cache_read,
        cache_write,
        cache_write_1h: None,
        reasoning: Some(reasoning),
        total_tokens: input + output + cache_read + cache_write,
        cost: Default::default(),
    };
    calculate_cost(model, &mut usage);
    usage
}

/// Port of `mapStopReason`.
pub fn map_stop_reason(reason: &str) -> (StopReason, Option<String>) {
    match reason {
        "stop" | "end" => (StopReason::Stop, None),
        "length" => (StopReason::Length, None),
        "function_call" | "tool_calls" => (StopReason::ToolUse, None),
        "content_filter" => (
            StopReason::Error,
            Some("Provider finish_reason: content_filter".to_string()),
        ),
        "network_error" => (
            StopReason::Error,
            Some("Provider finish_reason: network_error".to_string()),
        ),
        other => (
            StopReason::Error,
            Some(format!("Provider finish_reason: {other}")),
        ),
    }
}

// ============================================================================
// Headers
// ============================================================================

fn build_headers(
    model: &Model,
    context: &Context,
    api_key: &str,
    option_headers: &ProviderHeaders,
    session_id: Option<&str>,
    compat: &CompletionsCompat,
) -> pi_http::HeaderMap {
    let mut headers = model_headers(model);

    if model.provider == "github-copilot" {
        let has_images = has_copilot_vision_input(&context.messages);
        for (name, value) in build_copilot_dynamic_headers(&context.messages, has_images) {
            assign_header(&mut headers, &name, Some(value));
        }
    }

    if let Some(session_id) = session_id.filter(|_| compat.send_session_affinity_headers) {
        match compat.session_affinity_format {
            SessionAffinityFormat::Openrouter => {
                assign_header(&mut headers, "x-session-id", Some(session_id.to_string()));
            }
            format => {
                if format == SessionAffinityFormat::Openai {
                    assign_header(&mut headers, "session_id", Some(session_id.to_string()));
                }
                assign_header(
                    &mut headers,
                    "x-client-request-id",
                    Some(session_id.to_string()),
                );
                assign_header(
                    &mut headers,
                    "x-session-affinity",
                    Some(session_id.to_string()),
                );
            }
        }
    }

    // Caller headers last so they override every default above.
    assign_all(&mut headers, option_headers);

    if model.provider == "xai" {
        force_pi_user_agent(&mut headers);
    }

    finalize_headers(base_json_sse_headers(api_key), &headers)
}

// ============================================================================
// Request building
// ============================================================================

/// Port of `buildParams`. Public so payload shape can be asserted directly.
pub fn build_completions_body(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    compat: &CompletionsCompat,
    cache_retention: CacheRetention,
    grammar_props: &GrammarToolInputProperties,
) -> Result<Value, AiError> {
    let mut messages = convert_completions_messages(model, context, compat, grammar_props)?;

    let reasoning_effort = provider_opt_str(options, ProviderOptionKey::ReasoningEffort)
        .and_then(|s| parse_thinking_level(&s));

    let mut params = Map::new();
    params.insert("model".into(), json!(model.id));
    // `messages` is inserted at the end so cache-control mutations land first;
    // the placeholder keeps upstream's key order.
    params.insert("messages".into(), Value::Null);
    params.insert("stream".into(), json!(true));

    let wants_cache_key = (model.base_url.contains("api.openai.com")
        && cache_retention != CacheRetention::None)
        || (cache_retention == CacheRetention::Long && compat.supports_long_cache_retention);
    if wants_cache_key {
        if let Some(key) = clamp_openai_prompt_cache_key(options.session_id.as_deref()) {
            params.insert("prompt_cache_key".into(), json!(key));
        }
    }
    if cache_retention == CacheRetention::Long && compat.supports_long_cache_retention {
        params.insert("prompt_cache_retention".into(), json!("24h"));
    }

    if compat.supports_usage_in_streaming {
        params.insert("stream_options".into(), json!({ "include_usage": true }));
    }
    if compat.supports_store {
        params.insert("store".into(), json!(false));
    }
    if let Some(max_tokens) = options.max_tokens.filter(|m| *m > 0) {
        match compat.max_tokens_field {
            MaxTokensField::MaxTokens => params.insert("max_tokens".into(), json!(max_tokens)),
            MaxTokensField::MaxCompletionTokens => {
                params.insert("max_completion_tokens".into(), json!(max_tokens))
            }
        };
    }
    if let Some(temperature) = options.temperature {
        params.insert("temperature".into(), json!(temperature));
    }

    // Kimi loads tools mid-transcript; once loaded they must not be re-declared.
    let deferred_tool_names: Vec<String> = if compat.deferred_tools_mode.as_deref() == Some("kimi")
    {
        deferred_tool_names(&context.messages)
    } else {
        Vec::new()
    };
    let active_tools: Vec<Tool> = context
        .tools()
        .iter()
        .filter(|tool| !deferred_tool_names.contains(&tool.name))
        .cloned()
        .collect();

    let mut tools: Option<Vec<Value>> = None;
    if !active_tools.is_empty() {
        tools = Some(convert_tools(&active_tools, compat)?);
        if compat.zai_tool_stream {
            params.insert("tool_stream".into(), json!(true));
        }
    } else if has_tool_history(&context.messages) {
        // Anthropic behind LiteLLM requires `tools` whenever the transcript has
        // tool calls or tool results, even when it is empty.
        tools = Some(Vec::new());
    }

    if let Some(cache_control) = compat_cache_control(compat, cache_retention) {
        apply_anthropic_cache_control(&mut messages, tools.as_mut(), &cache_control);
    }
    params.insert("messages".into(), Value::Array(messages));
    if let Some(tools) = tools {
        params.insert("tools".into(), Value::Array(tools));
    }

    if let Some(tool_choice) = provider_opt_value(options, ProviderOptionKey::ToolChoice) {
        params.insert("tool_choice".into(), tool_choice);
    }

    apply_thinking_params(&mut params, model, compat, reasoning_effort);

    // vLLM caps reasoning with a top-level thinking_token_budget, independent of
    // thinkingFormat: one server can host zai, qwen and chat-template models.
    // Reasoning and the answer share max_tokens there, so an uncapped reasoning
    // phase can eat the whole response and leave no answer and no tool call.
    if compat.supports_thinking_token_budget && model.reasoning {
        if let Some(effort) = reasoning_effort {
            let level = clamp_reasoning(Some(effort)).unwrap_or(ThinkingLevel::Medium);
            let budgets = thinking_budgets_from(options);
            let ceiling = options.max_tokens.unwrap_or(model.max_tokens);
            let budget = thinking_budget_for(level, budgets.as_ref())
                .min(ceiling.saturating_sub(MIN_ANSWER_TOKENS));
            if budget > 0 {
                params.insert("thinking_token_budget".into(), json!(budget));
            }
        }
    }

    if let Some(routing) = model
        .compat
        .as_ref()
        .and_then(|c| c.open_router_routing.clone())
    {
        params.insert("provider".into(), routing);
    }
    if let Some(routing) = model
        .compat
        .as_ref()
        .and_then(|c| c.vercel_gateway_routing.clone())
    {
        let only = routing.get("only").cloned();
        let order = routing.get("order").cloned();
        if only.is_some() || order.is_some() {
            let mut gateway = Map::new();
            if let Some(only) = only {
                gateway.insert("only".into(), only);
            }
            if let Some(order) = order {
                gateway.insert("order".into(), order);
            }
            params.insert("providerOptions".into(), json!({ "gateway": gateway }));
        }
    }

    // Last, so caller keys override the named request fields.
    if let Some(sampling) = &options.sampling_params {
        for (key, value) in sampling {
            params.insert(key.clone(), value.clone());
        }
    }

    Ok(Value::Object(params))
}

fn parse_thinking_level(value: &str) -> Option<ThinkingLevel> {
    Some(match value {
        "minimal" => ThinkingLevel::Minimal,
        "low" => ThinkingLevel::Low,
        "medium" => ThinkingLevel::Medium,
        "high" => ThinkingLevel::High,
        "xhigh" => ThinkingLevel::Xhigh,
        "max" => ThinkingLevel::Max,
        _ => return None,
    })
}

/// The eleven `thinkingFormat` dialects, in upstream's `else if` order.
fn apply_thinking_params(
    params: &mut Map<String, Value>,
    model: &Model,
    compat: &CompletionsCompat,
    effort: Option<ThinkingLevel>,
) {
    let mapped = |level: ThinkingLevel| MappedLevel::lookup(model, level.into());
    let off = || MappedLevel::lookup(model, pi_core::model::ModelThinkingLevel::Off);
    let effort_str = effort.map(|e| e.as_str().to_string());

    if !model.reasoning {
        // Only the plain OpenAI branch is reachable, and it too requires
        // `model.reasoning`; nothing to add.
        return;
    }

    match compat.thinking_format {
        ThinkingFormat::Zai => {
            params.insert(
                "thinking".into(),
                if effort.is_some() {
                    json!({ "type": "enabled", "clear_thinking": false })
                } else {
                    json!({ "type": "disabled" })
                },
            );
            if let Some(level) = effort.filter(|_| compat.supports_reasoning_effort) {
                if let Some(value) = mapped(level).value_or_requested(effort_str.as_deref()) {
                    params.insert("reasoning_effort".into(), json!(value));
                }
            }
        }
        ThinkingFormat::Qwen => {
            params.insert("enable_thinking".into(), json!(effort.is_some()));
            if let Some(level) = effort.filter(|_| compat.supports_reasoning_effort) {
                let value = mapped(level).or(effort_str.as_deref().unwrap_or_default());
                params.insert("reasoning_effort".into(), json!(value));
            }
        }
        ThinkingFormat::QwenChatTemplate => {
            params.insert(
                "chat_template_kwargs".into(),
                json!({ "enable_thinking": effort.is_some(), "preserve_thinking": true }),
            );
        }
        ThinkingFormat::ChatTemplate => {
            if let Some(values) =
                build_chat_template_values(model, effort, &compat.chat_template_kwargs)
            {
                params.insert("chat_template_kwargs".into(), Value::Object(values));
            }
        }
        ThinkingFormat::Baseten => {
            if let Some(values) =
                build_chat_template_values(model, effort, &compat.chat_template_args)
            {
                params.insert("chat_template_args".into(), Value::Object(values));
            }
            if compat.supports_reasoning_effort {
                let lookup = match effort {
                    Some(level) => mapped(level),
                    None => off(),
                };
                if let Some(value) = lookup.value_or_requested(effort_str.as_deref()) {
                    params.insert("reasoning_effort".into(), json!(value));
                }
            }
        }
        ThinkingFormat::Deepseek => {
            if effort.is_some() {
                params.insert("thinking".into(), json!({ "type": "enabled" }));
            } else if !off().is_null() {
                params.insert("thinking".into(), json!({ "type": "disabled" }));
            }
            if let Some(level) = effort.filter(|_| compat.supports_reasoning_effort) {
                let value = mapped(level).or(effort_str.as_deref().unwrap_or_default());
                params.insert("reasoning_effort".into(), json!(value));
            }
        }
        ThinkingFormat::Openrouter => {
            // OpenRouter normalizes reasoning across providers via a nested object.
            if let Some(level) = effort {
                let value = mapped(level).or(effort_str.as_deref().unwrap_or_default());
                params.insert("reasoning".into(), json!({ "effort": value }));
            } else if !off().is_null() {
                params.insert("reasoning".into(), json!({ "effort": off().or("none") }));
            }
        }
        ThinkingFormat::AntLing => {
            if let Some(level) = effort {
                if let MappedLevel::Value(value) = mapped(level) {
                    params.insert("reasoning".into(), json!({ "effort": value }));
                }
            }
        }
        ThinkingFormat::Together => {
            params.insert("reasoning".into(), json!({ "enabled": effort.is_some() }));
            if let Some(level) = effort.filter(|_| compat.supports_reasoning_effort) {
                let value = mapped(level).or(effort_str.as_deref().unwrap_or_default());
                params.insert("reasoning_effort".into(), json!(value));
            }
        }
        ThinkingFormat::StringThinking => {
            if let Some(level) = effort {
                let value = mapped(level).or(effort_str.as_deref().unwrap_or_default());
                params.insert("thinking".into(), json!(value));
            } else if !off().is_null() {
                params.insert("thinking".into(), json!(off().or("none")));
            }
        }
        ThinkingFormat::Openai => {
            if !compat.supports_reasoning_effort {
                return;
            }
            match effort {
                Some(level) => {
                    let value = mapped(level).or(effort_str.as_deref().unwrap_or_default());
                    params.insert("reasoning_effort".into(), json!(value));
                }
                None => {
                    if let MappedLevel::Value(value) = off() {
                        params.insert("reasoning_effort".into(), json!(value));
                    }
                }
            }
        }
    }
}

/// Port of `buildChatTemplateValues` / `resolveChatTemplateKwargValue`.
///
/// A value is either a literal, or a `$var` descriptor resolved against the
/// current reasoning effort: `thinking.enabled` becomes a boolean, anything else
/// resolves through `model.thinkingLevelMap`.
fn build_chat_template_values(
    model: &Model,
    effort: Option<ThinkingLevel>,
    values: &Map<String, Value>,
) -> Option<Map<String, Value>> {
    let mut resolved = Map::new();
    for (key, value) in values {
        if let Some(value) = resolve_chat_template_value(model, effort, value) {
            resolved.insert(key.clone(), value);
        }
    }
    (!resolved.is_empty()).then_some(resolved)
}

fn resolve_chat_template_value(
    model: &Model,
    effort: Option<ThinkingLevel>,
    value: &Value,
) -> Option<Value> {
    let Some(descriptor) = value.as_object() else {
        return Some(value.clone());
    };

    if effort.is_none()
        && descriptor
            .get("omitWhenOff")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return None;
    }
    if descriptor.get("$var").and_then(Value::as_str) == Some("thinking.enabled") {
        return Some(json!(effort.is_some()));
    }

    let lookup = match effort {
        Some(level) => MappedLevel::lookup(model, level.into()),
        None => MappedLevel::lookup(model, pi_core::model::ModelThinkingLevel::Off),
    };
    match lookup {
        MappedLevel::Value(value) => Some(json!(value)),
        MappedLevel::Missing => effort.map(|e| json!(e.as_str())),
        MappedLevel::Null => None,
    }
}

fn has_tool_history(messages: &[Message]) -> bool {
    messages.iter().any(|msg| match msg {
        Message::ToolResult(_) => true,
        Message::Assistant(assistant) => assistant.tool_calls().next().is_some(),
        Message::User(_) => false,
    })
}

fn deferred_tool_names(messages: &[Message]) -> Vec<String> {
    let mut names = Vec::new();
    for message in messages {
        if let Message::ToolResult(result) = message {
            for name in result.added_tool_names.clone().unwrap_or_default() {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    names
}

fn tools_by_name(tools: &[Tool], names: &[String]) -> Vec<Tool> {
    names
        .iter()
        .filter_map(|name| tools.iter().find(|t| &t.name == name).cloned())
        .collect()
}

// ---------------------------------------------------------------------------
// Anthropic-style cache_control (OpenRouter → Anthropic models)
// ---------------------------------------------------------------------------

fn compat_cache_control(compat: &CompletionsCompat, retention: CacheRetention) -> Option<Value> {
    if compat.cache_control_format.as_deref() != Some("anthropic")
        || retention == CacheRetention::None
    {
        return None;
    }
    let mut control = Map::new();
    control.insert("type".into(), json!("ephemeral"));
    if retention == CacheRetention::Long && compat.supports_long_cache_retention {
        control.insert("ttl".into(), json!("1h"));
    }
    Some(Value::Object(control))
}

fn apply_anthropic_cache_control(
    messages: &mut [Value],
    tools: Option<&mut Vec<Value>>,
    cache_control: &Value,
) {
    // System prompt.
    for message in messages.iter_mut() {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if role == "system" || role == "developer" {
            add_cache_control_to_text_content(message, cache_control);
            break;
        }
    }
    // Last tool.
    if let Some(tools) = tools {
        if let Some(last) = tools.last_mut() {
            if let Some(obj) = last.as_object_mut() {
                obj.insert("cache_control".into(), cache_control.clone());
            }
        }
    }
    // Last conversation message.
    for message in messages.iter_mut().rev() {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if matches!(role, "user" | "assistant" | "tool")
            && add_cache_control_to_text_content(message, cache_control)
        {
            break;
        }
    }
}

fn add_cache_control_to_text_content(message: &mut Value, cache_control: &Value) -> bool {
    let Some(obj) = message.as_object_mut() else {
        return false;
    };
    match obj.get("content").cloned() {
        Some(Value::String(text)) => {
            if text.is_empty() {
                return false;
            }
            obj.insert(
                "content".into(),
                json!([{ "type": "text", "text": text, "cache_control": cache_control }]),
            );
            true
        }
        Some(Value::Array(mut parts)) => {
            for part in parts.iter_mut().rev() {
                if part.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(part) = part.as_object_mut() {
                        part.insert("cache_control".into(), cache_control.clone());
                    }
                    obj.insert("content".into(), Value::Array(parts));
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Message conversion
// ---------------------------------------------------------------------------

fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Port of `convertMessages`.
pub fn convert_completions_messages(
    model: &Model,
    context: &Context,
    compat: &CompletionsCompat,
    grammar_props: &GrammarToolInputProperties,
) -> Result<Vec<Value>, AiError> {
    // Responses-style ids look like `{call_id}|{item_id}` and can run to 450+
    // characters with `+`, `/` and `=`. Chat Completions needs distinct ids of at
    // most 40 chars, and two calls in one turn can share a call_id, so the item
    // id has to survive in some form.
    let normalize_tool_call_id = |id: &str, _model: &Model, _source: &AssistantMessage| -> String {
        if let Some(separator) = id.find('|') {
            let call_id = sanitize_id(&id[..separator]);
            let item_id = sanitize_id(&id[separator + 1..]);
            let combined = if item_id.is_empty() {
                call_id.clone()
            } else {
                format!("{call_id}_{item_id}")
            };
            if combined.chars().count() <= 40 {
                return combined;
            }
            let hash: String = short_hash(id).chars().take(8).collect();
            let prefix_len = 40usize.saturating_sub(hash.chars().count() + 1).max(1);
            let prefix: String = call_id.chars().take(prefix_len).collect();
            return format!("{prefix}_{hash}");
        }
        if model.provider == "openai" && id.chars().count() > 40 {
            return id.chars().take(40).collect();
        }
        id.to_string()
    };

    let transformed = transform_messages(&context.messages, model, Some(&normalize_tool_call_id));

    let mut params: Vec<Value> = Vec::new();

    if let Some(system_prompt) = &context.system_prompt {
        let role = if model.reasoning && compat.supports_developer_role {
            "developer"
        } else {
            "system"
        };
        params.push(json!({ "role": role, "content": sanitize_surrogates(system_prompt) }));
    }

    let mut last_role: Option<&'static str> = None;
    let mut i = 0usize;
    while i < transformed.len() {
        let msg = &transformed[i];

        // Some providers reject a user message straight after a tool result;
        // bridge the gap with a synthetic assistant turn.
        if compat.requires_assistant_after_tool_result
            && last_role == Some("toolResult")
            && matches!(msg, Message::User(_))
        {
            params.push(json!({
                "role": "assistant",
                "content": "I have processed the tool results."
            }));
        }

        match msg {
            Message::User(user) => match &user.content {
                UserContent::Text(text) => {
                    params.push(json!({ "role": "user", "content": sanitize_surrogates(text) }));
                }
                UserContent::Blocks(blocks) => {
                    if blocks.is_empty() {
                        i += 1;
                        continue;
                    }
                    let content: Vec<Value> = blocks.iter().map(input_content_to_part).collect();
                    params.push(json!({ "role": "user", "content": content }));
                }
            },
            Message::Assistant(assistant) => {
                if let Some(message) =
                    convert_assistant_message(assistant, model, compat, grammar_props)?
                {
                    params.push(message);
                }
            }
            Message::ToolResult(_) => {
                let mut image_blocks: Vec<Value> = Vec::new();
                let mut deferred_names: Vec<String> = Vec::new();
                let mut j = i;

                while j < transformed.len() {
                    let Message::ToolResult(tool_msg) = &transformed[j] else {
                        break;
                    };
                    let text_result = tool_msg
                        .content
                        .iter()
                        .filter_map(|b| b.as_text().map(|t| t.text.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let has_images = tool_msg
                        .content
                        .iter()
                        .any(|b| matches!(b, InputContent::Image(_)));
                    let output = if !text_result.is_empty() {
                        text_result
                    } else if has_images {
                        "(see attached image)".to_string()
                    } else {
                        "(no tool output)".to_string()
                    };

                    let mut tool_message = Map::new();
                    tool_message.insert("role".into(), json!("tool"));
                    tool_message.insert("content".into(), json!(sanitize_surrogates(&output)));
                    tool_message.insert("tool_call_id".into(), json!(tool_msg.tool_call_id));
                    if compat.requires_tool_result_name && !tool_msg.tool_name.is_empty() {
                        tool_message.insert("name".into(), json!(tool_msg.tool_name));
                    }
                    params.push(Value::Object(tool_message));

                    if compat.deferred_tools_mode.as_deref() == Some("kimi") {
                        for name in tool_msg.added_tool_names.clone().unwrap_or_default() {
                            if !deferred_names.contains(&name) {
                                deferred_names.push(name);
                            }
                        }
                    }

                    if has_images && model.supports_images() {
                        for block in &tool_msg.content {
                            if let InputContent::Image(image) = block {
                                image_blocks.push(json!({
                                    "type": "image_url",
                                    "image_url": {
                                        "url": format!("data:{};base64,{}", image.mime_type, image.data)
                                    }
                                }));
                            }
                        }
                    }
                    j += 1;
                }

                i = j - 1;

                if !image_blocks.is_empty() {
                    if compat.requires_assistant_after_tool_result {
                        params.push(json!({
                            "role": "assistant",
                            "content": "I have processed the tool results."
                        }));
                    }
                    let mut content = vec![json!({
                        "type": "text",
                        "text": "Attached image(s) from tool result:"
                    })];
                    content.extend(image_blocks);
                    params.push(json!({ "role": "user", "content": content }));
                    last_role = Some("user");
                } else {
                    last_role = Some("toolResult");
                }

                if !deferred_names.is_empty() {
                    let deferred_tools = tools_by_name(context.tools(), &deferred_names);
                    if !deferred_tools.is_empty() {
                        // Kimi accepts a system message carrying tools and no content.
                        params.push(json!({
                            "role": "system",
                            "tools": convert_tools(&deferred_tools, compat)?
                        }));
                    }
                }
                i += 1;
                continue;
            }
        }

        last_role = Some(msg.role());
        i += 1;
    }

    Ok(params)
}

fn input_content_to_part(block: &InputContent) -> Value {
    match block {
        InputContent::Text(text) => {
            json!({ "type": "text", "text": sanitize_surrogates(&text.text) })
        }
        InputContent::Image(image) => json!({
            "type": "image_url",
            "image_url": { "url": format!("data:{};base64,{}", image.mime_type, image.data) }
        }),
    }
}

fn convert_assistant_message(
    assistant: &AssistantMessage,
    model: &Model,
    compat: &CompletionsCompat,
    grammar_props: &GrammarToolInputProperties,
) -> Result<Option<Value>, AiError> {
    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    // Some providers reject null content; use an empty string for those.
    message.insert(
        "content".into(),
        if compat.requires_assistant_after_tool_result {
            json!("")
        } else {
            Value::Null
        },
    );

    let text_parts: Vec<String> = assistant
        .content
        .iter()
        .filter_map(|b| b.as_text())
        .filter(|t| !t.text.trim().is_empty())
        .map(|t| sanitize_surrogates(&t.text).to_string())
        .collect();
    let assistant_text = text_parts.join("");

    let thinking_blocks: Vec<&ThinkingContent> = assistant
        .content
        .iter()
        .filter_map(|b| b.as_thinking())
        .filter(|t| !t.thinking.trim().is_empty())
        .collect();

    if !thinking_blocks.is_empty() {
        if compat.requires_thinking_as_text {
            // Plain text, no tags — tags make models mimic them in their output.
            let thinking_text = thinking_blocks
                .iter()
                .map(|b| sanitize_surrogates(&b.thinking))
                .collect::<Vec<_>>()
                .join("\n\n");
            let mut parts = vec![json!({ "type": "text", "text": thinking_text })];
            parts.extend(
                text_parts
                    .iter()
                    .map(|text| json!({ "type": "text", "text": text })),
            );
            message.insert("content".into(), Value::Array(parts));
        } else {
            // Assistant content always goes out as a plain string. Sending an
            // array of text parts is non-standard here and makes some models
            // (DeepSeek V3.2 via NVIDIA NIM) echo the block structure literally.
            if !assistant_text.is_empty() {
                message.insert("content".into(), json!(assistant_text));
            }
            let mut signature = thinking_blocks[0].thinking_signature.clone();
            if model.provider == "opencode-go" && signature.as_deref() == Some("reasoning") {
                signature = Some("reasoning_content".to_string());
            }
            if let Some(signature) = signature.filter(|s| !s.is_empty()) {
                let joined = thinking_blocks
                    .iter()
                    .map(|b| b.thinking.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                message.insert(signature, json!(joined));
            }
        }
    } else if !assistant_text.is_empty() {
        message.insert("content".into(), json!(assistant_text));
    }

    let tool_calls: Vec<&ToolCall> = assistant
        .content
        .iter()
        .filter_map(|b| b.as_tool_call())
        .collect();
    if !tool_calls.is_empty() {
        let mut encoded = Vec::with_capacity(tool_calls.len());
        for call in &tool_calls {
            match grammar_props.get(&call.name) {
                Some(property) => encoded.push(json!({
                    "id": call.id,
                    "type": "custom",
                    "custom": {
                        "name": call.name,
                        "input": sanitize_surrogates(&grammar_tool_input(&call.name, &call.arguments, property)?)
                    }
                })),
                None => encoded.push(json!({
                    "id": call.id,
                    "type": "function",
                    "function": {
                        "name": call.name,
                        "arguments": serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".into())
                    }
                })),
            }
        }
        message.insert("tool_calls".into(), Value::Array(encoded));

        let reasoning_details: Vec<Value> = tool_calls
            .iter()
            .filter_map(|c| c.thought_signature.as_deref())
            .filter_map(|s| serde_json::from_str::<Value>(s).ok())
            .collect();
        if !reasoning_details.is_empty() {
            message.insert("reasoning_details".into(), Value::Array(reasoning_details));
        }
    }

    if compat.requires_reasoning_content_on_assistant_messages
        && model.reasoning
        && !message.contains_key("reasoning_content")
    {
        message.insert("reasoning_content".into(), json!(""));
    }

    // Skip assistant turns with neither content nor tool calls: providers
    // variously require "content or tool_calls, but not none". This covers
    // aborted responses that produced nothing.
    let has_content = match message.get("content") {
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        _ => false,
    };
    if !has_content && !message.contains_key("tool_calls") {
        return Ok(None);
    }
    Ok(Some(Value::Object(message)))
}

/// Port of `convertTools`.
pub fn convert_tools(tools: &[Tool], compat: &CompletionsCompat) -> Result<Vec<Value>, AiError> {
    let mut out = Vec::with_capacity(tools.len());
    for tool in tools {
        if let Some(grammar) =
            resolve_grammar_constrained_sampling(tool, compat.supports_openai_grammar_tools)?
        {
            out.push(json!({
                "type": "custom",
                "custom": {
                    "name": tool.name,
                    "description": tool.description,
                    "format": {
                        "type": "grammar",
                        "grammar": { "syntax": grammar.syntax(), "definition": grammar.definition }
                    }
                }
            }));
            continue;
        }

        let strict = resolve_json_schema_strict_sampling(tool, compat.supports_strict_mode)?;
        let mut function = Map::new();
        function.insert("name".into(), json!(tool.name));
        function.insert("description".into(), json!(tool.description));
        function.insert(
            "parameters".into(),
            json_schema_tool_parameters(tool, strict),
        );
        let mut entry = Map::new();
        entry.insert("type".into(), json!("function"));
        // Only send `strict` where it is supported; some providers reject unknown keys.
        if compat.supports_strict_mode {
            function.insert("strict".into(), json!(strict.unwrap_or(false)));
        }
        entry.insert("function".into(), Value::Object(function));
        out.push(Value::Object(entry));
    }
    Ok(out)
}

/// Build the request body with the adapter's own defaults resolved from
/// `options`. Used by tests and by callers that want to inspect the payload
/// without issuing a request.
pub fn build_body_for(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
) -> Result<Value, AiError> {
    let compat = completions_compat(model);
    let grammar_props = create_grammar_tool_input_properties(
        context.tools.as_deref(),
        compat.supports_openai_grammar_tools,
    )?;
    let cache_retention = resolve_cache_retention(options.cache_retention, &options.request.env);
    build_completions_body(
        model,
        context,
        options,
        &compat,
        cache_retention,
        &grammar_props,
    )
}
