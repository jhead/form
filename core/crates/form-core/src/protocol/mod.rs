//! The Swift ↔ Rust boundary contract. **Frozen** — see `docs/specs/00-protocol.md`.
//!
//! Nothing crosses this boundary except JSON. No shared structs, no pointers into Rust
//! memory, no lifetimes. That is what makes the transport swappable and the core reusable
//! from a future Windows/Linux client.

pub mod domain;
pub mod wire;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use domain::*;
pub use wire::*;

use crate::error::CoreError;

/// Bumped on any breaking change to this module. Swift asserts a match at startup.
pub const ABI_VERSION: u32 = 1;

// ---------------------------------------------------------------- config

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreConfig {
    pub data_dir: String,
    /// Populate a demo corpus. Off by default: the app talks to a real provider, and a
    /// dashboard full of invented sessions would be indistinguishable from real history.
    #[serde(default)]
    pub seed_mock_data: bool,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// Multiplier on stub-harness timings. 1.0 is human-realistic; tests use 100.0.
    #[serde(default = "default_speed")]
    pub harness_speed: f64,
    /// Which harness answers prompts. `pi` is the real agent against a live provider and is
    /// what the app ships with. `stub` is the deterministic generator, kept for tests and
    /// previews so the suite does not depend on a network or a key.
    #[serde(default)]
    pub harness: HarnessKind,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HarnessKind {
    #[default]
    Pi,
    Stub,
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_speed() -> f64 {
    1.0
}

// ---------------------------------------------------------------- envelope

/// Uniform reply for both `query` and `dispatch`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

impl Envelope {
    pub fn ok<T: Serialize>(value: T) -> Self {
        match serde_json::to_value(value) {
            Ok(data) => Self {
                ok: true,
                data: Some(data),
                error: None,
            },
            Err(e) => Self::from_error(&CoreError::Serialization(e.to_string())),
        }
    }

    pub fn from_error(err: &CoreError) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(ErrorBody {
                code: err.code().to_string(),
                message: err.to_string(),
                detail: None,
            }),
        }
    }

    /// Serialize, falling back to a hand-built error object so this can never itself fail.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            r#"{"ok":false,"error":{"code":"serialization","message":"envelope encode failed"}}"#
                .to_string()
        })
    }
}

// ---------------------------------------------------------------- queries

/// Synchronous reads. Must be cheap — anything expensive is a [`Command`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Query {
    ListSessions {
        #[serde(default)]
        include_archived: bool,
    },
    GetSession {
        session_id: String,
    },
    SearchSessions {
        q: String,
        #[serde(default)]
        limit: Option<usize>,
    },
    SearchInSession {
        session_id: String,
        q: String,
    },
    GetSettings,
    GetCatalog,
    GetStats {
        range: StatsRange,
        /// IANA timezone id; bucketing is meaningless in UTC.
        tz: String,
    },
    GetContextUsage {
        session_id: String,
    },
    RenderMarkdown {
        text: String,
        #[serde(default)]
        complete: Option<bool>,
    },
    ResolvePath {
        session_id: String,
        path: String,
    },
    GetAttachment {
        attachment_id: String,
    },
    ListRecentRoots,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StatsRange {
    D7,
    D30,
    All,
}

// ---------------------------------------------------------------- commands

/// Asynchronous effects. Returns an ack immediately; outcomes arrive as [`Event`]s
/// carrying the same `commandId`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Command {
    CreateSession {
        #[serde(default)]
        group_id: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        workspace_root: Option<String>,
        #[serde(default)]
        model_ref: Option<ModelRef>,
    },
    SendPrompt {
        session_id: String,
        text: String,
        #[serde(default)]
        attachment_ids: Vec<String>,
    },
    AbortRun {
        session_id: String,
    },
    RenameSession {
        session_id: String,
        title: String,
    },
    DeleteSession {
        session_id: String,
    },
    ArchiveSession {
        session_id: String,
        archived: bool,
    },
    PinSession {
        session_id: String,
        pinned: bool,
    },
    MoveSession {
        session_id: String,
        #[serde(default)]
        group_id: Option<String>,
        index: u32,
    },
    CreateGroup {
        name: String,
    },
    RenameGroup {
        group_id: String,
        name: String,
    },
    DeleteGroup {
        group_id: String,
    },
    ReorderGroup {
        group_id: String,
        index: u32,
    },
    SetGroupCollapsed {
        group_id: String,
        collapsed: bool,
    },
    SetSessionModel {
        session_id: String,
        model_ref: ModelRef,
    },
    SetWorkspaceRoot {
        session_id: String,
        #[serde(default)]
        path: Option<String>,
    },
    UpdateSettings {
        settings: Value,
    },
    AddAttachment {
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        bytes_base64: Option<String>,
        filename: String,
        mime: String,
    },
    RemoveAttachment {
        attachment_id: String,
    },
    /// Record the thumbnail the host rendered. Thumbnailing needs platform image APIs, so it
    /// stays in the app layer — but the *path* belongs in the store, or a second client on
    /// another platform has no way to find what was already rendered.
    SetAttachmentThumbnail {
        attachment_id: String,
        path: String,
    },
    BranchFromMessage {
        session_id: String,
        entry_id: String,
    },
    RetryMessage {
        session_id: String,
        entry_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandAck {
    pub command_id: String,
}

// ---------------------------------------------------------------- events

/// Everything the core pushes to the app. Delivered in order, on one dispatcher thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum EventKind {
    // --- run lifecycle (mirrors pi's AgentEvent) ---
    RunStart {
        session_id: String,
        run_id: String,
    },
    TurnStart {
        session_id: String,
        run_id: String,
    },
    MessageStart {
        session_id: String,
        entry: Entry,
    },
    MessageUpdate {
        session_id: String,
        entry_id: String,
        event: AssistantMessageEvent,
    },
    MessageEnd {
        session_id: String,
        entry: Entry,
    },
    ToolExecutionStart {
        session_id: String,
        tool_call_id: String,
        tool_name: String,
        args: Value,
    },
    ToolExecutionUpdate {
        session_id: String,
        tool_call_id: String,
        partial_result: Value,
    },
    ToolExecutionEnd {
        session_id: String,
        tool_call_id: String,
        result: Value,
        is_error: bool,
    },
    TurnEnd {
        session_id: String,
        run_id: String,
        usage: Usage,
    },
    RunEnd {
        session_id: String,
        run_id: String,
        outcome: RunOutcome,
        usage: Usage,
        duration_ms: u64,
    },

    // --- store and app ---
    SessionCreated {
        session: SessionSummary,
    },
    SessionUpdated {
        session: SessionSummary,
    },
    SessionDeleted {
        session_id: String,
    },
    GroupsChanged {
        groups: Vec<SessionGroup>,
    },
    SettingsChanged {
        settings: Value,
    },
    ContextUsageChanged {
        usage: ContextUsage,
    },
    StatsInvalidated,
    AttachmentAdded {
        attachment: Attachment,
    },
    AttachmentRemoved {
        attachment_id: String,
    },
    Error {
        code: String,
        message: String,
        detail: Option<Value>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub timestamp: TimestampMs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    #[serde(flatten)]
    pub kind: EventKind,
}

impl Event {
    pub fn new(kind: EventKind) -> Self {
        Self {
            timestamp: now_ms(),
            command_id: None,
            kind,
        }
    }

    pub fn with_command(kind: EventKind, command_id: Option<String>) -> Self {
        Self {
            timestamp: now_ms(),
            command_id,
            kind,
        }
    }
}
