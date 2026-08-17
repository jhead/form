//! Port of `packages/agent/src/agent.ts`.
//!
//! Stateful wrapper around the low-level loop: owns the transcript, reduces
//! loop events into [`AgentState`], executes tools, and exposes the steering /
//! follow-up queues.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use pi_core::{
    AbortHandle, AbortSignal, AssistantMessage, ImageContent, InputContent, Model,
    ModelThinkingLevel, StopReason, StreamFn, Usage,
};

use crate::agent_loop::{run_agent_loop, run_agent_loop_continue};
use crate::error::AgentError;
use crate::stream_fn::resolve_stream_fn;
use crate::types::{
    AfterToolCall, AgentContext, AgentEvent, AgentEventSink, AgentLoopConfig, AgentMessage,
    AgentState, AgentToolRef, ApiKeyProvider, BeforeToolCall, ContextTransform,
    DefaultMessageConverter, ExecutionEnvRef, MessageConverter, MessageSource, PrepareNextTurn,
    QueueMode, ShouldStopAfterTurn, ToolExecutionMode, TurnContext,
};

/// Subscriber for agent lifecycle events.
///
/// Listeners are awaited in subscription order and are part of the run's
/// settlement: the agent becomes idle only after the `agent_end` listeners
/// finish. Each call receives the active run's abort signal.
#[async_trait]
pub trait AgentEventListener: Send + Sync + 'static {
    async fn on_event(&self, event: AgentEvent, signal: AbortSignal);
}

type ListenerRegistry = Arc<Mutex<Vec<(u64, Arc<dyn AgentEventListener>)>>>;

/// Handle returned by [`Agent::subscribe`].
pub struct Subscription {
    listeners: ListenerRegistry,
    id: u64,
}

impl Subscription {
    pub fn unsubscribe(self) {
        self.listeners.lock().retain(|(id, _)| *id != self.id);
    }
}

/// Options for constructing an [`Agent`].
#[derive(Clone, Default)]
pub struct AgentOptions {
    pub initial_state: Option<InitialAgentState>,
    pub convert_to_llm: Option<Arc<dyn MessageConverter>>,
    pub transform_context: Option<Arc<dyn ContextTransform>>,
    /// `None` falls back to the process default installed with
    /// [`crate::stream_fn::set_default_stream_fn`].
    pub stream_fn: Option<StreamFn>,
    pub get_api_key: Option<Arc<dyn ApiKeyProvider>>,
    pub before_tool_call: Option<Arc<dyn BeforeToolCall>>,
    pub after_tool_call: Option<Arc<dyn AfterToolCall>>,
    pub should_stop_after_turn: Option<Arc<dyn ShouldStopAfterTurn>>,
    pub prepare_next_turn: Option<Arc<dyn PrepareNextTurn>>,
    pub steering_mode: Option<QueueMode>,
    pub follow_up_mode: Option<QueueMode>,
    /// Forwarded to providers for cache-aware backends.
    pub session_id: Option<String>,
    pub thinking_budgets: Option<pi_core::ThinkingBudgets>,
    pub transport: Option<pi_core::Transport>,
    pub max_retry_delay_ms: Option<u64>,
    pub tool_execution: Option<ToolExecutionMode>,
}

/// The mutable slice of [`AgentState`] a caller may seed.
#[derive(Clone, Default)]
pub struct InitialAgentState {
    pub system_prompt: Option<String>,
    pub model: Option<Model>,
    pub thinking_level: Option<ModelThinkingLevel>,
    pub tools: Option<Vec<AgentToolRef>>,
    /// Filesystem and shell the tools run against. Defaults to an empty
    /// in-memory environment; supply a `pi_tools::LocalExecutionEnv` to let the
    /// built-in tools touch the real filesystem.
    pub env: Option<ExecutionEnvRef>,
    pub messages: Option<Vec<AgentMessage>>,
}

/// FIFO queue of user messages with a [`QueueMode`] drain policy.
struct PendingMessageQueue {
    messages: Vec<AgentMessage>,
    mode: QueueMode,
}

impl PendingMessageQueue {
    fn new(mode: QueueMode) -> Self {
        Self {
            messages: Vec::new(),
            mode,
        }
    }

    fn enqueue(&mut self, message: AgentMessage) {
        self.messages.push(message);
    }

    fn has_items(&self) -> bool {
        !self.messages.is_empty()
    }

    fn drain(&mut self) -> Vec<AgentMessage> {
        match self.mode {
            QueueMode::All => std::mem::take(&mut self.messages),
            QueueMode::OneAtATime => {
                if self.messages.is_empty() {
                    Vec::new()
                } else {
                    vec![self.messages.remove(0)]
                }
            }
        }
    }

    fn clear(&mut self) {
        self.messages.clear();
    }
}

struct ActiveRun {
    abort: Arc<AbortHandle>,
    signal: AbortSignal,
}

struct AgentInner {
    state: Mutex<AgentState>,
    listeners: ListenerRegistry,
    next_listener_id: AtomicU64,
    steering: Mutex<PendingMessageQueue>,
    follow_up: Mutex<PendingMessageQueue>,
    active_run: Mutex<Option<ActiveRun>>,
    /// `true` while a run is in flight. Drives [`Agent::wait_for_idle`].
    running: tokio::sync::watch::Sender<bool>,
    options: Mutex<AgentOptions>,
}

/// Stateful agent: transcript, lifecycle events, tool execution, queueing.
#[derive(Clone)]
pub struct Agent {
    inner: Arc<AgentInner>,
}

impl Agent {
    pub fn new(options: AgentOptions) -> Self {
        let initial = options.initial_state.clone().unwrap_or_default();
        let state = AgentState {
            system_prompt: initial.system_prompt.unwrap_or_default(),
            model: initial.model.unwrap_or_else(crate::types::default_model),
            thinking_level: initial.thinking_level.unwrap_or(ModelThinkingLevel::Off),
            tools: initial.tools.unwrap_or_default(),
            env: initial
                .env
                .unwrap_or_else(crate::types::default_execution_env),
            messages: initial.messages.unwrap_or_default(),
            ..AgentState::default()
        };
        let (running, _) = tokio::sync::watch::channel(false);
        let steering = PendingMessageQueue::new(options.steering_mode.unwrap_or_default());
        let follow_up = PendingMessageQueue::new(options.follow_up_mode.unwrap_or_default());

        Self {
            inner: Arc::new(AgentInner {
                state: Mutex::new(state),
                listeners: Arc::new(Mutex::new(Vec::new())),
                next_listener_id: AtomicU64::new(0),
                steering: Mutex::new(steering),
                follow_up: Mutex::new(follow_up),
                active_run: Mutex::new(None),
                running,
                options: Mutex::new(options),
            }),
        }
    }

    // --- subscriptions -----------------------------------------------------

    pub fn subscribe(&self, listener: Arc<dyn AgentEventListener>) -> Subscription {
        let id = self.inner.next_listener_id.fetch_add(1, Ordering::Relaxed);
        self.inner.listeners.lock().push((id, listener));
        Subscription {
            listeners: self.inner.listeners.clone(),
            id,
        }
    }

    // --- state -------------------------------------------------------------

    /// Snapshot of the current state.
    pub fn state(&self) -> AgentState {
        self.inner.state.lock().clone()
    }

    pub fn messages(&self) -> Vec<AgentMessage> {
        self.inner.state.lock().messages.clone()
    }

    pub fn set_messages(&self, messages: Vec<AgentMessage>) {
        self.inner.state.lock().messages = messages;
    }

    pub fn push_message(&self, message: AgentMessage) {
        self.inner.state.lock().messages.push(message);
    }

    pub fn set_system_prompt(&self, prompt: impl Into<String>) {
        self.inner.state.lock().system_prompt = prompt.into();
    }

    pub fn set_model(&self, model: Model) {
        self.inner.state.lock().model = model;
    }

    pub fn set_thinking_level(&self, level: ModelThinkingLevel) {
        self.inner.state.lock().thinking_level = level;
    }

    pub fn set_tools(&self, tools: Vec<AgentToolRef>) {
        self.inner.state.lock().tools = tools;
    }

    /// Replace the execution environment handed to every tool call.
    pub fn set_env(&self, env: ExecutionEnvRef) {
        self.inner.state.lock().env = env;
    }

    pub fn env(&self) -> ExecutionEnvRef {
        self.inner.state.lock().env.clone()
    }

    pub fn is_streaming(&self) -> bool {
        self.inner.state.lock().is_streaming
    }

    pub fn session_id(&self) -> Option<String> {
        self.inner.options.lock().session_id.clone()
    }

    pub fn set_session_id(&self, session_id: Option<String>) {
        self.inner.options.lock().session_id = session_id;
    }

    pub fn set_tool_execution(&self, mode: ToolExecutionMode) {
        self.inner.options.lock().tool_execution = Some(mode);
    }

    // --- queues ------------------------------------------------------------

    /// Queue a message injected after the current assistant turn finishes.
    pub fn steer(&self, message: AgentMessage) {
        self.inner.steering.lock().enqueue(message);
    }

    /// Queue a message that runs only once the agent would otherwise stop.
    pub fn follow_up(&self, message: AgentMessage) {
        self.inner.follow_up.lock().enqueue(message);
    }

    pub fn set_steering_mode(&self, mode: QueueMode) {
        self.inner.steering.lock().mode = mode;
    }

    pub fn steering_mode(&self) -> QueueMode {
        self.inner.steering.lock().mode
    }

    pub fn set_follow_up_mode(&self, mode: QueueMode) {
        self.inner.follow_up.lock().mode = mode;
    }

    pub fn follow_up_mode(&self) -> QueueMode {
        self.inner.follow_up.lock().mode
    }

    pub fn clear_steering_queue(&self) {
        self.inner.steering.lock().clear();
    }

    pub fn clear_follow_up_queue(&self) {
        self.inner.follow_up.lock().clear();
    }

    pub fn clear_all_queues(&self) {
        self.clear_steering_queue();
        self.clear_follow_up_queue();
    }

    pub fn has_queued_messages(&self) -> bool {
        self.inner.steering.lock().has_items() || self.inner.follow_up.lock().has_items()
    }

    // --- lifecycle ---------------------------------------------------------

    /// Active abort signal for the current run, if any.
    pub fn signal(&self) -> Option<AbortSignal> {
        self.inner
            .active_run
            .lock()
            .as_ref()
            .map(|r| r.signal.clone())
    }

    /// Abort the current run, if one is active.
    pub fn abort(&self) {
        if let Some(run) = self.inner.active_run.lock().as_ref() {
            run.abort.abort();
        }
    }

    /// Resolve once the current run and all awaited listeners have finished.
    pub async fn wait_for_idle(&self) {
        let mut rx = self.inner.running.subscribe();
        while *rx.borrow() {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    /// Clear transcript, runtime state and queued messages.
    pub fn reset(&self) -> Result<(), AgentError> {
        if self.inner.active_run.lock().is_some() {
            return Err(AgentError::invalid_state(
                "Agent is already processing. Wait for completion before resetting.",
            ));
        }
        {
            let mut state = self.inner.state.lock();
            state.messages.clear();
            state.is_streaming = false;
            state.streaming_message = None;
            state.pending_tool_calls = BTreeSet::new();
            state.error_message = None;
        }
        self.clear_all_queues();
        Ok(())
    }

    /// Start a new prompt from plain text plus optional images.
    pub async fn prompt_text(
        &self,
        input: impl Into<String>,
        images: Vec<ImageContent>,
    ) -> Result<(), AgentError> {
        let mut content = vec![InputContent::text(input)];
        content.extend(images.into_iter().map(InputContent::Image));
        self.prompt(vec![AgentMessage::User(pi_core::UserMessage {
            content: pi_core::UserContent::Blocks(content),
            timestamp: pi_core::now_ms(),
        })])
        .await
    }

    /// Start a new prompt from a batch of messages.
    pub async fn prompt(&self, messages: Vec<AgentMessage>) -> Result<(), AgentError> {
        if self.inner.active_run.lock().is_some() {
            return Err(AgentError::invalid_state(
                "Agent is already processing a prompt. Use steer() or followUp() to queue messages, or wait for completion.",
            ));
        }
        self.run_prompt_messages(messages, false).await
    }

    /// Continue from the current transcript.
    ///
    /// When the transcript ends in an assistant message, queued steering (then
    /// follow-up) messages are drained and run as a prompt instead.
    pub async fn continue_run(&self) -> Result<(), AgentError> {
        if self.inner.active_run.lock().is_some() {
            return Err(AgentError::invalid_state(
                "Agent is already processing. Wait for completion before continuing.",
            ));
        }

        let last = self.inner.state.lock().messages.last().cloned();
        let Some(last) = last else {
            return Err(AgentError::invalid_state("No messages to continue from"));
        };

        if matches!(last, AgentMessage::Assistant(_)) {
            let steering = self.inner.steering.lock().drain();
            if !steering.is_empty() {
                // The loop's first steering poll would otherwise immediately
                // re-drain and double-inject; skip it for this run.
                return self.run_prompt_messages(steering, true).await;
            }
            let follow_ups = self.inner.follow_up.lock().drain();
            if !follow_ups.is_empty() {
                return self.run_prompt_messages(follow_ups, false).await;
            }
            return Err(AgentError::invalid_state(
                "Cannot continue from message role: assistant",
            ));
        }

        self.run_continuation().await
    }

    async fn run_prompt_messages(
        &self,
        messages: Vec<AgentMessage>,
        skip_initial_steering_poll: bool,
    ) -> Result<(), AgentError> {
        let agent = self.clone();
        self.run_with_lifecycle(move |signal, sink| {
            let context = agent.context_snapshot();
            let stream_fn = resolve_stream_fn(agent.inner.options.lock().stream_fn.clone());
            let config = agent.loop_config(skip_initial_steering_poll);
            Box::pin(async move {
                let stream_fn = stream_fn?;
                run_agent_loop(messages, context, config, sink, Some(signal), stream_fn)
                    .await
                    .map(|_| ())
            })
        })
        .await
    }

    async fn run_continuation(&self) -> Result<(), AgentError> {
        let agent = self.clone();
        self.run_with_lifecycle(move |signal, sink| {
            let context = agent.context_snapshot();
            let stream_fn = resolve_stream_fn(agent.inner.options.lock().stream_fn.clone());
            let config = agent.loop_config(false);
            Box::pin(async move {
                let stream_fn = stream_fn?;
                run_agent_loop_continue(context, config, sink, Some(signal), stream_fn)
                    .await
                    .map(|_| ())
            })
        })
        .await
    }

    fn context_snapshot(&self) -> AgentContext {
        let state = self.inner.state.lock();
        AgentContext {
            system_prompt: state.system_prompt.clone(),
            messages: state.messages.clone(),
            tools: state.tools.clone(),
            env: state.env.clone(),
        }
    }

    fn loop_config(&self, skip_initial_steering_poll: bool) -> AgentLoopConfig {
        let options = self.inner.options.lock().clone();
        let state = self.inner.state.lock();

        let mut config = AgentLoopConfig::new(state.model.clone());
        config.set_thinking_level(state.thinking_level);
        config.stream_options.thinking_budgets = options.thinking_budgets.clone();
        config.stream_options.stream.session_id = options.session_id.clone();
        config.stream_options.stream.transport = options.transport;
        config.stream_options.stream.request.max_retry_delay_ms = options.max_retry_delay_ms;
        config.tool_execution = options
            .tool_execution
            .unwrap_or(crate::types::DEFAULT_TOOL_EXECUTION);
        config.convert_to_llm = options
            .convert_to_llm
            .clone()
            .unwrap_or_else(|| Arc::new(DefaultMessageConverter));
        config.transform_context = options.transform_context.clone();
        config.get_api_key = options.get_api_key.clone();
        config.before_tool_call = options.before_tool_call.clone();
        config.after_tool_call = options.after_tool_call.clone();
        config.should_stop_after_turn = options.should_stop_after_turn.clone();
        config.prepare_next_turn = options.prepare_next_turn.clone();
        config.get_steering_messages = Some(Arc::new(QueueSource {
            agent: self.inner.clone(),
            follow_up: false,
            skip_first: Mutex::new(skip_initial_steering_poll),
        }));
        config.get_follow_up_messages = Some(Arc::new(QueueSource {
            agent: self.inner.clone(),
            follow_up: true,
            skip_first: Mutex::new(false),
        }));
        config
    }

    async fn run_with_lifecycle<F>(&self, executor: F) -> Result<(), AgentError>
    where
        F: FnOnce(
            AbortSignal,
            AgentEventSink,
        ) -> futures::future::BoxFuture<'static, Result<(), AgentError>>,
    {
        let (handle, signal) = AbortHandle::new();
        {
            let mut active = self.inner.active_run.lock();
            if active.is_some() {
                return Err(AgentError::invalid_state("Agent is already processing."));
            }
            *active = Some(ActiveRun {
                abort: Arc::new(handle),
                signal: signal.clone(),
            });
        }
        self.inner.running.send_replace(true);

        {
            let mut state = self.inner.state.lock();
            state.is_streaming = true;
            state.streaming_message = None;
            state.error_message = None;
        }

        let agent = self.clone();
        let sink: AgentEventSink = Arc::new(move |event| {
            let agent = agent.clone();
            Box::pin(async move { agent.process_event(event).await })
        });

        let outcome = executor(signal, sink).await;
        if let Err(error) = outcome {
            self.handle_run_failure(&error).await;
        }
        self.finish_run();
        Ok(())
    }

    /// Emit the full failure lifecycle upstream produces when the loop throws.
    async fn handle_run_failure(&self, error: &AgentError) {
        let aborted = self
            .inner
            .active_run
            .lock()
            .as_ref()
            .is_some_and(|r| r.signal.is_aborted());
        let model = self.inner.state.lock().model.clone();
        let failure = AgentMessage::Assistant(AssistantMessage {
            content: vec![pi_core::AssistantContent::text("")],
            api: model.api.as_str().to_string(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: if aborted {
                StopReason::Aborted
            } else {
                StopReason::Error
            },
            deferred: None,
            error_message: Some(error.message()),
            raw_stop_reason: None,
            end_turn: None,
            timestamp: pi_core::now_ms(),
        });

        self.process_event(AgentEvent::MessageStart {
            message: failure.clone(),
        })
        .await;
        self.process_event(AgentEvent::MessageEnd {
            message: failure.clone(),
        })
        .await;
        self.process_event(AgentEvent::TurnEnd {
            message: failure.clone(),
            tool_results: Vec::new(),
        })
        .await;
        self.process_event(AgentEvent::AgentEnd {
            messages: vec![failure],
        })
        .await;
    }

    fn finish_run(&self) {
        {
            let mut state = self.inner.state.lock();
            state.is_streaming = false;
            state.streaming_message = None;
            state.pending_tool_calls = BTreeSet::new();
        }
        *self.inner.active_run.lock() = None;
        self.inner.running.send_replace(false);
    }

    /// Reduce internal state for a loop event, then await listeners.
    ///
    /// `agent_end` only means no further loop events arrive; the run settles
    /// after its listeners finish and `finish_run` clears runtime state.
    async fn process_event(&self, event: AgentEvent) {
        {
            let mut state = self.inner.state.lock();
            match &event {
                AgentEvent::MessageStart { message }
                | AgentEvent::MessageUpdate { message, .. } => {
                    state.streaming_message = Some(message.clone());
                }
                AgentEvent::MessageEnd { message } => {
                    state.streaming_message = None;
                    state.messages.push(message.clone());
                }
                AgentEvent::ToolExecutionStart { tool_call_id, .. } => {
                    state.pending_tool_calls.insert(tool_call_id.clone());
                }
                AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
                    state.pending_tool_calls.remove(tool_call_id);
                }
                AgentEvent::TurnEnd { message, .. } => {
                    if let Some(assistant) = message.as_assistant() {
                        if let Some(error) = &assistant.error_message {
                            state.error_message = Some(error.clone());
                        }
                    }
                }
                AgentEvent::AgentEnd { .. } => {
                    state.streaming_message = None;
                }
                _ => {}
            }
        }

        let signal = self
            .inner
            .active_run
            .lock()
            .as_ref()
            .map(|r| r.signal.clone());
        let Some(signal) = signal else {
            // Upstream throws here; the port drops the event instead rather than
            // panicking across the FFI boundary.
            return;
        };
        let listeners: Vec<Arc<dyn AgentEventListener>> = self
            .inner
            .listeners
            .lock()
            .iter()
            .map(|(_, l)| l.clone())
            .collect();
        for listener in listeners {
            listener.on_event(event.clone(), signal.clone()).await;
        }
    }
}

/// Bridges an [`Agent`] queue to the loop's [`MessageSource`] hook.
struct QueueSource {
    agent: Arc<AgentInner>,
    follow_up: bool,
    skip_first: Mutex<bool>,
}

#[async_trait]
impl MessageSource for QueueSource {
    async fn take_messages(&self) -> Vec<AgentMessage> {
        {
            let mut skip = self.skip_first.lock();
            if *skip {
                *skip = false;
                return Vec::new();
            }
        }
        if self.follow_up {
            self.agent.follow_up.lock().drain()
        } else {
            self.agent.steering.lock().drain()
        }
    }
}

/// Adapter so a plain async closure can serve as a [`PrepareNextTurn`] hook
/// without the caller writing an impl block. Kept because upstream's
/// `prepareNextTurn` (the no-context variant) is a common shape.
pub struct FnPrepareNextTurn<F>(pub F);

#[async_trait]
impl<F, Fut> PrepareNextTurn for FnPrepareNextTurn<F>
where
    F: Fn(TurnContext) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Option<crate::types::AgentLoopTurnUpdate>> + Send + 'static,
{
    async fn prepare_next_turn(
        &self,
        context: TurnContext,
    ) -> Option<crate::types::AgentLoopTurnUpdate> {
        (self.0)(context).await
    }
}
