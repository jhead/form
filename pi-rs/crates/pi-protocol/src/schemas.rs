//! Request/response/event schemas.
//!
//! Port of `.upstream/packages/protocol/src/schemas.ts`.
//!
//! # How TypeBox maps onto serde here
//!
//! * `Type.Object(..., { additionalProperties: false })` becomes
//!   `#[serde(deny_unknown_fields)]`. Serde only accepts that attribute on a
//!   container, so every variant of an internally tagged union is a *newtype*
//!   variant wrapping a named struct rather than an inline struct variant.
//! * Discriminated unions become `#[serde(tag = "...")]` enums, on whichever
//!   property upstream discriminates by (`type`, `command`, `role`, `status`).
//! * Refinements serde cannot express — `minLength: 1`, `minimum: 1`, literal
//!   values, and the status/stopReason consistency rules — are checked by
//!   [`Validate`], which the codec runs on every message in both directions.
//!   Unsigned integer types already cover every `minimum: 0`.
//! * Unions discriminated by a *non-tag* shape (a `status` that decides which
//!   further properties exist, or `ok` deciding `result` vs `error`) are one
//!   flat struct with optional fields plus a [`Validate`] rule. That keeps the
//!   types bridgeable to Swift, which is the constraint in AGENTS.md.

use serde::{Deserialize, Serialize};

/// `Type.Number()`/`Type.String()`/… — arbitrary JSON carried by the protocol.
///
/// Byte strings are not JSON and are rejected when a frame is decoded; see
/// `CborValue::to_json`.
pub type JsonValue = serde_json::Value;

/// Milliseconds since the epoch. `Type.Integer({ minimum: 0 })`.
pub type TimestampMs = u64;

/// The protocol version this crate speaks.
pub const PROTOCOL_VERSION: u32 = 1;

/// `off | minimal | low | medium | high | xhigh | max`.
///
/// Structurally identical to `pi_core::ModelThinkingLevel`, and reused from
/// there so the session server and the agent runtime share one vocabulary.
pub use pi_core::ModelThinkingLevel as ThinkingLevel;

/// `text | image`, the input modalities a model accepts.
pub use pi_core::Modality;

// ---------------------------------------------------------------------------
// validation
// ---------------------------------------------------------------------------

/// A refinement check failed. Deliberately carries nothing: upstream's
/// `ProtocolValidationError` never retains the rejected payload, and there is a
/// test asserting exactly that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Invalid;

pub(crate) type Checked = Result<(), Invalid>;

pub(crate) fn require(condition: bool) -> Checked {
    if condition {
        Ok(())
    } else {
        Err(Invalid)
    }
}

/// Non-empty string, i.e. TypeBox `Type.String({ minLength: 1 })`.
fn require_id(value: &str) -> Checked {
    require(!value.is_empty())
}

/// `Type.Number({ minimum: 0 })`. TypeBox's number guard also rejects `NaN`.
fn require_amount(value: f64) -> Checked {
    require(value.is_finite() && value >= 0.0)
}

fn require_each<T: Validate>(items: &[T]) -> Checked {
    for item in items {
        item.validate()?;
    }
    Ok(())
}

/// Refinements the type system does not carry.
pub trait Validate {
    fn validate(&self) -> Checked;
}

// ---------------------------------------------------------------------------
// models
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelRef {
    pub provider: String,
    pub id: String,
}

impl Validate for ModelRef {
    fn validate(&self) -> Checked {
        require_id(&self.provider)?;
        require_id(&self.id)
    }
}

/// Per-million-token rates.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

impl Validate for ModelCost {
    fn validate(&self) -> Checked {
        require_amount(self.input)?;
        require_amount(self.output)?;
        require_amount(self.cache_read)?;
        require_amount(self.cache_write)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelMetadata {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub api: String,
    pub reasoning: bool,
    pub input: Vec<Modality>,
    pub context_window: u64,
    pub max_tokens: u64,
    pub cost: ModelCost,
    pub supported_thinking_levels: Vec<ThinkingLevel>,
    pub authenticated: bool,
}

impl Validate for ModelMetadata {
    fn validate(&self) -> Checked {
        require_id(&self.provider)?;
        require_id(&self.id)?;
        require_id(&self.name)?;
        require_id(&self.api)?;
        require(self.context_window >= 1)?;
        require(self.max_tokens >= 1)?;
        self.cost.validate()?;
        require(!self.supported_thinking_levels.is_empty())
    }
}

// ---------------------------------------------------------------------------
// content
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TextContent {
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ThinkingContent {
    pub thinking: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redacted: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ImageContent {
    /// Base64-encoded image data.
    pub data: String,
    pub mime_type: String,
}

impl Validate for ImageContent {
    fn validate(&self) -> Checked {
        require_id(&self.mime_type)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ToolCallContent {
    pub tool_call_id: String,
    pub tool_name: String,
    pub input: JsonValue,
}

impl Validate for ToolCallContent {
    fn validate(&self) -> Checked {
        require_id(&self.tool_call_id)?;
        require_id(&self.tool_name)
    }
}

/// Content a user turn may carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UserContent {
    Text(TextContent),
    Image(ImageContent),
}

impl Validate for UserContent {
    fn validate(&self) -> Checked {
        match self {
            Self::Text(_) => Ok(()),
            Self::Image(content) => content.validate(),
        }
    }
}

/// Content an assistant turn may carry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AssistantContent {
    Text(TextContent),
    Thinking(ThinkingContent),
    ToolCall(ToolCallContent),
}

impl Validate for AssistantContent {
    fn validate(&self) -> Checked {
        match self {
            Self::Text(_) | Self::Thinking(_) => Ok(()),
            Self::ToolCall(content) => content.validate(),
        }
    }
}

/// Content a tool result may carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ToolContent {
    Text(TextContent),
    Image(ImageContent),
}

impl Validate for ToolContent {
    fn validate(&self) -> Checked {
        match self {
            Self::Text(_) => Ok(()),
            Self::Image(content) => content.validate(),
        }
    }
}

// ---------------------------------------------------------------------------
// usage
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UsageCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub total: f64,
}

impl Validate for UsageCost {
    fn validate(&self) -> Checked {
        require_amount(self.input)?;
        require_amount(self.output)?;
        require_amount(self.cache_read)?;
        require_amount(self.cache_write)?;
        require_amount(self.total)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    pub total_tokens: u64,
    pub cost: UsageCost,
}

impl Validate for Usage {
    fn validate(&self) -> Checked {
        self.cost.validate()
    }
}

// ---------------------------------------------------------------------------
// transcript
// ---------------------------------------------------------------------------

// Upstream declares `role` as the *second* property of every transcript item
// (`{ id, role, content, ... }`), and `packages/server/src/protocol.ts` builds
// them in exactly that order — which is the order they reach the wire, since
// CBOR maps here are insertion-ordered. A serde internally tagged enum always
// writes its tag first, so the discriminator is a real field on each struct and
// `TranscriptItem` is `untagged`; that reproduces upstream's byte order exactly.
// The single-variant marker enums pin each literal the way `Type.Literal` does.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UserRole {
    #[default]
    #[serde(rename = "user")]
    User,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AssistantRole {
    #[default]
    #[serde(rename = "assistant")]
    Assistant,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolRole {
    #[default]
    #[serde(rename = "tool")]
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UserTranscriptItem {
    pub id: String,
    pub role: UserRole,
    pub content: Vec<UserContent>,
    pub timestamp: TimestampMs,
}

impl UserTranscriptItem {
    pub fn new(id: impl Into<String>, content: Vec<UserContent>, timestamp: TimestampMs) -> Self {
        Self {
            id: id.into(),
            role: UserRole::User,
            content,
            timestamp,
        }
    }
}

impl Validate for UserTranscriptItem {
    fn validate(&self) -> Checked {
        require_id(&self.id)?;
        require_each(&self.content)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AssistantStatus {
    Streaming,
    Complete,
    Error,
    Aborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AssistantStopReason {
    Stop,
    Length,
    ToolUse,
    Error,
    Aborted,
}

/// Upstream splits this into four strict objects keyed on `status`. Flattening
/// them into one struct keeps the type bridgeable; [`Validate`] enforces the
/// same combinations the four schemas allow, including that `stopReason` and
/// `errorMessage` are absent where the corresponding schema omits them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AssistantTranscriptItem {
    pub id: String,
    pub role: AssistantRole,
    pub content: Vec<AssistantContent>,
    pub model: ModelRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub timestamp: TimestampMs,
    pub status: AssistantStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<AssistantStopReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

impl AssistantTranscriptItem {
    /// True once the item can no longer change, i.e. it may appear in an
    /// `item_finished` progress event.
    pub fn is_terminal(&self) -> bool {
        !matches!(self.status, AssistantStatus::Streaming)
    }
}

impl Validate for AssistantTranscriptItem {
    fn validate(&self) -> Checked {
        require_id(&self.id)?;
        require_each(&self.content)?;
        self.model.validate()?;
        if let Some(response_model) = &self.response_model {
            require_id(response_model)?;
        }
        if let Some(usage) = &self.usage {
            usage.validate()?;
        }
        match self.status {
            AssistantStatus::Streaming => {
                require(self.stop_reason.is_none())?;
                require(self.error_message.is_none())
            }
            AssistantStatus::Complete => {
                require(matches!(
                    self.stop_reason,
                    Some(
                        AssistantStopReason::Stop
                            | AssistantStopReason::Length
                            | AssistantStopReason::ToolUse
                    )
                ))?;
                require(self.error_message.is_none())
            }
            AssistantStatus::Error => {
                require(self.stop_reason == Some(AssistantStopReason::Error))?;
                // `Type.String({ minLength: 1 })` on the error variant only.
                match &self.error_message {
                    Some(message) => require_id(message),
                    None => Ok(()),
                }
            }
            AssistantStatus::Aborted => {
                // The aborted variant's `errorMessage` is a plain `Type.String()`,
                // so the empty string is allowed here but not above.
                require(self.stop_reason == Some(AssistantStopReason::Aborted))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolStatus {
    Running,
    Complete,
    Error,
}

/// Upstream splits this into three strict objects keyed on `status`; `isError`
/// is a literal in each one, so [`Validate`] pins it to the status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ToolTranscriptItem {
    pub id: String,
    pub role: ToolRole,
    pub tool_call_id: String,
    pub tool_name: String,
    pub input: JsonValue,
    pub content: Vec<ToolContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub timestamp: TimestampMs,
    pub status: ToolStatus,
    pub is_error: bool,
}

impl ToolTranscriptItem {
    /// True once the item can no longer change.
    pub fn is_terminal(&self) -> bool {
        !matches!(self.status, ToolStatus::Running)
    }
}

impl Validate for ToolTranscriptItem {
    fn validate(&self) -> Checked {
        require_id(&self.id)?;
        require_id(&self.tool_call_id)?;
        require_id(&self.tool_name)?;
        require_each(&self.content)?;
        if let Some(usage) = &self.usage {
            usage.validate()?;
        }
        match self.status {
            ToolStatus::Running | ToolStatus::Complete => require(!self.is_error),
            ToolStatus::Error => require(self.is_error),
        }
    }
}

/// One entry of a session transcript, discriminated by `role`.
///
/// `untagged` rather than `tag = "role"` so `role` keeps its upstream position
/// as the second key; the marker enum on each variant does the discriminating.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TranscriptItem {
    User(UserTranscriptItem),
    Assistant(AssistantTranscriptItem),
    Tool(ToolTranscriptItem),
}

impl TranscriptItem {
    /// True for the assistant and tool items that incremental progress events
    /// are allowed to carry (upstream's `item_updated` union).
    pub fn is_activity(&self) -> bool {
        !matches!(self, Self::User(_))
    }

    /// True for the items an `item_finished` event is allowed to carry.
    pub fn is_terminal(&self) -> bool {
        match self {
            Self::User(_) => false,
            Self::Assistant(item) => item.is_terminal(),
            Self::Tool(item) => item.is_terminal(),
        }
    }
}

impl Validate for TranscriptItem {
    fn validate(&self) -> Checked {
        match self {
            Self::User(item) => item.validate(),
            Self::Assistant(item) => item.validate(),
            Self::Tool(item) => item.validate(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AssistantDeltaKind {
    Text,
    Thinking,
    ToolCall,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ItemStarted {
    pub item: TranscriptItem,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AssistantDelta {
    pub message_id: String,
    pub content_index: u64,
    pub kind: AssistantDeltaKind,
    pub delta: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ItemUpdated {
    pub item: TranscriptItem,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ItemFinished {
    pub item: TranscriptItem,
}

/// Normalized incremental activity. Snapshots remain authoritative.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptProgress {
    ItemStarted(ItemStarted),
    AssistantDelta(AssistantDelta),
    ItemUpdated(ItemUpdated),
    ItemFinished(ItemFinished),
}

impl Validate for TranscriptProgress {
    fn validate(&self) -> Checked {
        match self {
            Self::ItemStarted(progress) => progress.item.validate(),
            Self::AssistantDelta(progress) => require_id(&progress.message_id),
            Self::ItemUpdated(progress) => {
                // Upstream's union here is assistant | tool, never user.
                require(progress.item.is_activity())?;
                progress.item.validate()
            }
            Self::ItemFinished(progress) => {
                // Upstream's union here is only the terminal variants.
                require(progress.item.is_terminal())?;
                progress.item.validate()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// sessions
// ---------------------------------------------------------------------------

/// Matches `AgentHarnessPhase` so adapters do not need a second phase vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    Idle,
    Turn,
    Compaction,
    BranchSummary,
    Retry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionMetadata {
    pub id: String,
    pub created_at: TimestampMs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<TimestampMs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

impl Validate for SessionMetadata {
    fn validate(&self) -> Checked {
        require_id(&self.id)?;
        if let Some(parent_session_id) = &self.parent_session_id {
            require_id(parent_session_id)?;
        }
        if let Some(cwd) = &self.cwd {
            require_id(cwd)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub cwd: String,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
    pub phase: SessionPhase,
    pub model: ModelRef,
    pub thinking_level: ThinkingLevel,
    pub attached: bool,
    pub locked: bool,
    pub revision: u64,
    pub transcript: Vec<TranscriptItem>,
    pub queued_steer: Vec<UserTranscriptItem>,
    pub queued_steer_count: u64,
}

impl Validate for SessionSnapshot {
    fn validate(&self) -> Checked {
        require_id(&self.id)?;
        require_id(&self.cwd)?;
        self.model.validate()?;
        require_each(&self.transcript)?;
        require_each(&self.queued_steer)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServerSnapshot {
    pub server_id: String,
    pub protocol_version: u32,
    pub revision: u64,
    pub sessions: Vec<SessionMetadata>,
    pub models: Vec<ModelMetadata>,
}

impl Validate for ServerSnapshot {
    fn validate(&self) -> Checked {
        require_id(&self.server_id)?;
        // `Type.Literal(PROTOCOL_VERSION)`, not a range.
        require(self.protocol_version == PROTOCOL_VERSION)?;
        require_each(&self.sessions)?;
        require_each(&self.models)
    }
}

// ---------------------------------------------------------------------------
// errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolErrorCode {
    Version,
    Busy,
    SessionLocked,
    NotFound,
    InvalidRequest,
    NotImplemented,
    InternalError,
}

impl ProtocolErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Version => "version",
            Self::Busy => "busy",
            Self::SessionLocked => "session_locked",
            Self::NotFound => "not_found",
            Self::InvalidRequest => "invalid_request",
            Self::NotImplemented => "not_implemented",
            Self::InternalError => "internal_error",
        }
    }
}

/// The error payload carried inside `hello_error` and failed responses. This is
/// wire data, not this crate's error type — see `ProtocolValidationError`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProtocolError {
    pub code: ProtocolErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
}

impl ProtocolError {
    pub fn new(code: ProtocolErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }
}

impl Validate for ProtocolError {
    fn validate(&self) -> Checked {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListCommand {}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CreateCommand {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ThinkingLevel>,
}

/// `attach`, `detach` and `abort` all carry just a session id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionCommand {
    pub session_id: String,
}

impl SessionCommand {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
        }
    }
}

/// `prompt` and `steer` share one payload upstream (`PromptPayloadProperties`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PromptCommand {
    pub session_id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SetModelCommand {
    pub session_id: String,
    pub model: ModelRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SetThinkingCommand {
    pub session_id: String,
    pub thinking_level: ThinkingLevel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    List(ListCommand),
    Create(CreateCommand),
    Attach(SessionCommand),
    Detach(SessionCommand),
    Prompt(PromptCommand),
    Steer(PromptCommand),
    Abort(SessionCommand),
    SetModel(SetModelCommand),
    SetThinking(SetThinkingCommand),
}

impl Command {
    /// The wire value of the `command` discriminator.
    pub fn name(&self) -> &'static str {
        match self {
            Self::List(_) => "list",
            Self::Create(_) => "create",
            Self::Attach(_) => "attach",
            Self::Detach(_) => "detach",
            Self::Prompt(_) => "prompt",
            Self::Steer(_) => "steer",
            Self::Abort(_) => "abort",
            Self::SetModel(_) => "set_model",
            Self::SetThinking(_) => "set_thinking",
        }
    }
}

impl Validate for Command {
    fn validate(&self) -> Checked {
        match self {
            Self::List(_) => Ok(()),
            Self::Create(command) => {
                if let Some(cwd) = &command.cwd {
                    require_id(cwd)?;
                }
                match &command.model {
                    Some(model) => model.validate(),
                    None => Ok(()),
                }
            }
            Self::Attach(command) | Self::Detach(command) | Self::Abort(command) => {
                require_id(&command.session_id)
            }
            Self::Prompt(command) | Self::Steer(command) => require_id(&command.session_id),
            Self::SetModel(command) => {
                require_id(&command.session_id)?;
                command.model.validate()
            }
            Self::SetThinking(command) => require_id(&command.session_id),
        }
    }
}

// ---------------------------------------------------------------------------
// command results
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ListResult {
    pub sessions: Vec<SessionMetadata>,
}

/// Every command except `list` and `detach` answers with a session snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionResult {
    pub session: SessionSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DetachResult {
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum CommandResult {
    List(ListResult),
    Create(SessionResult),
    Attach(SessionResult),
    Detach(DetachResult),
    Prompt(SessionResult),
    Steer(SessionResult),
    Abort(SessionResult),
    SetModel(SessionResult),
    SetThinking(SessionResult),
}

impl CommandResult {
    /// The wire value of the `command` discriminator.
    pub fn name(&self) -> &'static str {
        match self {
            Self::List(_) => "list",
            Self::Create(_) => "create",
            Self::Attach(_) => "attach",
            Self::Detach(_) => "detach",
            Self::Prompt(_) => "prompt",
            Self::Steer(_) => "steer",
            Self::Abort(_) => "abort",
            Self::SetModel(_) => "set_model",
            Self::SetThinking(_) => "set_thinking",
        }
    }
}

impl Validate for CommandResult {
    fn validate(&self) -> Checked {
        match self {
            Self::List(result) => require_each(&result.sessions),
            Self::Create(result)
            | Self::Attach(result)
            | Self::Prompt(result)
            | Self::Steer(result)
            | Self::Abort(result)
            | Self::SetModel(result)
            | Self::SetThinking(result) => result.session.validate(),
            Self::Detach(result) => require_id(&result.session_id),
        }
    }
}

// ---------------------------------------------------------------------------
// client messages
// ---------------------------------------------------------------------------

/// Must be the first frame sent by a client. Version is intentionally an
/// integer, not a coercible string, and any non-negative integer is accepted so
/// the server can answer a mismatch with a `version` error instead of a parse
/// failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ClientHello {
    pub version: u64,
}

impl Default for ClientHello {
    fn default() -> Self {
        Self {
            version: u64::from(PROTOCOL_VERSION),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RequestEnvelope {
    pub id: String,
    pub request: Command,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Hello(ClientHello),
    Request(RequestEnvelope),
}

impl Validate for ClientMessage {
    fn validate(&self) -> Checked {
        match self {
            Self::Hello(_) => Ok(()),
            Self::Request(envelope) => {
                require_id(&envelope.id)?;
                envelope.request.validate()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// server messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServerSnapshotEvent {
    pub snapshot: ServerSnapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionSnapshotEvent {
    pub snapshot: SessionSnapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionProgressEvent {
    pub session_id: String,
    pub progress: TranscriptProgress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionRemovedEvent {
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    ServerSnapshot(ServerSnapshotEvent),
    SessionSnapshot(SessionSnapshotEvent),
    SessionProgress(SessionProgressEvent),
    SessionRemoved(SessionRemovedEvent),
}

impl Validate for ServerEvent {
    fn validate(&self) -> Checked {
        match self {
            Self::ServerSnapshot(event) => event.snapshot.validate(),
            Self::SessionSnapshot(event) => event.snapshot.validate(),
            Self::SessionProgress(event) => {
                require_id(&event.session_id)?;
                event.progress.validate()
            }
            Self::SessionRemoved(event) => require_id(&event.session_id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServerHello {
    pub version: u32,
    pub connection_id: String,
    pub snapshot: ServerSnapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServerHelloError {
    pub error: ProtocolError,
}

/// Upstream is a union of two strict objects keyed on the `ok` literal. A
/// boolean cannot be a serde tag, so this is one struct plus a [`Validate`]
/// rule: `ok` true means `result` and no `error`, `ok` false means the reverse.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ResponseEnvelope {
    pub id: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<CommandResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
}

impl ResponseEnvelope {
    pub fn ok(id: impl Into<String>, result: CommandResult) -> Self {
        Self {
            id: id.into(),
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn failed(id: impl Into<String>, error: ProtocolError) -> Self {
        Self {
            id: id.into(),
            ok: false,
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EventEnvelope {
    pub event: ServerEvent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Hello(ServerHello),
    HelloError(ServerHelloError),
    Response(ResponseEnvelope),
    Event(EventEnvelope),
}

impl Validate for ServerMessage {
    fn validate(&self) -> Checked {
        match self {
            Self::Hello(hello) => {
                // `Type.Literal(PROTOCOL_VERSION)`: the server never negotiates.
                require(hello.version == PROTOCOL_VERSION)?;
                require_id(&hello.connection_id)?;
                hello.snapshot.validate()
            }
            Self::HelloError(message) => message.error.validate(),
            Self::Response(envelope) => {
                require_id(&envelope.id)?;
                if envelope.ok {
                    require(envelope.error.is_none())?;
                    match &envelope.result {
                        Some(result) => result.validate(),
                        None => Err(Invalid),
                    }
                } else {
                    require(envelope.result.is_none())?;
                    match &envelope.error {
                        Some(error) => error.validate(),
                        None => Err(Invalid),
                    }
                }
            }
            Self::Event(envelope) => envelope.event.validate(),
        }
    }
}

/// True only for the one protocol version this crate implements.
pub fn is_supported_protocol_version(version: u64) -> bool {
    version == u64::from(PROTOCOL_VERSION)
}
