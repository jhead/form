//! The agent loop: turn execution, tool dispatch, hooks, queueing and the
//! harness pieces that tie sessions, tools and providers together.
//!
//! Port of `.upstream/packages/agent/src/`. See `AGENTS.md` for the FFI-shaped
//! conventions every public type here follows:
//!
//! - Extension points are object-safe traits behind `Arc<dyn Trait>`
//!   ([`AgentTool`], [`BeforeToolCall`], [`AfterToolCall`], ...), never generic
//!   bounds or closures with type parameters.
//! - The event output crosses the boundary as a channel of serializable
//!   [`AgentEvent`]s ([`AgentRun`]), or as an awaited [`AgentEventSink`] when
//!   the caller needs backpressure.
//! - Every long-running call takes an `Option<AbortSignal>`.
//!
//! # Not yet ported
//!
//! These upstream modules depend on crates that are still empty at the time of
//! writing and are deliberately absent rather than stubbed:
//!
//! - `harness/agent-harness.ts` and `harness/reducer.ts` — need `pi-session`'s
//!   session state, `SessionRepo` and `EntryStore` (W10).
//! - `harness/compaction/` — assigned to W10.
//! - `proxy.ts` — a transport, not part of the loop contract.

pub mod agent;
pub mod agent_loop;
pub mod error;
pub mod harness;
pub mod stream_fn;
pub mod testing;
pub mod types;

pub use agent::{Agent, AgentEventListener, AgentOptions, InitialAgentState, Subscription};
pub use agent_loop::{
    agent_loop, agent_loop_continue, run_agent_loop, run_agent_loop_continue, AgentRun,
};
pub use error::AgentError;
pub use stream_fn::{default_stream_fn, resolve_stream_fn, set_default_stream_fn};
pub use types::{
    default_execution_env, default_model, AfterToolCall, AfterToolCallContext, AfterToolCallResult,
    AgentContext, AgentEvent, AgentEventSink, AgentLoopConfig, AgentLoopTurnUpdate, AgentMessage,
    AgentState, ApiKeyProvider, BeforeToolCall, BeforeToolCallContext, BeforeToolCallResult,
    ContextTransform, DefaultMessageConverter, MessageConverter, MessageSource, PrepareNextTurn,
    QueueMode, ShouldStopAfterTurn, TurnContext, DEFAULT_TOOL_EXECUTION,
};
/// The tool contract lives in `pi-tools`; re-exported so embedders need only
/// depend on `pi-agent`.
pub use types::{
    AgentTool, AgentToolRef, ExecutionEnvRef, ToolContext, ToolExecutionMode, ToolResult,
    ToolUpdateCallback,
};

pub use harness::agent_harness::{
    AgentHarness, AgentHarnessOptions, HarnessError, HarnessResult, HarnessTool, HookName,
    ReplayPolicy,
};
pub use harness::frontmatter::{parse_frontmatter, Frontmatter};
pub use harness::messages::{
    convert_to_llm, BashExecutionMessage, BranchSummaryMessage, CompactionSummaryMessage,
    CustomMessage, HarnessMessageConverter,
};
pub use harness::prompt_templates::{
    format_prompt_template_invocation, load_prompt_templates, load_sourced_prompt_templates,
    parse_command_args, substitute_args, PromptTemplateDiagnostic, PromptTemplateLoadResult,
    PromptTemplateSource,
};
pub use harness::skills::{
    format_skill_invocation, load_skills, load_sourced_skills, SkillDiagnostic, SkillLoadResult,
    SkillSource,
};
pub use harness::summarizer::{stream_fn_summarizer, summary_failed, StreamFnSummarizer};
pub use harness::system_prompt::format_skills_for_system_prompt;
pub use harness::telemetry::{
    agent_span_starter, agent_telemetry_schemas, ai_telemetry_schema, harness_telemetry_schema,
    start_ai_span, start_harness_span,
};
pub use harness::types::{AgentHarnessResources, PromptTemplate, Skill};
