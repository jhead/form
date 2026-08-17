//! Port of `packages/agent/src/agent-loop.ts`.
//!
//! The loop works with [`AgentMessage`] throughout and converts to
//! `pi_core::Message` only at the provider boundary.
//!
//! Two ordering rules from upstream are load-bearing and are asserted by the
//! ported tests:
//!
//! - In `Parallel` mode, `tool_execution_end` fires in **completion** order,
//!   while the tool-result message artifacts (`message_start`/`message_end`)
//!   are emitted afterwards in **assistant source** order.
//! - Early termination happens only when *every* finalized result in a batch
//!   sets `terminate`.

use std::sync::Arc;

use futures::future::BoxFuture;
use futures_util::StreamExt;
use parking_lot::Mutex;
use pi_core::{
    AbortSignal, AssistantMessage, AssistantMessageEvent, Context, Message, StopReason, StreamFn,
    ToolCall, ToolResultMessage,
};
use serde_json::{Map, Value};

use crate::error::AgentError;
use crate::types::{
    AfterToolCallContext, AgentContext, AgentEvent, AgentEventSink, AgentLoopConfig, AgentMessage,
    AgentToolRef, BeforeToolCallContext, ToolContext, ToolExecutionMode, ToolResult,
    ToolUpdateCallback, TurnContext,
};

/// A running agent loop: a channel of [`AgentEvent`]s plus the final transcript.
///
/// The event channel is unbounded, matching upstream's `EventStream`, so a slow
/// consumer never deadlocks the loop. Events are also delivered to the
/// `AgentEventSink` form (`run_agent_loop`) if you need backpressure instead.
pub struct AgentRun {
    events: tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
    handle: tokio::task::JoinHandle<Result<Vec<AgentMessage>, AgentError>>,
}

impl AgentRun {
    /// Next event, or `None` once the loop has finished emitting.
    pub async fn next_event(&mut self) -> Option<AgentEvent> {
        self.events.recv().await
    }

    /// Drain every event the loop emits, blocking until it finishes.
    pub async fn collect_events(&mut self) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        while let Some(event) = self.events.recv().await {
            out.push(event);
        }
        out
    }

    /// Drain any remaining events and return the messages this run produced.
    pub async fn result(mut self) -> Result<Vec<AgentMessage>, AgentError> {
        while self.events.recv().await.is_some() {}
        match self.handle.await {
            Ok(result) => result,
            Err(join) => Err(AgentError::invalid_state(format!(
                "agent loop task failed: {join}"
            ))),
        }
    }
}

impl futures::Stream for AgentRun {
    type Item = AgentEvent;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<AgentEvent>> {
        self.events.poll_recv(cx)
    }
}

fn channel_sink() -> (
    AgentEventSink,
    tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let sink: AgentEventSink = Arc::new(move |event| {
        let _ = tx.send(event);
        Box::pin(std::future::ready(()))
    });
    (sink, rx)
}

/// Start an agent loop with new prompt messages.
///
/// The prompts are appended to the context and message events are emitted for
/// them before the first provider request.
pub fn agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<AbortSignal>,
    stream_fn: StreamFn,
) -> AgentRun {
    let (sink, events) = channel_sink();
    let handle = tokio::spawn(async move {
        run_agent_loop(prompts, context, config, sink, signal, stream_fn).await
    });
    AgentRun { events, handle }
}

/// Continue from the current context without adding a message. Used for retries.
///
/// The last message must convert to a `user` or `toolResult` message via the
/// config's converter; that cannot be checked here because the converter only
/// runs once per turn.
pub fn agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<AbortSignal>,
    stream_fn: StreamFn,
) -> Result<AgentRun, AgentError> {
    check_continuable(&context)?;
    let (sink, events) = channel_sink();
    let handle = tokio::spawn(async move {
        run_agent_loop_continue(context, config, sink, signal, stream_fn).await
    });
    Ok(AgentRun { events, handle })
}

fn check_continuable(context: &AgentContext) -> Result<(), AgentError> {
    if context.messages.is_empty() {
        return Err(AgentError::invalid_state(
            "Cannot continue: no messages in context",
        ));
    }
    if matches!(context.messages.last(), Some(AgentMessage::Assistant(_))) {
        return Err(AgentError::invalid_state(
            "Cannot continue from message role: assistant",
        ));
    }
    Ok(())
}

/// Sink-driven form of [`agent_loop`], for callers that need awaited listeners.
pub async fn run_agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    emit: AgentEventSink,
    signal: Option<AbortSignal>,
    stream_fn: StreamFn,
) -> Result<Vec<AgentMessage>, AgentError> {
    let mut new_messages: Vec<AgentMessage> = prompts.clone();
    let mut current_context = context;
    current_context.messages.extend(prompts.iter().cloned());

    emit(AgentEvent::AgentStart).await;
    emit(AgentEvent::TurnStart).await;
    for prompt in &prompts {
        emit(AgentEvent::MessageStart {
            message: prompt.clone(),
        })
        .await;
        emit(AgentEvent::MessageEnd {
            message: prompt.clone(),
        })
        .await;
    }

    run_loop(
        current_context,
        &mut new_messages,
        config,
        signal,
        &emit,
        &stream_fn,
    )
    .await?;
    Ok(new_messages)
}

/// Sink-driven form of [`agent_loop_continue`].
pub async fn run_agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    emit: AgentEventSink,
    signal: Option<AbortSignal>,
    stream_fn: StreamFn,
) -> Result<Vec<AgentMessage>, AgentError> {
    check_continuable(&context)?;

    let mut new_messages: Vec<AgentMessage> = Vec::new();
    emit(AgentEvent::AgentStart).await;
    emit(AgentEvent::TurnStart).await;

    run_loop(
        context,
        &mut new_messages,
        config,
        signal,
        &emit,
        &stream_fn,
    )
    .await?;
    Ok(new_messages)
}

fn aborted(signal: &Option<AbortSignal>) -> bool {
    signal.as_ref().is_some_and(|s| s.is_aborted())
}

async fn poll_source(source: &Option<Arc<dyn crate::types::MessageSource>>) -> Vec<AgentMessage> {
    match source {
        Some(source) => source.take_messages().await,
        None => Vec::new(),
    }
}

/// The main loop shared by the prompt and continuation entry points.
async fn run_loop(
    initial_context: AgentContext,
    new_messages: &mut Vec<AgentMessage>,
    initial_config: AgentLoopConfig,
    signal: Option<AbortSignal>,
    emit: &AgentEventSink,
    stream_fn: &StreamFn,
) -> Result<(), AgentError> {
    let mut current_context = initial_context;
    let mut config = initial_config;
    let mut first_turn = true;
    // The user may have typed while the previous run was still finishing.
    let mut pending_messages = poll_source(&config.get_steering_messages).await;

    // Outer loop: re-entered when follow-up messages arrive after the agent
    // would otherwise stop.
    loop {
        let mut has_more_tool_calls = true;

        while has_more_tool_calls || !pending_messages.is_empty() {
            if first_turn {
                first_turn = false;
            } else {
                emit(AgentEvent::TurnStart).await;
            }

            if !pending_messages.is_empty() {
                for message in std::mem::take(&mut pending_messages) {
                    emit(AgentEvent::MessageStart {
                        message: message.clone(),
                    })
                    .await;
                    emit(AgentEvent::MessageEnd {
                        message: message.clone(),
                    })
                    .await;
                    current_context.messages.push(message.clone());
                    new_messages.push(message);
                }
            }

            let message = stream_assistant_response(
                &mut current_context,
                &config,
                signal.clone(),
                emit,
                stream_fn,
            )
            .await?;
            new_messages.push(AgentMessage::Assistant(message.clone()));

            if matches!(message.stop_reason, StopReason::Error | StopReason::Aborted) {
                emit(AgentEvent::TurnEnd {
                    message: AgentMessage::Assistant(message),
                    tool_results: Vec::new(),
                })
                .await;
                emit(AgentEvent::AgentEnd {
                    messages: new_messages.clone(),
                })
                .await;
                return Ok(());
            }

            let tool_calls: Vec<ToolCall> = message.tool_calls().cloned().collect();
            let mut tool_results: Vec<ToolResultMessage> = Vec::new();
            has_more_tool_calls = false;

            if !tool_calls.is_empty() {
                // A "length" stop means the output was cut off by the token
                // limit, so every tool call may carry truncated arguments. Fail
                // them all rather than execute potentially borked calls.
                let batch = if message.stop_reason == StopReason::Length {
                    fail_tool_calls_from_truncated_message(&tool_calls, emit).await
                } else {
                    execute_tool_calls(
                        &current_context,
                        &message,
                        &tool_calls,
                        &config,
                        signal.clone(),
                        emit,
                    )
                    .await
                };
                tool_results.extend(batch.messages);
                has_more_tool_calls = !batch.terminate;

                for result in &tool_results {
                    current_context
                        .messages
                        .push(AgentMessage::ToolResult(result.clone()));
                    new_messages.push(AgentMessage::ToolResult(result.clone()));
                }
            }

            emit(AgentEvent::TurnEnd {
                message: AgentMessage::Assistant(message.clone()),
                tool_results: tool_results.clone(),
            })
            .await;

            let turn_context = TurnContext {
                message: AgentMessage::Assistant(message),
                tool_results,
                context: current_context.clone(),
                new_messages: new_messages.clone(),
            };

            if let Some(hook) = &config.prepare_next_turn {
                if let Some(update) = hook.prepare_next_turn(turn_context.clone()).await {
                    if let Some(context) = update.context {
                        current_context = context;
                    }
                    if let Some(model) = update.model {
                        config.model = model;
                    }
                    if let Some(level) = update.thinking_level {
                        config.stream_options.reasoning = level.level();
                    }
                }
            }

            if let Some(hook) = &config.should_stop_after_turn {
                // Re-snapshot the context: prepare_next_turn may have replaced it.
                let mut stop_context = turn_context;
                stop_context.context = current_context.clone();
                if hook.should_stop_after_turn(stop_context).await {
                    emit(AgentEvent::AgentEnd {
                        messages: new_messages.clone(),
                    })
                    .await;
                    return Ok(());
                }
            }

            pending_messages = poll_source(&config.get_steering_messages).await;
        }

        // The agent would stop here. Check for follow-up messages.
        let follow_ups = poll_source(&config.get_follow_up_messages).await;
        if !follow_ups.is_empty() {
            pending_messages = follow_ups;
            continue;
        }
        break;
    }

    emit(AgentEvent::AgentEnd {
        messages: new_messages.clone(),
    })
    .await;
    Ok(())
}

/// Stream one assistant response. This is where `AgentMessage` becomes
/// `pi_core::Message` for the provider.
async fn stream_assistant_response(
    context: &mut AgentContext,
    config: &AgentLoopConfig,
    signal: Option<AbortSignal>,
    emit: &AgentEventSink,
    stream_fn: &StreamFn,
) -> Result<AssistantMessage, AgentError> {
    let mut messages = context.messages.clone();
    if let Some(transform) = &config.transform_context {
        messages = transform.transform(messages, signal.clone()).await;
    }
    let llm_messages: Vec<Message> = config.convert_to_llm.convert(&messages).await;

    let llm_context = Context {
        system_prompt: Some(context.system_prompt.clone()),
        messages: llm_messages,
        tools: Some(context.tools.iter().map(|t| t.declaration()).collect()),
    };

    // Resolved per request, because OAuth tokens expire during long tool phases.
    let resolved_key = match &config.get_api_key {
        Some(provider) => provider.api_key(&config.model.provider).await,
        None => None,
    }
    .or_else(|| config.stream_options.stream.request.api_key.clone());

    let mut options = config.stream_options.clone();
    options.stream.request.api_key = resolved_key;
    options.stream.request.signal = signal.clone();

    let mut stream = stream_fn(config.model.clone(), llm_context, options).await?;

    let mut added_partial = false;
    while let Some(event) = stream.next().await {
        match &event {
            AssistantMessageEvent::Start { partial } => {
                context
                    .messages
                    .push(AgentMessage::Assistant(partial.clone()));
                added_partial = true;
                emit(AgentEvent::MessageStart {
                    message: AgentMessage::Assistant(partial.clone()),
                })
                .await;
            }
            AssistantMessageEvent::Done { message, .. } => {
                return finish_assistant_message(context, emit, message.clone(), added_partial)
                    .await;
            }
            AssistantMessageEvent::Error { error, .. } => {
                return finish_assistant_message(context, emit, error.clone(), added_partial).await;
            }
            other => {
                // Only track partials once `start` established one, matching upstream.
                if added_partial {
                    if let Some(partial) = other.partial() {
                        if let Some(last) = context.messages.last_mut() {
                            *last = AgentMessage::Assistant(partial.clone());
                        }
                        emit(AgentEvent::MessageUpdate {
                            message: AgentMessage::Assistant(partial.clone()),
                            assistant_message_event: event.clone(),
                        })
                        .await;
                    }
                }
            }
        }
    }

    Err(AgentError::Stream {
        code: "protocol".into(),
        message: "assistant stream ended without a done or error event".into(),
    })
}

async fn finish_assistant_message(
    context: &mut AgentContext,
    emit: &AgentEventSink,
    final_message: AssistantMessage,
    added_partial: bool,
) -> Result<AssistantMessage, AgentError> {
    if added_partial {
        if let Some(last) = context.messages.last_mut() {
            *last = AgentMessage::Assistant(final_message.clone());
        }
    } else {
        context
            .messages
            .push(AgentMessage::Assistant(final_message.clone()));
        emit(AgentEvent::MessageStart {
            message: AgentMessage::Assistant(final_message.clone()),
        })
        .await;
    }
    emit(AgentEvent::MessageEnd {
        message: AgentMessage::Assistant(final_message.clone()),
    })
    .await;
    Ok(final_message)
}

// ---------------------------------------------------------------------------
// Tool dispatch
// ---------------------------------------------------------------------------

struct ExecutedToolCallBatch {
    messages: Vec<ToolResultMessage>,
    terminate: bool,
}

#[derive(Clone)]
struct FinalizedToolCall {
    tool_call: ToolCall,
    result: ToolResult,
    is_error: bool,
}

enum Preparation {
    Immediate { result: ToolResult, is_error: bool },
    Prepared { tool: AgentToolRef, args: Value },
}

/// Upstream's `createErrorToolResult`: the message as text, `details` as `{}`.
fn error_tool_result(message: impl Into<String>) -> ToolResult {
    ToolResult {
        content: vec![pi_core::InputContent::text(message)],
        details: Some(Value::Object(Map::new())),
        ..Default::default()
    }
}

fn should_terminate_tool_batch(finalized: &[FinalizedToolCall]) -> bool {
    !finalized.is_empty() && finalized.iter().all(|f| f.result.terminate == Some(true))
}

/// Fail every tool call from a message the output token limit truncated.
///
/// Streamed tool-call arguments are finalized with a best-effort JSON salvage
/// parser, so a truncated message can yield calls whose arguments validate but
/// are silently incomplete. None are safe to execute.
async fn fail_tool_calls_from_truncated_message(
    tool_calls: &[ToolCall],
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let mut messages = Vec::new();
    for tool_call in tool_calls {
        emit_tool_execution_start(emit, tool_call).await;
        let finalized = FinalizedToolCall {
            tool_call: tool_call.clone(),
            result: error_tool_result(format!(
                "Tool call \"{}\" was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments.",
                tool_call.name
            )),
            is_error: true,
        };
        emit_tool_execution_end(&finalized, emit).await;
        let message = create_tool_result_message(&finalized);
        emit_tool_result_message(&message, emit).await;
        messages.push(message);
    }
    ExecutedToolCallBatch {
        messages,
        terminate: false,
    }
}

async fn execute_tool_calls(
    context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[ToolCall],
    config: &AgentLoopConfig,
    signal: Option<AbortSignal>,
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    // A single sequential-only tool forces the whole batch sequential.
    let has_sequential = tool_calls.iter().any(|tc| {
        context
            .find_tool(&tc.name)
            .and_then(|t| t.execution_mode())
            .is_some_and(|mode| mode == ToolExecutionMode::Sequential)
    });

    if config.tool_execution == ToolExecutionMode::Sequential || has_sequential {
        execute_tool_calls_sequential(context, assistant_message, tool_calls, config, signal, emit)
            .await
    } else {
        execute_tool_calls_parallel(context, assistant_message, tool_calls, config, signal, emit)
            .await
    }
}

async fn execute_tool_calls_sequential(
    context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[ToolCall],
    config: &AgentLoopConfig,
    signal: Option<AbortSignal>,
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let mut finalized_calls = Vec::new();
    let mut messages = Vec::new();

    for tool_call in tool_calls {
        emit_tool_execution_start(emit, tool_call).await;

        let preparation =
            prepare_tool_call(context, assistant_message, tool_call, config, &signal).await;
        let finalized = match preparation {
            Preparation::Immediate { result, is_error } => FinalizedToolCall {
                tool_call: tool_call.clone(),
                result,
                is_error,
            },
            Preparation::Prepared { tool, args } => {
                let executed =
                    execute_prepared_tool_call(context, &tool, tool_call, &args, &signal, emit)
                        .await;
                finalize_executed_tool_call(
                    context,
                    assistant_message,
                    tool_call,
                    &args,
                    executed,
                    config,
                    &signal,
                )
                .await
            }
        };

        emit_tool_execution_end(&finalized, emit).await;
        let message = create_tool_result_message(&finalized);
        emit_tool_result_message(&message, emit).await;
        finalized_calls.push(finalized);
        messages.push(message);

        if aborted(&signal) {
            break;
        }
    }

    ExecutedToolCallBatch {
        terminate: should_terminate_tool_batch(&finalized_calls),
        messages,
    }
}

async fn execute_tool_calls_parallel(
    context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[ToolCall],
    config: &AgentLoopConfig,
    signal: Option<AbortSignal>,
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    enum Entry<'a> {
        // Boxed so the already-finalized arm does not inflate every slot in the
        // vector to the size of a `FinalizedToolCall`.
        Done(Box<FinalizedToolCall>),
        Pending(BoxFuture<'a, FinalizedToolCall>),
    }

    let mut entries: Vec<Entry<'_>> = Vec::new();

    // Preparation is sequential; only execution is concurrent.
    for tool_call in tool_calls {
        emit_tool_execution_start(emit, tool_call).await;

        let preparation =
            prepare_tool_call(context, assistant_message, tool_call, config, &signal).await;
        match preparation {
            Preparation::Immediate { result, is_error } => {
                let finalized = FinalizedToolCall {
                    tool_call: tool_call.clone(),
                    result,
                    is_error,
                };
                emit_tool_execution_end(&finalized, emit).await;
                entries.push(Entry::Done(Box::new(finalized)));
                if aborted(&signal) {
                    break;
                }
            }
            Preparation::Prepared { tool, args } => {
                let call_signal = signal.clone();
                entries.push(Entry::Pending(Box::pin(async move {
                    let executed = execute_prepared_tool_call(
                        context,
                        &tool,
                        tool_call,
                        &args,
                        &call_signal,
                        emit,
                    )
                    .await;
                    let finalized = finalize_executed_tool_call(
                        context,
                        assistant_message,
                        tool_call,
                        &args,
                        executed,
                        config,
                        &call_signal,
                    )
                    .await;
                    // Completion order, before the source-ordered artifacts below.
                    emit_tool_execution_end(&finalized, emit).await;
                    finalized
                })));
                if aborted(&signal) {
                    break;
                }
            }
        }
    }

    let ordered = futures::future::join_all(entries.into_iter().map(|entry| async move {
        match entry {
            Entry::Done(finalized) => *finalized,
            Entry::Pending(future) => future.await,
        }
    }))
    .await;

    // Message artifacts are emitted in assistant source order.
    let mut messages = Vec::with_capacity(ordered.len());
    for finalized in &ordered {
        let message = create_tool_result_message(finalized);
        emit_tool_result_message(&message, emit).await;
        messages.push(message);
    }

    ExecutedToolCallBatch {
        terminate: should_terminate_tool_batch(&ordered),
        messages,
    }
}

async fn prepare_tool_call(
    context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_call: &ToolCall,
    config: &AgentLoopConfig,
    signal: &Option<AbortSignal>,
) -> Preparation {
    let Some(tool) = context.find_tool(&tool_call.name).cloned() else {
        return Preparation::Immediate {
            result: error_tool_result(format!("Tool {} not found", tool_call.name)),
            is_error: true,
        };
    };

    // `prepare_arguments` is a free-form shim, so it may hand back a non-object;
    // fall back to the raw arguments rather than losing the call.
    let prepared = tool.prepare_arguments(Value::Object(tool_call.arguments.clone()));
    let prepared_call = ToolCall {
        arguments: match prepared {
            Value::Object(map) => map,
            _ => tool_call.arguments.clone(),
        },
        ..tool_call.clone()
    };

    let mut args =
        match pi_http::validation::validate_tool_arguments(&tool.declaration(), &prepared_call) {
            Ok(args) => Value::Object(args),
            Err(error) => {
                return Preparation::Immediate {
                    result: error_tool_result(error.to_string()),
                    is_error: true,
                }
            }
        };

    if let Some(hook) = &config.before_tool_call {
        let before = hook
            .before_tool_call(
                BeforeToolCallContext {
                    assistant_message: assistant_message.clone(),
                    tool_call: tool_call.clone(),
                    args: args.clone(),
                    context: context.clone(),
                },
                signal.clone(),
            )
            .await;

        if aborted(signal) {
            return Preparation::Immediate {
                result: error_tool_result("Operation aborted"),
                is_error: true,
            };
        }

        if let Some(before) = before {
            if before.block == Some(true) {
                let mut result = error_tool_result(
                    before
                        .reason
                        .unwrap_or_else(|| "Tool execution was blocked".to_string()),
                );
                if before.terminate == Some(true) {
                    result.terminate = Some(true);
                }
                return Preparation::Immediate {
                    result,
                    is_error: true,
                };
            }
            // Upstream mutates `context.args` in place; the port carries the
            // replacement back on the result. Deliberately not revalidated.
            if let Some(replacement) = before.args {
                args = replacement;
            }
        }
    }

    if aborted(signal) {
        return Preparation::Immediate {
            result: error_tool_result("Operation aborted"),
            is_error: true,
        };
    }

    Preparation::Prepared { tool, args }
}

struct ExecutedToolCall {
    result: ToolResult,
    is_error: bool,
}

async fn execute_prepared_tool_call(
    context: &AgentContext,
    tool: &AgentToolRef,
    tool_call: &ToolCall,
    args: &Value,
    signal: &Option<AbortSignal>,
    emit: &AgentEventSink,
) -> ExecutedToolCall {
    // `Some(buffer)` means "still accepting"; `take()` latches it closed the way
    // upstream's `acceptingUpdates` flag does, so post-settlement calls are dropped.
    let buffer: Arc<Mutex<Option<Vec<ToolResult>>>> = Arc::new(Mutex::new(Some(Vec::new())));
    let sink = buffer.clone();
    let on_update: ToolUpdateCallback = Arc::new(move |partial: ToolResult| {
        if let Some(pending) = sink.lock().as_mut() {
            pending.push(partial);
        }
    });

    let tool_context = ToolContext::new(context.env.clone())
        .with_tool_call_id(tool_call.id.clone())
        .with_on_update(on_update);
    let outcome = tool
        .execute(args.clone(), &tool_context, signal.clone())
        .await;

    let updates = buffer.lock().take().unwrap_or_default();
    for partial in updates {
        emit(AgentEvent::ToolExecutionUpdate {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            args: Value::Object(tool_call.arguments.clone()),
            partial_result: partial,
        })
        .await;
    }

    match outcome {
        Ok(result) => ExecutedToolCall {
            result,
            is_error: false,
        },
        Err(error) => ExecutedToolCall {
            result: error_tool_result(error.message()),
            is_error: true,
        },
    }
}

#[allow(clippy::too_many_arguments)]
async fn finalize_executed_tool_call(
    context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_call: &ToolCall,
    args: &Value,
    executed: ExecutedToolCall,
    config: &AgentLoopConfig,
    signal: &Option<AbortSignal>,
) -> FinalizedToolCall {
    let mut result = executed.result;
    let mut is_error = executed.is_error;

    if let Some(hook) = &config.after_tool_call {
        let outcome = hook
            .after_tool_call(
                AfterToolCallContext {
                    assistant_message: assistant_message.clone(),
                    tool_call: tool_call.clone(),
                    args: args.clone(),
                    result: result.clone(),
                    is_error,
                    context: context.clone(),
                },
                signal.clone(),
            )
            .await;

        match outcome {
            // Each present field replaces its counterpart wholesale. No deep merge.
            Ok(Some(after)) => {
                if let Some(content) = after.content {
                    result.content = content;
                }
                if let Some(details) = after.details {
                    result.details = Some(details);
                }
                if let Some(usage) = after.usage {
                    result.usage = Some(usage);
                }
                if let Some(terminate) = after.terminate {
                    result.terminate = Some(terminate);
                }
                if let Some(flag) = after.is_error {
                    is_error = flag;
                }
            }
            Ok(None) => {}
            Err(error) => {
                result = error_tool_result(error.message());
                is_error = true;
            }
        }
    }

    FinalizedToolCall {
        tool_call: tool_call.clone(),
        result,
        is_error,
    }
}

async fn emit_tool_execution_start(emit: &AgentEventSink, tool_call: &ToolCall) {
    emit(AgentEvent::ToolExecutionStart {
        tool_call_id: tool_call.id.clone(),
        tool_name: tool_call.name.clone(),
        args: Value::Object(tool_call.arguments.clone()),
    })
    .await;
}

async fn emit_tool_execution_end(finalized: &FinalizedToolCall, emit: &AgentEventSink) {
    emit(AgentEvent::ToolExecutionEnd {
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        result: finalized.result.clone(),
        is_error: finalized.is_error,
    })
    .await;
}

fn create_tool_result_message(finalized: &FinalizedToolCall) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        content: finalized.result.content.clone(),
        details: finalized.result.details.clone(),
        usage: finalized.result.usage.clone(),
        // Upstream only sets the key when the list is non-empty.
        added_tool_names: finalized
            .result
            .added_tool_names
            .clone()
            .filter(|names| !names.is_empty()),
        is_error: finalized.is_error,
        timestamp: pi_core::now_ms(),
    }
}

async fn emit_tool_result_message(message: &ToolResultMessage, emit: &AgentEventSink) {
    emit(AgentEvent::MessageStart {
        message: AgentMessage::ToolResult(message.clone()),
    })
    .await;
    emit(AgentEvent::MessageEnd {
        message: AgentMessage::ToolResult(message.clone()),
    })
    .await;
}
