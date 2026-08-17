//! Test doubles for the agent loop, exported so downstream crates can drive it
//! without a provider.
//!
//! `pi_provider_misc::faux::FauxProvider` is the fuller double — a real adapter
//! that streams token by token — and the tests use it wherever provider
//! behaviour is what is under test. [`ScriptedStream`] stays for the loop tests,
//! which need to script one exact terminal message per turn and assert on the
//! request the loop built.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use pi_core::{
    Api, AssistantContent, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream,
    Context, DoneReason, Model, SimpleStreamOptions, StopReason, StreamFn, ToolCall, Usage,
};
use pi_tools::{ToolContext, ToolError};
use serde_json::Value;

use crate::types::{AgentTool, ToolExecutionMode, ToolResult};

/// A one-shot, level-triggered gate.
///
/// `tokio::sync::Notify` is edge-triggered: `notify_waiters()` before the waiter
/// polls is lost and the test hangs forever. Upstream's tests use a JS promise,
/// which latches, so the port needs a latching primitive too.
#[derive(Clone)]
pub struct Gate(tokio::sync::watch::Sender<bool>);

impl Default for Gate {
    fn default() -> Self {
        Self::new()
    }
}

impl Gate {
    pub fn new() -> Self {
        Gate(tokio::sync::watch::channel(false).0)
    }

    pub fn open(&self) {
        self.0.send_replace(true);
    }

    pub fn is_open(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolve once the gate is open, whether or not it opened first.
    pub async fn wait(&self) {
        let mut rx = self.0.subscribe();
        while !*rx.borrow() {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}

/// The model the ported upstream tests use.
pub fn mock_model() -> Model {
    Model {
        context_window: 8192,
        max_tokens: 2048,
        ..Model::new(
            "mock",
            Api::OpenAiResponses,
            "openai",
            "https://example.invalid",
        )
    }
}

/// An assistant message shaped like upstream's `createAssistantMessage`.
pub fn assistant_message(
    content: Vec<AssistantContent>,
    stop_reason: StopReason,
) -> AssistantMessage {
    AssistantMessage {
        content,
        api: "openai-responses".into(),
        provider: "openai".into(),
        model: "mock".into(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: pi_core::now_ms(),
    }
}

pub fn text_message(text: &str) -> AssistantMessage {
    assistant_message(vec![AssistantContent::text(text)], StopReason::Stop)
}

pub fn tool_call(id: &str, name: &str, arguments: Value) -> AssistantContent {
    AssistantContent::ToolCall(ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: arguments.as_object().cloned().unwrap_or_default(),
        thought_signature: None,
        namespace: None,
    })
}

pub fn tool_use_message(calls: Vec<AssistantContent>) -> AssistantMessage {
    assistant_message(calls, StopReason::ToolUse)
}

/// One scripted provider turn.
#[derive(Clone)]
pub enum Turn {
    /// Terminate with `done` and the given message.
    Done(AssistantMessage),
    /// Terminate with `error` and the given message.
    Failed(AssistantMessage),
    /// A full event script, terminal event included.
    Events(Vec<AssistantMessageEvent>),
}

impl Turn {
    fn into_events(self) -> Vec<AssistantMessageEvent> {
        match self {
            Turn::Done(message) => {
                let reason = match message.stop_reason {
                    StopReason::Length => DoneReason::Length,
                    StopReason::ToolUse => DoneReason::ToolUse,
                    StopReason::Deferred => DoneReason::Deferred,
                    _ => DoneReason::Stop,
                };
                vec![AssistantMessageEvent::Done { reason, message }]
            }
            Turn::Failed(error) => vec![AssistantMessageEvent::Error {
                reason: pi_core::ErrorReason::Error,
                error,
            }],
            Turn::Events(events) => events,
        }
    }
}

/// What a [`ScriptedStream`] observed for one request.
#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub model: Model,
    pub context: Context,
    pub session_id: Option<String>,
    pub api_key: Option<String>,
}

/// A [`StreamFn`] that replays scripted turns and records every request.
#[derive(Clone, Default)]
pub struct ScriptedStream {
    turns: Arc<Mutex<Vec<Turn>>>,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    /// Reused for every call past the end of the script.
    fallback: Arc<Mutex<Option<Turn>>>,
}

impl ScriptedStream {
    pub fn new(turns: Vec<Turn>) -> Self {
        Self {
            turns: Arc::new(Mutex::new(turns)),
            requests: Arc::new(Mutex::new(Vec::new())),
            fallback: Arc::new(Mutex::new(None)),
        }
    }

    /// Turn used once the script is exhausted, instead of a protocol error.
    pub fn with_fallback(self, turn: Turn) -> Self {
        *self.fallback.lock() = Some(turn);
        self
    }

    pub fn call_count(&self) -> usize {
        self.requests.lock().len()
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().clone()
    }

    pub fn into_stream_fn(self) -> StreamFn {
        Arc::new(
            move |model: Model, context: Context, options: SimpleStreamOptions| {
                let this = self.clone();
                Box::pin(async move {
                    this.requests.lock().push(RecordedRequest {
                        model,
                        context,
                        session_id: options.stream.session_id.clone(),
                        api_key: options.stream.request.api_key.clone(),
                    });
                    let turn = {
                        let mut turns = this.turns.lock();
                        if turns.is_empty() {
                            this.fallback.lock().clone()
                        } else {
                            Some(turns.remove(0))
                        }
                    };
                    let events = turn.map(Turn::into_events).unwrap_or_default();
                    Ok(AssistantMessageEventStream::from_events(events))
                })
            },
        )
    }
}

/// A `StreamFn` that always fails before producing a stream, the way upstream's
/// `streamFn: () => { throw }` test does.
pub fn failing_stream_fn(message: &'static str) -> StreamFn {
    Arc::new(move |_model, _context, _options| {
        Box::pin(async move { Err(pi_core::AiError::other(message)) })
    })
}

/// Argument shim for [`FnTool`], mirroring `AgentTool::prepare_arguments`.
pub type PrepareArgsFn = Arc<dyn Fn(Value) -> Value + Send + Sync>;

/// Boxed `execute` body for [`FnTool`].
pub type ExecuteFn = Arc<
    dyn Fn(
            Value,
            ToolContext,
            Option<pi_core::AbortSignal>,
        ) -> futures::future::BoxFuture<'static, Result<ToolResult, ToolError>>
        + Send
        + Sync,
>;

/// A [`pi_tools::AgentTool`] built from a closure, for tests and simple embedders.
#[derive(Clone)]
pub struct FnTool {
    name: String,
    label: String,
    description: String,
    parameters: Value,
    execution_mode: Option<ToolExecutionMode>,
    prepare: Option<PrepareArgsFn>,
    execute: ExecuteFn,
}

impl FnTool {
    pub fn new(name: &str, parameters: Value, execute: ExecuteFn) -> Self {
        Self {
            name: name.to_string(),
            label: name.to_string(),
            description: format!("{name} tool"),
            parameters,
            execution_mode: None,
            prepare: None,
            execute,
        }
    }

    pub fn with_execution_mode(mut self, mode: ToolExecutionMode) -> Self {
        self.execution_mode = Some(mode);
        self
    }

    pub fn with_prepare_arguments(mut self, prepare: PrepareArgsFn) -> Self {
        self.prepare = Some(prepare);
        self
    }
}

#[async_trait]
impl AgentTool for FnTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn description(&self) -> String {
        self.description.clone()
    }

    fn parameters(&self) -> Value {
        self.parameters.clone()
    }

    fn execution_mode(&self) -> Option<ToolExecutionMode> {
        self.execution_mode
    }

    fn prepare_arguments(&self, args: Value) -> Value {
        match &self.prepare {
            Some(prepare) => prepare(args),
            None => args,
        }
    }

    async fn execute(
        &self,
        args: Value,
        context: &ToolContext,
        abort: Option<pi_core::AbortSignal>,
    ) -> Result<ToolResult, ToolError> {
        (self.execute)(args, context.clone(), abort).await
    }
}

/// The `{ "value": string }` schema the ported upstream tests use.
pub fn value_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": { "value": { "type": "string" } },
        "required": ["value"],
        "additionalProperties": false
    })
}

/// An empty object schema.
pub fn empty_schema() -> Value {
    serde_json::json!({ "type": "object", "properties": {}, "additionalProperties": false })
}
