//! Port of `packages/agent/src/types.ts`.
//!
//! Every extension point upstream expresses as a callback is an object-safe
//! `Arc<dyn Trait>` here, because the SDK is consumed from Swift over FFI and a
//! Swift caller cannot hand Rust a monomorphised closure. The event output is a
//! channel of serializable [`AgentEvent`]s for the same reason.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{
    AbortSignal, AssistantMessage, AssistantMessageEvent, InputContent, Message, Model,
    ModelThinkingLevel, ToolResultMessage, Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AgentError;

/// The tool contract, owned by `pi-tools`.
///
/// Upstream splits this in two — `AgentTool` in `agent/src/types.ts` and
/// `AgentHarnessTool` in `harness/types.ts`, differing only in the
/// application-defined context argument. `pi-tools` collapses them into one
/// object-safe trait whose context carries an [`ExecutionEnvRef`], and this
/// crate re-exports rather than redefines it: two tool traits either side of
/// the `pi-agent`/`pi-tools` boundary would make a Swift host choose which one
/// its custom tools implement.
pub use pi_tools::{
    AgentTool, AgentToolRef, ExecutionEnvRef, ToolContext, ToolExecutionMode, ToolResult,
    ToolUpdateCallback,
};

/// How many queued user messages are injected at a queue drain point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueueMode {
    /// Drain and inject every queued message.
    All,
    /// Drain and inject only the oldest queued message.
    #[default]
    OneAtATime,
}

/// The message union the agent works with: the LLM messages plus the harness's
/// application-level message kinds.
///
/// Defined in `pi-session`, which owns the durable format it is persisted in;
/// re-exported here so embedders need only depend on `pi-agent`.
pub use pi_session::messages::AgentMessage;

/// The loop's default tool-execution mode.
///
/// `pi_tools::ToolExecutionMode` has no `Default` (a tool's own mode is an
/// override, where "unset" is meaningful), but the loop needs one.
pub const DEFAULT_TOOL_EXECUTION: ToolExecutionMode = ToolExecutionMode::Parallel;

/// Context snapshot passed into the low-level agent loop.
#[derive(Clone)]
pub struct AgentContext {
    /// System prompt included with the request.
    pub system_prompt: String,
    /// Transcript visible to the model.
    pub messages: Vec<AgentMessage>,
    /// Tools available for this run.
    pub tools: Vec<AgentToolRef>,
    /// Filesystem and shell the tools run against.
    ///
    /// Upstream's `AgentHarness` resolves an application-defined tool context
    /// per turn snapshot (`AgentHarnessToolContextSource`); `pi-tools` fixes
    /// that context to an [`ExecutionEnvRef`], so the loop carries one here and
    /// builds a [`ToolContext`] per call from it.
    pub env: ExecutionEnvRef,
}

impl Default for AgentContext {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            env: default_execution_env(),
        }
    }
}

/// An empty in-memory environment, for agents whose tools do not touch a real
/// filesystem. Callers that use the built-in tools must supply a
/// [`pi_tools::LocalExecutionEnv`] instead.
pub fn default_execution_env() -> ExecutionEnvRef {
    Arc::new(pi_tools::MemoryExecutionEnv::new("/"))
}

impl AgentContext {
    pub fn find_tool(&self, name: &str) -> Option<&AgentToolRef> {
        self.tools.iter().find(|t| t.name() == name)
    }
}

impl fmt::Debug for AgentContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentContext")
            .field("system_prompt", &self.system_prompt)
            .field("messages", &self.messages.len())
            .field(
                "tools",
                &self.tools.iter().map(|t| t.name()).collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// Events emitted by the agent loop for UI updates.
///
/// `AgentEnd` is the last event of a run, but awaited subscribers for it are
/// still part of run settlement: the agent becomes idle only afterwards.
// A lifecycle union whose payload-carrying arms hold whole messages while
// `AgentStart`/`TurnStart` hold nothing: the size spread is intrinsic. Boxing
// would add an allocation per event on the hot streaming path for no benefit.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AgentEvent {
    // --- agent lifecycle ---
    AgentStart,
    AgentEnd {
        messages: Vec<AgentMessage>,
    },
    // --- turn lifecycle: one assistant response plus its tool calls/results ---
    TurnStart,
    TurnEnd {
        message: AgentMessage,
        tool_results: Vec<ToolResultMessage>,
    },
    // --- message lifecycle ---
    MessageStart {
        message: AgentMessage,
    },
    /// Only emitted for assistant messages during streaming.
    MessageUpdate {
        message: AgentMessage,
        assistant_message_event: AssistantMessageEvent,
    },
    MessageEnd {
        message: AgentMessage,
    },
    // --- tool execution lifecycle ---
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: Value,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        args: Value,
        partial_result: ToolResult,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: ToolResult,
        is_error: bool,
    },
}

impl AgentEvent {
    /// Stable discriminator, matching the TypeScript `type` field.
    pub fn kind(&self) -> &'static str {
        match self {
            AgentEvent::AgentStart => "agent_start",
            AgentEvent::AgentEnd { .. } => "agent_end",
            AgentEvent::TurnStart => "turn_start",
            AgentEvent::TurnEnd { .. } => "turn_end",
            AgentEvent::MessageStart { .. } => "message_start",
            AgentEvent::MessageUpdate { .. } => "message_update",
            AgentEvent::MessageEnd { .. } => "message_end",
            AgentEvent::ToolExecutionStart { .. } => "tool_execution_start",
            AgentEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
            AgentEvent::ToolExecutionEnd { .. } => "tool_execution_end",
        }
    }
}

/// Async sink the loop pushes [`AgentEvent`]s into. Awaited, so a subscriber can
/// backpressure the loop exactly like upstream's awaited listeners.
pub type AgentEventSink =
    Arc<dyn Fn(AgentEvent) -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

// ---------------------------------------------------------------------------
// Hook contracts
// ---------------------------------------------------------------------------

/// Result returned from [`BeforeToolCall`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeToolCallResult {
    /// Prevent the tool from executing. The loop emits an error tool result instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<bool>,
    /// Text shown in that error result. Defaults to `"Tool execution was blocked"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Participate in the batch early-termination rule when blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
    /// Replacement arguments, executed **without** revalidation.
    ///
    /// Port deviation: upstream mutates `context.args` in place (there is a test
    /// asserting mutated args reach `execute` unvalidated). Rust hands the hook
    /// an owned value, so the replacement comes back through this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
}

/// Partial override returned from [`AfterToolCall`].
///
/// Merge semantics are field-by-field and **replace wholesale**: `content`,
/// `details`, `is_error`, `usage` and `terminate` each replace the executed
/// value when present. Omitted fields keep the original. There is no deep merge.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AfterToolCallResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<InputContent>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

/// Context passed to [`BeforeToolCall`].
#[derive(Debug, Clone)]
pub struct BeforeToolCallContext {
    pub assistant_message: AssistantMessage,
    pub tool_call: pi_core::ToolCall,
    /// Validated tool arguments.
    pub args: Value,
    pub context: AgentContext,
}

/// Context passed to [`AfterToolCall`].
#[derive(Debug, Clone)]
pub struct AfterToolCallContext {
    pub assistant_message: AssistantMessage,
    pub tool_call: pi_core::ToolCall,
    pub args: Value,
    /// The executed result before any overrides.
    pub result: ToolResult,
    /// Whether the executed result is currently treated as an error.
    pub is_error: bool,
    pub context: AgentContext,
}

/// Context passed to [`ShouldStopAfterTurn`] and [`PrepareNextTurn`].
#[derive(Debug, Clone)]
pub struct TurnContext {
    /// The assistant message that completed the turn.
    pub message: AgentMessage,
    /// Tool results passed to the preceding `turn_end` event.
    pub tool_results: Vec<ToolResultMessage>,
    /// Agent context after the turn's messages were appended.
    pub context: AgentContext,
    /// Messages this loop invocation returns if it exits here.
    pub new_messages: Vec<AgentMessage>,
}

/// Replacement runtime state used before starting another provider request.
#[derive(Debug, Clone, Default)]
pub struct AgentLoopTurnUpdate {
    pub context: Option<AgentContext>,
    pub model: Option<Model>,
    pub thinking_level: Option<ModelThinkingLevel>,
}

/// Converts [`AgentMessage`]s to LLM-compatible [`Message`]s before each call.
///
/// Contract: must not fail. Return a safe fallback instead.
#[async_trait]
pub trait MessageConverter: Send + Sync + 'static {
    async fn convert(&self, messages: &[AgentMessage]) -> Vec<Message>;
}

/// Transform applied to the context before [`MessageConverter`].
#[async_trait]
pub trait ContextTransform: Send + Sync + 'static {
    async fn transform(
        &self,
        messages: Vec<AgentMessage>,
        signal: Option<AbortSignal>,
    ) -> Vec<AgentMessage>;
}

/// Resolves an API key per LLM call, for short-lived OAuth tokens.
#[async_trait]
pub trait ApiKeyProvider: Send + Sync + 'static {
    async fn api_key(&self, provider: &str) -> Option<String>;
}

/// Called before a tool executes, after arguments have been validated.
#[async_trait]
pub trait BeforeToolCall: Send + Sync + 'static {
    async fn before_tool_call(
        &self,
        context: BeforeToolCallContext,
        signal: Option<AbortSignal>,
    ) -> Option<BeforeToolCallResult>;
}

/// Called after a tool finishes, before `tool_execution_end` is emitted.
///
/// Returning `Err` replaces the result with an error tool result carrying the
/// error message, matching upstream's `catch` around the hook.
#[async_trait]
pub trait AfterToolCall: Send + Sync + 'static {
    async fn after_tool_call(
        &self,
        context: AfterToolCallContext,
        signal: Option<AbortSignal>,
    ) -> Result<Option<AfterToolCallResult>, AgentError>;
}

/// Called after each turn completes and `turn_end` has been emitted.
#[async_trait]
pub trait ShouldStopAfterTurn: Send + Sync + 'static {
    async fn should_stop_after_turn(&self, context: TurnContext) -> bool;
}

/// Called after `turn_end`, before deciding whether another request starts.
#[async_trait]
pub trait PrepareNextTurn: Send + Sync + 'static {
    async fn prepare_next_turn(&self, context: TurnContext) -> Option<AgentLoopTurnUpdate>;
}

/// Supplies steering or follow-up messages at a queue drain point.
#[async_trait]
pub trait MessageSource: Send + Sync + 'static {
    async fn take_messages(&self) -> Vec<AgentMessage>;
}

/// The default converter: keep the three LLM roles, drop everything else.
pub struct DefaultMessageConverter;

#[async_trait]
impl MessageConverter for DefaultMessageConverter {
    async fn convert(&self, messages: &[AgentMessage]) -> Vec<Message> {
        messages.iter().filter_map(|m| m.as_llm_message()).collect()
    }
}

/// Configuration for one agent-loop invocation.
///
/// Upstream's `AgentLoopConfig extends SimpleStreamOptions`; the port nests the
/// provider options instead of flattening them, because `SimpleStreamOptions`
/// belongs to `pi-core` and must not grow agent fields.
#[derive(Clone)]
pub struct AgentLoopConfig {
    pub model: Model,
    /// Provider options forwarded to the stream function. `signal` and
    /// `api_key` are overwritten per request by the loop.
    pub stream_options: pi_core::SimpleStreamOptions,
    pub tool_execution: ToolExecutionMode,
    pub convert_to_llm: Arc<dyn MessageConverter>,
    pub transform_context: Option<Arc<dyn ContextTransform>>,
    pub get_api_key: Option<Arc<dyn ApiKeyProvider>>,
    pub before_tool_call: Option<Arc<dyn BeforeToolCall>>,
    pub after_tool_call: Option<Arc<dyn AfterToolCall>>,
    pub should_stop_after_turn: Option<Arc<dyn ShouldStopAfterTurn>>,
    pub prepare_next_turn: Option<Arc<dyn PrepareNextTurn>>,
    pub get_steering_messages: Option<Arc<dyn MessageSource>>,
    pub get_follow_up_messages: Option<Arc<dyn MessageSource>>,
}

impl AgentLoopConfig {
    pub fn new(model: Model) -> Self {
        Self {
            model,
            stream_options: pi_core::SimpleStreamOptions::default(),
            tool_execution: DEFAULT_TOOL_EXECUTION,
            convert_to_llm: Arc::new(DefaultMessageConverter),
            transform_context: None,
            get_api_key: None,
            before_tool_call: None,
            after_tool_call: None,
            should_stop_after_turn: None,
            prepare_next_turn: None,
            get_steering_messages: None,
            get_follow_up_messages: None,
        }
    }

    /// Set the reasoning level, mapping `Off` onto "no reasoning" the way
    /// upstream does when it copies `thinkingLevel` into `reasoning`.
    pub fn set_thinking_level(&mut self, level: ModelThinkingLevel) {
        self.stream_options.reasoning = level.level();
    }
}

impl fmt::Debug for AgentLoopConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentLoopConfig")
            .field("model", &self.model.id)
            .field("tool_execution", &self.tool_execution)
            .finish_non_exhaustive()
    }
}

/// Public snapshot of [`crate::Agent`] state.
#[derive(Clone)]
pub struct AgentState {
    pub system_prompt: String,
    pub model: Model,
    pub thinking_level: ModelThinkingLevel,
    pub tools: Vec<AgentToolRef>,
    /// Filesystem and shell handed to every tool call.
    pub env: ExecutionEnvRef,
    pub messages: Vec<AgentMessage>,
    /// True while the agent is processing a prompt or continuation. Stays true
    /// until awaited `agent_end` listeners settle.
    pub is_streaming: bool,
    /// Partial assistant message for the current streamed response.
    pub streaming_message: Option<AgentMessage>,
    /// Tool call ids currently executing.
    pub pending_tool_calls: BTreeSet<String>,
    /// Error message from the most recent failed or aborted assistant turn.
    pub error_message: Option<String>,
}

impl fmt::Debug for AgentState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentState")
            .field("system_prompt", &self.system_prompt)
            .field("model", &self.model.id)
            .field("thinking_level", &self.thinking_level)
            .field(
                "tools",
                &self.tools.iter().map(|t| t.name()).collect::<Vec<_>>(),
            )
            .field("messages", &self.messages.len())
            .field("is_streaming", &self.is_streaming)
            .field("pending_tool_calls", &self.pending_tool_calls)
            .field("error_message", &self.error_message)
            .finish()
    }
}

/// The placeholder model upstream uses when a caller supplies none.
pub fn default_model() -> Model {
    Model {
        id: "unknown".into(),
        name: "unknown".into(),
        api: pi_core::Api::Custom("unknown".into()),
        provider: "unknown".into(),
        base_url: String::new(),
        reasoning: false,
        thinking_level_map: None,
        input: Vec::new(),
        cost: pi_core::ModelCost::default(),
        context_window: 0,
        max_tokens: 0,
        sampling_params: None,
        headers: None,
        compat: None,
    }
}

impl Default for AgentState {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            model: default_model(),
            thinking_level: ModelThinkingLevel::Off,
            tools: Vec::new(),
            env: default_execution_env(),
            messages: Vec::new(),
            is_streaming: false,
            streaming_message: None,
            pending_tool_calls: BTreeSet::new(),
            error_message: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agent_event_matches_the_typescript_wire_shape() {
        let event = AgentEvent::ToolExecutionEnd {
            tool_call_id: "call-1".into(),
            tool_name: "echo".into(),
            result: ToolResult::text("ok"),
            is_error: false,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "tool_execution_end");
        assert_eq!(value["toolCallId"], "call-1");
        assert_eq!(value["toolName"], "echo");
        assert_eq!(value["isError"], false);
        assert_eq!(value["result"]["content"][0]["type"], "text");

        let back: AgentEvent = serde_json::from_value(value).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn unit_events_serialize_as_bare_type_tags() {
        assert_eq!(
            serde_json::to_value(AgentEvent::AgentStart).unwrap(),
            json!({"type": "agent_start"})
        );
        assert_eq!(
            serde_json::to_value(AgentEvent::TurnStart).unwrap(),
            json!({"type": "turn_start"})
        );
    }

    #[test]
    fn queue_and_execution_modes_use_the_typescript_spellings() {
        assert_eq!(
            serde_json::to_value(QueueMode::OneAtATime).unwrap(),
            json!("one-at-a-time")
        );
        assert_eq!(serde_json::to_value(QueueMode::All).unwrap(), json!("all"));
        assert_eq!(
            serde_json::to_value(ToolExecutionMode::Sequential).unwrap(),
            json!("sequential")
        );
    }
}
