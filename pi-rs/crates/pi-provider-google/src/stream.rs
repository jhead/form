//! The streaming loop both Google adapters run.
//!
//! `google-generative-ai.ts` and `google-vertex.ts` contain the identical body;
//! only the request construction and two error strings differ, so the port
//! keeps one engine and parameterises those.
//!
//! Framing note: `@google/genai` does not use a WHATWG event-stream parser. It
//! splits the body on `\n\n` / `\r\r` / `\r\n\r\n`, keeps only segments that
//! start with `data:`, and separately fails the stream when a raw chunk parses
//! as a JSON object carrying an `error` with a 4xx/5xx `code`. Both rules are
//! reproduced here on top of `pi_http`'s SSE parser.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures_util::future::BoxFuture;
use futures_util::StreamExt;
use serde_json::Value;

use pi_core::content::{AssistantContent, TextContent, ThinkingContent, ToolCall};
use pi_core::event::{AssistantMessageEvent, AssistantMessageEventSink, DoneReason, ErrorReason};
use pi_core::message::{now_ms, AssistantMessage, StopReason, Usage};
use pi_core::model::Model;
use pi_core::options::StreamOptions;
use pi_core::{AiError, AssistantMessageEventStream};
use pi_http::client::{HttpClient, JsonRequest};
use pi_http::{retry_with_backoff, HttpError, RetryPolicy};

use crate::google_shared::{
    calculate_cost, is_thinking_part, map_stop_reason, retain_thought_signature,
};
use crate::wire::{GenerateContentResponse, Part};

/// Upstream generates ids as `${name}_${Date.now()}_${++toolCallCounter}` from
/// a module-level counter; this is the process-wide equivalent.
static TOOL_CALL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fully-built Gemini request: adapters differ only in how they produce this.
#[derive(Debug, Clone)]
pub(crate) struct GoogleHttpRequest {
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

/// The in-progress block being appended to, mirroring upstream's `currentBlock`.
enum CurrentBlock {
    Text(TextContent),
    Thinking(ThinkingContent),
}

struct StreamState {
    output: AssistantMessage,
    current: Option<CurrentBlock>,
    sink: AssistantMessageEventSink,
}

impl StreamState {
    fn block_index(&self) -> usize {
        self.output.content.len().saturating_sub(1)
    }

    async fn emit(&self, event: AssistantMessageEvent) -> bool {
        self.sink.send(event).await
    }

    /// Snapshot of the running message. Upstream aliases one mutable object;
    /// the Rust port clones so consumers get a true point-in-time partial.
    fn partial(&self) -> AssistantMessage {
        self.output.clone()
    }

    /// Flush whatever text/thinking block is open, emitting its `*_end` event.
    async fn close_current_block(&mut self) -> bool {
        let Some(current) = self.current.take() else {
            return true;
        };
        let content_index = self.block_index();
        let partial = self.partial();
        let event = match current {
            CurrentBlock::Text(text) => AssistantMessageEvent::TextEnd {
                content_index,
                content: text.text,
                partial,
            },
            CurrentBlock::Thinking(thinking) => AssistantMessageEvent::ThinkingEnd {
                content_index,
                content: thinking.thinking,
                partial,
            },
        };
        self.emit(event).await
    }

    /// Mirror the mutation back into `output.content` so partials stay in sync.
    fn sync_current_into_output(&mut self) {
        let Some(current) = &self.current else { return };
        let Some(last) = self.output.content.last_mut() else {
            return;
        };
        match (current, last) {
            (CurrentBlock::Text(text), AssistantContent::Text(target)) => *target = text.clone(),
            (CurrentBlock::Thinking(thinking), AssistantContent::Thinking(target)) => {
                *target = thinking.clone()
            }
            _ => {}
        }
    }

    async fn handle_text_part(&mut self, part: &Part) -> bool {
        let Some(text) = part.text.as_deref() else {
            return true;
        };
        let is_thinking = is_thinking_part(part);
        let needs_new_block = match &self.current {
            None => true,
            Some(CurrentBlock::Thinking(_)) => !is_thinking,
            Some(CurrentBlock::Text(_)) => is_thinking,
        };

        if needs_new_block {
            if !self.close_current_block().await {
                return false;
            }
            if is_thinking {
                let block = ThinkingContent::default();
                self.output
                    .content
                    .push(AssistantContent::Thinking(block.clone()));
                self.current = Some(CurrentBlock::Thinking(block));
                let content_index = self.block_index();
                let partial = self.partial();
                if !self
                    .emit(AssistantMessageEvent::ThinkingStart {
                        content_index,
                        partial,
                    })
                    .await
                {
                    return false;
                }
            } else {
                let block = TextContent::default();
                self.output
                    .content
                    .push(AssistantContent::Text(block.clone()));
                self.current = Some(CurrentBlock::Text(block));
                let content_index = self.block_index();
                let partial = self.partial();
                if !self
                    .emit(AssistantMessageEvent::TextStart {
                        content_index,
                        partial,
                    })
                    .await
                {
                    return false;
                }
            }
        }

        match self.current.as_mut() {
            Some(CurrentBlock::Thinking(block)) => {
                block.thinking.push_str(text);
                block.thinking_signature = retain_thought_signature(
                    block.thinking_signature.take(),
                    part.thought_signature.as_deref(),
                );
                self.sync_current_into_output();
                let content_index = self.block_index();
                let partial = self.partial();
                self.emit(AssistantMessageEvent::ThinkingDelta {
                    content_index,
                    delta: text.to_string(),
                    partial,
                })
                .await
            }
            Some(CurrentBlock::Text(block)) => {
                block.text.push_str(text);
                block.text_signature = retain_thought_signature(
                    block.text_signature.take(),
                    part.thought_signature.as_deref(),
                );
                self.sync_current_into_output();
                let content_index = self.block_index();
                let partial = self.partial();
                self.emit(AssistantMessageEvent::TextDelta {
                    content_index,
                    delta: text.to_string(),
                    partial,
                })
                .await
            }
            None => true,
        }
    }

    async fn handle_function_call_part(&mut self, part: &Part) -> bool {
        let Some(function_call) = &part.function_call else {
            return true;
        };
        if !self.close_current_block().await {
            return false;
        }

        let name = function_call.name.clone().unwrap_or_default();
        let provided_id = function_call.id.as_deref().filter(|id| !id.is_empty());
        let needs_new_id = match provided_id {
            None => true,
            Some(id) => self.output.tool_calls().any(|call| call.id == id),
        };
        let id = if needs_new_id {
            let counter = TOOL_CALL_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
            format!("{name}_{}_{counter}", now_ms())
        } else {
            provided_id.unwrap_or_default().to_string()
        };

        let tool_call = ToolCall {
            id,
            name,
            arguments: function_call.args.clone().unwrap_or_default(),
            thought_signature: part.thought_signature.clone(),
            namespace: None,
        };

        self.output
            .content
            .push(AssistantContent::ToolCall(tool_call.clone()));
        let content_index = self.block_index();
        let partial = self.partial();
        if !self
            .emit(AssistantMessageEvent::ToolCallStart {
                content_index,
                partial,
            })
            .await
        {
            return false;
        }
        let delta = serde_json::to_string(&tool_call.arguments).unwrap_or_else(|_| "{}".into());
        let partial = self.partial();
        if !self
            .emit(AssistantMessageEvent::ToolCallDelta {
                content_index,
                delta,
                partial,
            })
            .await
        {
            return false;
        }
        let partial = self.partial();
        self.emit(AssistantMessageEvent::ToolCallEnd {
            content_index,
            tool_call,
            partial,
        })
        .await
    }
}

/// Kick off a Google stream. Failures never surface as `Err`: they terminate
/// the returned stream with an `error` event carrying the partial message.
pub(crate) fn start_stream(
    api: &'static str,
    stream_ended_message: &'static str,
    http: Arc<HttpClient>,
    model: Model,
    options: StreamOptions,
    build_request: BoxFuture<'static, Result<GoogleHttpRequest, AiError>>,
) -> AssistantMessageEventStream {
    let (sink, stream) = AssistantMessageEventStream::channel(64);
    tokio::spawn(async move {
        run(
            api,
            stream_ended_message,
            http,
            model,
            options,
            build_request,
            sink,
        )
        .await;
    });
    stream
}

async fn run(
    api: &'static str,
    stream_ended_message: &'static str,
    http: Arc<HttpClient>,
    model: Model,
    options: StreamOptions,
    build_request: BoxFuture<'static, Result<GoogleHttpRequest, AiError>>,
    sink: AssistantMessageEventSink,
) {
    let mut state = StreamState {
        output: AssistantMessage::pending(api, &model.provider, &model.id),
        current: None,
        sink,
    };

    match run_inner(
        &mut state,
        stream_ended_message,
        &http,
        &model,
        &options,
        build_request,
    )
    .await
    {
        Ok(reason) => {
            let message = state.output.clone();
            state
                .emit(AssistantMessageEvent::Done { reason, message })
                .await;
        }
        Err(message) => {
            let aborted = options.request.is_aborted();
            state.output.stop_reason = if aborted {
                StopReason::Aborted
            } else {
                StopReason::Error
            };
            state.output.error_message = Some(message);
            let error = state.output.clone();
            state
                .emit(AssistantMessageEvent::Error {
                    reason: if aborted {
                        ErrorReason::Aborted
                    } else {
                        ErrorReason::Error
                    },
                    error,
                })
                .await;
        }
    }
}

/// The `try` body of upstream's IIFE. `Err(String)` is the thrown error message.
async fn run_inner(
    state: &mut StreamState,
    stream_ended_message: &'static str,
    http: &HttpClient,
    model: &Model,
    options: &StreamOptions,
    build_request: BoxFuture<'static, Result<GoogleHttpRequest, AiError>>,
) -> Result<DoneReason, String> {
    let signal = options.request.signal();
    if signal.is_aborted() {
        return Err("Request aborted".to_string());
    }

    let mut request = build_request.await.map_err(format_ai_error)?;

    // `onPayload` may replace the whole body, matching upstream's hook.
    if let Some(on_payload) = &options.request.on_payload {
        if let Some(next) = on_payload(&request.body, model) {
            request.body = next;
        }
    }

    let policy = RetryPolicy {
        // Upstream defaults `maxRetries` to 0 (a single attempt).
        max_attempts: options.request.max_retries.unwrap_or(0) + 1,
        max_server_delay_ms: options.request.max_retry_delay_ms.unwrap_or(60_000),
        ..Default::default()
    };
    let response = retry_with_backoff(&policy, Some(&signal), |_| {
        let req = JsonRequest::post(request.url.clone(), request.body.clone())
            .signal(Some(signal.clone()))
            .timeout_ms(options.request.timeout_ms);
        let req = request
            .headers
            .iter()
            .fold(req, |req, (name, value)| req.header(name, value));
        async { http.post_sse(req).await }
    })
    .await
    .map_err(format_http_error)?;

    if let Some(on_response) = &options.request.on_response {
        on_response(
            &pi_core::options::ProviderResponse {
                status: response.status,
                headers: response.headers.clone(),
            },
            model,
        );
    }

    if !state
        .emit(AssistantMessageEvent::Start {
            partial: state.partial(),
        })
        .await
    {
        return Err("Request was aborted".to_string());
    }

    let mut body = response.body;
    loop {
        let next = tokio::select! {
            biased;
            _ = signal.aborted() => return Err("Request was aborted".to_string()),
            next = body.next() => next,
        };
        let Some(event) = next else { break };
        let event = event.map_err(format_http_error)?;
        let data = event.data.trim();
        if data.is_empty() {
            continue;
        }

        let chunk: GenerateContentResponse = match serde_json::from_str(data) {
            Ok(chunk) => chunk,
            Err(err) => return Err(format!("exception parsing stream chunk {data}. {err}")),
        };
        // The SDK fails the stream when a chunk carries an API error envelope.
        if let Some(error) = &chunk.error {
            if let Some(message) = api_error_message(error, data) {
                return Err(message);
            }
        }

        if state.output.response_id.is_none() {
            state.output.response_id = chunk.response_id.clone();
        }
        if state.output.response_model.is_none() {
            if let Some(version) = &chunk.model_version {
                if version != &model.id {
                    state.output.response_model = Some(version.clone());
                }
            }
        }

        let candidate = chunk.candidates.as_ref().and_then(|c| c.first());
        if let Some(content) = candidate.and_then(|c| c.content.as_ref()) {
            for part in content.parts() {
                if !state.handle_text_part(part).await {
                    return Err("Request was aborted".to_string());
                }
                if !state.handle_function_call_part(part).await {
                    return Err("Request was aborted".to_string());
                }
            }
        }

        if let Some(reason) = candidate.and_then(|c| c.finish_reason.as_deref()) {
            state.output.raw_stop_reason = Some(reason.to_string());
            let mut stop_reason = map_stop_reason(reason);
            if stop_reason == StopReason::Stop && state.output.tool_calls().next().is_some() {
                stop_reason = StopReason::ToolUse;
            }
            state.output.stop_reason = stop_reason;
        }

        if let Some(metadata) = &chunk.usage_metadata {
            let prompt = metadata.prompt_token_count.unwrap_or(0);
            let cached = metadata.cached_content_token_count.unwrap_or(0);
            let thoughts = metadata.thoughts_token_count.unwrap_or(0);
            let mut usage = Usage {
                // Upstream subtracts plainly here (no clamp at zero), so with
                // signed counters this can legitimately go negative.
                input: prompt - cached,
                output: metadata.candidates_token_count.unwrap_or(0) + thoughts,
                cache_read: cached,
                cache_write: 0,
                cache_write_1h: None,
                reasoning: Some(thoughts),
                total_tokens: metadata.total_token_count.unwrap_or(0),
                cost: Default::default(),
            };
            calculate_cost(model, &mut usage);
            state.output.usage = usage;
        }
    }

    if !state.close_current_block().await {
        return Err("Request was aborted".to_string());
    }

    if signal.is_aborted() {
        return Err("Request was aborted".to_string());
    }
    match state.output.stop_reason {
        StopReason::Pending => Err(stream_ended_message.to_string()),
        StopReason::Aborted | StopReason::Error => Err(state
            .output
            .raw_stop_reason
            .as_ref()
            .map(|raw| format!("Provider stopped with: {raw}"))
            .unwrap_or_else(|| "An unknown error occurred".to_string())),
        StopReason::Stop => Ok(DoneReason::Stop),
        StopReason::Length => Ok(DoneReason::Length),
        StopReason::ToolUse => Ok(DoneReason::ToolUse),
        StopReason::Deferred => Ok(DoneReason::Deferred),
    }
}

/// Reproduce the SDK's mid-stream error text: `got status: {status}. {chunk}`.
fn api_error_message(error: &Value, raw_chunk: &str) -> Option<String> {
    let code = error.get("code").and_then(Value::as_u64)?;
    if !(400..600).contains(&code) {
        return None;
    }
    let status = error
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Some(format!("got status: {status}. {raw_chunk}"))
}

/// `formatProviderError(normalizeProviderError(error))` for an `AiError`.
///
/// Upstream surfaces the provider's raw JSON error body when there is one,
/// because that is what `@google/genai` puts in `ApiError.message`.
fn format_ai_error(error: AiError) -> String {
    if let AiError::Provider {
        body: Some(body), ..
    } = &error
    {
        if let Ok(serialized) = serde_json::to_string(body) {
            return serialized;
        }
    }
    error.message()
}

/// The `@google/genai` `ApiError.message` for a non-2xx response is the raw
/// JSON error body, so keep the body rather than pi-http's extracted summary.
fn format_http_error(error: HttpError) -> String {
    match error {
        HttpError::Status {
            body: Some(body), ..
        } => serde_json::to_string(&body).unwrap_or_default(),
        HttpError::Status { message, .. } => message,
        HttpError::Aborted => "Request was aborted".to_string(),
        other => other.to_string(),
    }
}
