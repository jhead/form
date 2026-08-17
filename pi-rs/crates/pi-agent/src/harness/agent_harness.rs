//! Port of `packages/agent/src/harness/agent-harness.ts`.
//!
//! **Upstream is a scaffold and so is this.** `AgentHarness` there declares the
//! full durable, multi-lane API — runs, compaction, tree navigation, resume,
//! hooks, watches — but every one of those methods currently rejects with
//! `HarnessNotImplemented`; its own test file is `agent-harness-scaffold.test.ts`.
//! What *is* live is the configuration surface (with defensive copies), the
//! leaf-id passthrough, `close()`, and the `create()` gate that refuses a
//! session which already has records because restore is unwritten.
//!
//! The port keeps that shape rather than inventing an implementation, so the
//! declared vocabulary is available to `pi-client`/`pi-server` and the diff
//! against upstream stays honest about what is real.

use std::sync::Arc;

use parking_lot::Mutex;
use pi_core::{Model, ModelThinkingLevel, SimpleStreamOptions, Usage};
use pi_http::RetryPolicy;
use pi_session::compaction::{CompactionSettings, DEFAULT_COMPACTION_SETTINGS};
use pi_session::types::RecordQuery;
use pi_session::Session;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::harness::types::AgentHarnessResources;
use crate::types::{AgentMessage, AgentToolRef, ExecutionEnvRef, QueueMode, ToolExecutionMode};

/// Why a harness call did not succeed.
///
/// Upstream splits these in two: *rejections* come back inside a `Result` value
/// (`LaneBusy`, `UnknownSkill`, …) while *faults* are thrown
/// (`HarnessNotImplemented`, `HarnessClosed`). Rust returns both as `Err`, so
/// the variants stay distinct and [`HarnessError::is_fault`] tells them apart.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HarnessError {
    #[error("AgentHarness.{operation} is not implemented yet")]
    NotImplemented { operation: String },

    #[error("AgentHarness was closed while the operation was active")]
    Closed,

    #[error("{message}")]
    LaneBusy {
        lane: String,
        operation_id: String,
        operation_kind: OperationKind,
        message: String,
    },

    #[error("{message}")]
    MissingIdentities {
        lane: String,
        tools: Vec<String>,
        models: Vec<String>,
        message: String,
    },

    #[error("{message}")]
    NoActiveRun { lane: String, message: String },

    #[error("{message}")]
    NoActiveOperation { lane: String, message: String },

    #[error("{message}")]
    NothingToResume { lane: String, message: String },

    #[error("{message}")]
    InvalidMessage {
        lane: String,
        reason: String,
        message: String,
    },

    #[error("{message}")]
    UnknownSkill { name: String, message: String },

    #[error("{message}")]
    UnknownTemplate { name: String, message: String },

    #[error("{message}")]
    UnknownTarget { target_id: String, message: String },

    #[error("{message}")]
    UnknownQueueItem {
        lane: String,
        entry_id: String,
        message: String,
    },

    #[error("{message}")]
    LaneExists { lane: String, message: String },

    #[error("{message}")]
    InvalidLane {
        lane: String,
        reason: String,
        message: String,
    },

    #[error("{message}")]
    NothingToCompact { lane: String, message: String },

    /// The durable session layer failed. Upstream's `HarnessFault`.
    #[error("{message}")]
    Fault { message: String },
}

impl HarnessError {
    fn not_implemented(operation: &str) -> Self {
        HarnessError::NotImplemented {
            operation: operation.to_string(),
        }
    }

    /// Stable machine-readable code. Do not change these strings.
    pub fn code(&self) -> &'static str {
        match self {
            HarnessError::NotImplemented { .. } => "not_implemented",
            HarnessError::Closed => "closed",
            HarnessError::LaneBusy { .. } => "lane_busy",
            HarnessError::MissingIdentities { .. } => "missing_identities",
            HarnessError::NoActiveRun { .. } => "no_active_run",
            HarnessError::NoActiveOperation { .. } => "no_active_operation",
            HarnessError::NothingToResume { .. } => "nothing_to_resume",
            HarnessError::InvalidMessage { .. } => "invalid_message",
            HarnessError::UnknownSkill { .. } => "unknown_skill",
            HarnessError::UnknownTemplate { .. } => "unknown_template",
            HarnessError::UnknownTarget { .. } => "unknown_target",
            HarnessError::UnknownQueueItem { .. } => "unknown_queue_item",
            HarnessError::LaneExists { .. } => "lane_exists",
            HarnessError::InvalidLane { .. } => "invalid_lane",
            HarnessError::NothingToCompact { .. } => "nothing_to_compact",
            HarnessError::Fault { .. } => "fault",
        }
    }

    /// True for the two upstream *throws* rather than returned rejections.
    pub fn is_fault(&self) -> bool {
        matches!(
            self,
            HarnessError::NotImplemented { .. } | HarnessError::Closed | HarnessError::Fault { .. }
        )
    }
}

pub type HarnessResult<T> = Result<T, HarnessError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Run,
    Compaction,
    Navigation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Running,
    Suspended,
    Aborting,
}

/// Every hook point the durable harness will expose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookName {
    BeforeRun,
    BeforeResume,
    BeforeRunEnd,
    TransformContext,
    BeforeRequest,
    BeforePayload,
    AfterResponse,
    BeforeTool,
    AfterTool,
    BeforeCompaction,
    BeforeNavigation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RunOutcome {
    Completed {
        leaf_id: String,
        final_entry_id: String,
    },
    Aborted {
        leaf_id: String,
        final_entry_id: String,
    },
    Failed {
        leaf_id: String,
        error: OperationError,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        final_entry_id: Option<String>,
    },
    Suspended {
        leaf_id: String,
        final_entry_id: String,
        deferred: pi_core::DeferredHandle,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneOperation {
    pub id: String,
    pub kind: OperationKind,
    pub status: OperationStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneInfo {
    pub name: String,
    pub leaf_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<LaneOperation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedItem {
    pub entry_id: String,
    pub message: AgentMessage,
}

/// A run that was interrupted and can be resumed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuspendedOperation {
    pub lane: String,
    pub kind: OperationKind,
    pub id: String,
    pub started_at: i64,
    pub reason: SuspendReason,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompt: Vec<AgentMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred: Option<pi_core::DeferredHandle>,
    pub missing: MissingIdentities,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuspendReason {
    Crash,
    Deferred,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingIdentities {
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub models: Vec<String>,
}

/// A tool as the durable harness sees it: an [`AgentToolRef`] plus its replay
/// policy, which decides whether a recovered run may re-execute the call.
#[derive(Clone)]
pub struct HarnessTool {
    pub tool: AgentToolRef,
    pub replay: ReplayPolicy,
}

impl HarnessTool {
    pub fn new(tool: AgentToolRef) -> Self {
        Self {
            tool,
            replay: ReplayPolicy::Never,
        }
    }

    pub fn with_replay(mut self, replay: ReplayPolicy) -> Self {
        self.replay = replay;
        self
    }

    pub fn name(&self) -> &str {
        self.tool.name()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayPolicy {
    /// Never re-execute on recovery. The safe default.
    #[default]
    Never,
    /// Idempotent: safe to re-execute after a crash.
    Safe,
}

/// Options for [`AgentHarness::create`].
#[derive(Clone)]
pub struct AgentHarnessOptions {
    pub session: Session,
    pub model: Model,
    pub thinking_level: Option<ModelThinkingLevel>,
    pub active_tool_names: Option<Vec<String>>,
    pub tools: Vec<HarnessTool>,
    /// Port of upstream's `toolContext`. `pi-tools` fixes the tool context to an
    /// execution environment, so that is what is resolved per turn.
    pub tool_context: Option<ExecutionEnvRef>,
    pub system_prompt: Option<String>,
    pub resources: AgentHarnessResources,
    pub stream_options: SimpleStreamOptions,
    pub retry: Option<RetryPolicy>,
    pub compaction: Option<CompactionSettings>,
    pub steering_mode: Option<QueueMode>,
    pub follow_up_mode: Option<QueueMode>,
    pub tool_execution: Option<ToolExecutionMode>,
    pub telemetry: Option<Arc<dyn pi_telemetry::TelemetryContext>>,
}

impl AgentHarnessOptions {
    pub fn new(session: Session, model: Model) -> Self {
        Self {
            session,
            model,
            thinking_level: None,
            active_tool_names: None,
            tools: Vec::new(),
            tool_context: None,
            system_prompt: None,
            resources: AgentHarnessResources::default(),
            stream_options: SimpleStreamOptions::default(),
            retry: None,
            compaction: None,
            steering_mode: None,
            follow_up_mode: None,
            tool_execution: None,
            telemetry: None,
        }
    }
}

/// Mutable configuration, guarded so the harness stays `Send + Sync`.
struct HarnessConfig {
    model: Model,
    thinking_level: ModelThinkingLevel,
    active_tool_names: Vec<String>,
    tools: Vec<HarnessTool>,
    tool_context: Option<ExecutionEnvRef>,
    system_prompt: Option<String>,
    resources: AgentHarnessResources,
    stream_options: SimpleStreamOptions,
    retry: RetryPolicy,
    compaction: CompactionSettings,
    steering_mode: QueueMode,
    follow_up_mode: QueueMode,
    tool_execution: ToolExecutionMode,
    closed: bool,
}

/// The durable, multi-lane agent harness.
///
/// See the module docs: everything past configuration is
/// [`HarnessError::NotImplemented`] upstream and here.
#[derive(Clone)]
pub struct AgentHarness {
    session: Session,
    config: Arc<Mutex<HarnessConfig>>,
    telemetry: Option<Arc<dyn pi_telemetry::TelemetryContext>>,
}

impl AgentHarness {
    /// Lane name. Multi-lane support is part of the unimplemented surface, so
    /// this is always `"main"`.
    pub const NAME: &'static str = "main";

    /// Open a harness over a session.
    ///
    /// Refuses a session that already has records: restoring in-flight
    /// operations is unwritten, and silently ignoring them would drop durable
    /// work. Returns the harness plus the operations that would need resuming
    /// (always empty until restore lands).
    pub async fn create(
        options: AgentHarnessOptions,
    ) -> HarnessResult<(AgentHarness, Vec<SuspendedOperation>)> {
        let query = RecordQuery {
            limit: Some(1),
            ..Default::default()
        };
        let records = options
            .session
            .find_records(&query)
            .await
            .map_err(|error| HarnessError::Fault {
                message: error.to_string(),
            })?;
        if !records.is_empty() {
            return Err(HarnessError::not_implemented("create.restore"));
        }

        let active_tool_names = options.active_tool_names.clone().unwrap_or_else(|| {
            options
                .tools
                .iter()
                .map(|tool| tool.name().to_string())
                .collect()
        });

        let config = HarnessConfig {
            model: options.model,
            thinking_level: options.thinking_level.unwrap_or(ModelThinkingLevel::Off),
            active_tool_names,
            tools: options.tools,
            tool_context: options.tool_context,
            system_prompt: options.system_prompt,
            resources: options.resources,
            stream_options: options.stream_options,
            retry: options.retry.unwrap_or_default(),
            compaction: options.compaction.unwrap_or(DEFAULT_COMPACTION_SETTINGS),
            steering_mode: options.steering_mode.unwrap_or_default(),
            follow_up_mode: options.follow_up_mode.unwrap_or_default(),
            tool_execution: options
                .tool_execution
                .unwrap_or(crate::types::DEFAULT_TOOL_EXECUTION),
            closed: false,
        };

        Ok((
            AgentHarness {
                session: options.session,
                config: Arc::new(Mutex::new(config)),
                telemetry: options.telemetry,
            },
            Vec::new(),
        ))
    }

    pub fn name(&self) -> &'static str {
        Self::NAME
    }

    /// The durable session this harness reads and writes.
    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn telemetry(&self) -> Option<&Arc<dyn pi_telemetry::TelemetryContext>> {
        self.telemetry.as_ref()
    }

    /// `Closed` once closed, `NotImplemented` otherwise — upstream's rule for
    /// every method that is still a stub.
    fn unavailable<T>(&self, operation: &str) -> HarnessResult<T> {
        if self.config.lock().closed {
            Err(HarnessError::Closed)
        } else {
            Err(HarnessError::not_implemented(operation))
        }
    }

    pub async fn get_leaf_id(&self) -> HarnessResult<Option<String>> {
        self.session
            .get_leaf_id()
            .await
            .map_err(|error| HarnessError::Fault {
                message: error.to_string(),
            })
    }

    pub async fn close(&self) {
        self.config.lock().closed = true;
    }

    pub fn is_closed(&self) -> bool {
        self.config.lock().closed
    }

    // --- configuration (live) ----------------------------------------------

    pub fn model(&self) -> Model {
        self.config.lock().model.clone()
    }

    pub fn set_model(&self, model: Model) {
        self.config.lock().model = model;
    }

    pub fn thinking_level(&self) -> ModelThinkingLevel {
        self.config.lock().thinking_level
    }

    pub fn set_thinking_level(&self, level: ModelThinkingLevel) {
        self.config.lock().thinking_level = level;
    }

    pub fn active_tools(&self) -> Vec<String> {
        self.config.lock().active_tool_names.clone()
    }

    pub fn set_active_tools(&self, names: Vec<String>) {
        self.config.lock().active_tool_names = names;
    }

    pub fn tools(&self) -> Vec<HarnessTool> {
        self.config.lock().tools.clone()
    }

    /// Replace the tool set. `active_names` defaults to every tool's name,
    /// matching upstream.
    pub fn set_tools(&self, tools: Vec<HarnessTool>, active_names: Option<Vec<String>>) {
        let mut config = self.config.lock();
        config.active_tool_names = active_names
            .unwrap_or_else(|| tools.iter().map(|tool| tool.name().to_string()).collect());
        config.tools = tools;
    }

    pub fn tool_context(&self) -> Option<ExecutionEnvRef> {
        self.config.lock().tool_context.clone()
    }

    pub fn set_tool_context(&self, env: Option<ExecutionEnvRef>) {
        self.config.lock().tool_context = env;
    }

    pub fn system_prompt(&self) -> Option<String> {
        self.config.lock().system_prompt.clone()
    }

    pub fn set_system_prompt(&self, prompt: Option<String>) {
        self.config.lock().system_prompt = prompt;
    }

    pub fn resources(&self) -> AgentHarnessResources {
        self.config.lock().resources.clone()
    }

    pub fn set_resources(&self, resources: AgentHarnessResources) {
        self.config.lock().resources = resources;
    }

    pub fn stream_options(&self) -> SimpleStreamOptions {
        self.config.lock().stream_options.clone()
    }

    pub fn set_stream_options(&self, options: SimpleStreamOptions) {
        self.config.lock().stream_options = options;
    }

    pub fn retry_policy(&self) -> RetryPolicy {
        self.config.lock().retry.clone()
    }

    pub fn set_retry_policy(&self, policy: RetryPolicy) {
        self.config.lock().retry = policy;
    }

    pub fn compaction_settings(&self) -> CompactionSettings {
        self.config.lock().compaction
    }

    pub fn set_compaction_settings(&self, settings: CompactionSettings) {
        self.config.lock().compaction = settings;
    }

    pub fn steering_mode(&self) -> QueueMode {
        self.config.lock().steering_mode
    }

    pub fn set_steering_mode(&self, mode: QueueMode) {
        self.config.lock().steering_mode = mode;
    }

    pub fn follow_up_mode(&self) -> QueueMode {
        self.config.lock().follow_up_mode
    }

    pub fn set_follow_up_mode(&self, mode: QueueMode) {
        self.config.lock().follow_up_mode = mode;
    }

    pub fn tool_execution(&self) -> ToolExecutionMode {
        self.config.lock().tool_execution
    }

    pub fn set_tool_execution(&self, mode: ToolExecutionMode) {
        self.config.lock().tool_execution = mode;
    }

    // --- durable operations (unimplemented upstream) -----------------------

    pub async fn prompt(&self, _messages: Vec<AgentMessage>) -> HarnessResult<RunOutcome> {
        self.unavailable("prompt")
    }

    pub async fn skill(
        &self,
        _name: &str,
        _additional_instructions: Option<&str>,
    ) -> HarnessResult<RunOutcome> {
        self.unavailable("skill")
    }

    pub async fn prompt_from_template(
        &self,
        _name: &str,
        _args: &[String],
    ) -> HarnessResult<RunOutcome> {
        self.unavailable("promptFromTemplate")
    }

    pub async fn compact(&self, _custom_instructions: Option<&str>) -> HarnessResult<Value> {
        self.unavailable("compact")
    }

    pub async fn navigate_tree(&self, _target_id: Option<&str>) -> HarnessResult<Value> {
        self.unavailable("navigateTree")
    }

    pub async fn resume(&self) -> HarnessResult<Value> {
        self.unavailable("resume")
    }

    pub async fn abort(&self) -> HarnessResult<Value> {
        self.unavailable("abort")
    }

    pub async fn steer(&self, _message: AgentMessage) -> HarnessResult<String> {
        self.unavailable("steer")
    }

    pub async fn follow_up(&self, _message: AgentMessage) -> HarnessResult<String> {
        self.unavailable("followUp")
    }

    pub async fn next_run(&self, _message: AgentMessage) -> HarnessResult<String> {
        self.unavailable("nextRun")
    }

    pub async fn cancel_queued(&self, _entry_id: &str) -> HarnessResult<Value> {
        self.unavailable("cancelQueued")
    }

    pub async fn record_usage(&self, _usage: Usage) -> HarnessResult<()> {
        self.unavailable("recordUsage")
    }

    pub async fn wait_for_idle(&self) -> HarnessResult<()> {
        self.unavailable("waitForIdle")
    }

    pub async fn peek_action(&self) -> HarnessResult<Option<Value>> {
        self.unavailable("peekAction")
    }

    pub async fn execute_action(&self) -> HarnessResult<Option<Value>> {
        self.unavailable("executeAction")
    }

    pub async fn run_to_completion(&self) -> HarnessResult<()> {
        self.unavailable("runToCompletion")
    }

    pub async fn watch(&self) -> HarnessResult<Value> {
        self.unavailable("watch")
    }

    pub async fn lane(&self, _name: &str) -> HarnessResult<Option<AgentHarness>> {
        self.unavailable("lane")
    }

    pub async fn create_lane(&self, _name: &str, _at: Option<&str>) -> HarnessResult<AgentHarness> {
        self.unavailable("createLane")
    }

    pub async fn lanes(&self) -> HarnessResult<Vec<LaneInfo>> {
        self.unavailable("lanes")
    }

    pub async fn watch_session(&self) -> HarnessResult<Value> {
        self.unavailable("watchSession")
    }

    /// Hook registration. Unimplemented upstream; kept so the vocabulary exists.
    pub fn on_hook(&self, _name: HookName) -> HarnessResult<()> {
        self.unavailable("hooks.on")
    }

    pub fn on_event(&self, _event_type: &str) -> HarnessResult<()> {
        self.unavailable("events.on")
    }
}

impl std::fmt::Debug for AgentHarness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let config = self.config.lock();
        f.debug_struct("AgentHarness")
            .field("name", &Self::NAME)
            .field("model", &config.model.id)
            .field("tools", &config.active_tool_names)
            .field("closed", &config.closed)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::mock_model;
    use pi_session::types::{
        NewRecord, OperationIntent, OperationStartedRecord, RecordPayload, RunIntent,
        SessionMetadata,
    };
    use pi_session::InMemorySessionStorage;

    fn new_session(id: &str) -> Session {
        Session::new(Arc::new(InMemorySessionStorage::new(SessionMetadata::new(
            id, 1,
        ))))
    }

    async fn harness() -> AgentHarness {
        AgentHarness::create(AgentHarnessOptions::new(
            new_session("session"),
            mock_model(),
        ))
        .await
        .unwrap()
        .0
    }

    #[tokio::test]
    async fn opens_only_record_free_sessions_before_restore_is_implemented() {
        let session = new_session("session");
        let (harness, suspended) =
            AgentHarness::create(AgentHarnessOptions::new(session.clone(), mock_model()))
                .await
                .unwrap();

        assert!(suspended.is_empty());
        assert_eq!(harness.name(), "main");
        assert_eq!(harness.get_leaf_id().await.unwrap(), None);
        harness.close().await;

        let recorded = new_session("recorded");
        recorded
            .append_record(&NewRecord::new(
                "run",
                "main",
                RecordPayload::OperationStarted(OperationStartedRecord {
                    source_leaf_id: None,
                    intent: OperationIntent::Run(RunIntent {
                        original_prompt: Vec::new(),
                        initial_messages: Vec::new(),
                        system_prompt_override: None,
                        resume_data: None,
                    }),
                }),
            ))
            .await
            .unwrap();

        let error = AgentHarness::create(AgentHarnessOptions::new(recorded, mock_model()))
            .await
            .unwrap_err();
        assert_eq!(
            error,
            HarnessError::NotImplemented {
                operation: "create.restore".into()
            }
        );
    }

    #[tokio::test]
    async fn configuration_reads_and_writes_are_copies() {
        let harness = harness().await;

        harness.set_thinking_level(ModelThinkingLevel::High);
        assert_eq!(harness.thinking_level(), ModelThinkingLevel::High);

        harness.set_active_tools(vec!["one".into()]);
        let mut read = harness.active_tools();
        read.push("mutated".into());
        assert_eq!(harness.active_tools(), vec!["one".to_string()]);

        harness.set_steering_mode(QueueMode::All);
        assert_eq!(harness.steering_mode(), QueueMode::All);
        harness.set_follow_up_mode(QueueMode::All);
        assert_eq!(harness.follow_up_mode(), QueueMode::All);

        let settings = CompactionSettings {
            enabled: false,
            reserve_tokens: 1,
            keep_recent_tokens: 2,
        };
        harness.set_compaction_settings(settings);
        assert_eq!(harness.compaction_settings(), settings);
    }

    #[tokio::test]
    async fn set_tools_defaults_active_names_to_every_tool() {
        use crate::testing::{empty_schema, ExecuteFn, FnTool};
        let body: ExecuteFn =
            Arc::new(|_, _, _| Box::pin(async { Ok(pi_tools::ToolResult::text("x")) }));
        let tool = HarnessTool::new(Arc::new(FnTool::new("alpha", empty_schema(), body)));

        let harness = harness().await;
        harness.set_tools(vec![tool.clone()], None);
        assert_eq!(harness.active_tools(), vec!["alpha".to_string()]);

        harness.set_tools(vec![tool], Some(vec!["explicit".into()]));
        assert_eq!(harness.active_tools(), vec!["explicit".to_string()]);
    }

    #[tokio::test]
    async fn every_unfinished_operation_rejects_explicitly() {
        let harness = harness().await;
        let message = AgentMessage::user_text("hello");

        macro_rules! assert_unimplemented {
            ($operation:expr, $call:expr) => {
                let error = $call.unwrap_err();
                assert_eq!(
                    error,
                    HarnessError::NotImplemented {
                        operation: $operation.into()
                    },
                    "{} should report itself by name",
                    $operation
                );
            };
        }

        assert_unimplemented!("prompt", harness.prompt(vec![message.clone()]).await);
        assert_unimplemented!("skill", harness.skill("skill", None).await);
        assert_unimplemented!(
            "promptFromTemplate",
            harness.prompt_from_template("template", &[]).await
        );
        assert_unimplemented!("compact", harness.compact(None).await);
        assert_unimplemented!("navigateTree", harness.navigate_tree(None).await);
        assert_unimplemented!("resume", harness.resume().await);
        assert_unimplemented!("abort", harness.abort().await);
        assert_unimplemented!("steer", harness.steer(message.clone()).await);
        assert_unimplemented!("followUp", harness.follow_up(message.clone()).await);
        assert_unimplemented!("nextRun", harness.next_run(message).await);
        assert_unimplemented!("cancelQueued", harness.cancel_queued("queued").await);
        assert_unimplemented!("recordUsage", harness.record_usage(Usage::default()).await);
        assert_unimplemented!("waitForIdle", harness.wait_for_idle().await);
        assert_unimplemented!("peekAction", harness.peek_action().await);
        assert_unimplemented!("executeAction", harness.execute_action().await);
        assert_unimplemented!("runToCompletion", harness.run_to_completion().await);
        assert_unimplemented!("watch", harness.watch().await);
        assert_unimplemented!("lane", harness.lane("main").await);
        assert_unimplemented!("createLane", harness.create_lane("thread", None).await);
        assert_unimplemented!("lanes", harness.lanes().await);
        assert_unimplemented!("watchSession", harness.watch_session().await);
        assert_unimplemented!("hooks.on", harness.on_hook(HookName::BeforeRun));
        assert_unimplemented!("events.on", harness.on_event("event"));
    }

    #[tokio::test]
    async fn unfinished_operations_report_closed_after_close() {
        let harness = harness().await;
        harness.close().await;
        assert!(harness.is_closed());

        assert_eq!(
            harness.prompt(vec![]).await.unwrap_err(),
            HarnessError::Closed
        );
        assert_eq!(
            harness.wait_for_idle().await.unwrap_err(),
            HarnessError::Closed
        );
        assert_eq!(
            harness.on_hook(HookName::BeforeRun).unwrap_err(),
            HarnessError::Closed
        );
        assert_eq!(harness.on_event("event").unwrap_err(), HarnessError::Closed);

        // Configuration stays readable after close, as upstream's does.
        assert_eq!(harness.thinking_level(), ModelThinkingLevel::Off);
    }

    #[test]
    fn error_codes_are_stable_and_faults_are_distinguishable() {
        assert_eq!(HarnessError::Closed.code(), "closed");
        assert!(HarnessError::Closed.is_fault());
        assert!(HarnessError::not_implemented("prompt").is_fault());
        let rejection = HarnessError::NothingToCompact {
            lane: "main".into(),
            message: "nothing to compact".into(),
        };
        assert_eq!(rejection.code(), "nothing_to_compact");
        assert!(!rejection.is_fault());
    }
}
