//! Durable session vocabulary. Port of `harness/session/types.ts`.
//!
//! # Wire format
//!
//! Every type here is persisted verbatim into the JSONL v4 store and must load
//! in the TypeScript implementation, so both the field *names* and the field
//! *order* matter. Upstream builds each line with object spread:
//!
//! ```text
//! entry  = { ...provisioned, parentId, seq, timestamp }   // storage.ts
//! record = { ...newRecord,             seq, timestamp }   // storage.ts
//! ```
//!
//! and every provisioned literal in the upstream tree is written
//! `{ type, id, ...payload }` (entries) or `{ type, id, lane, ...payload }`
//! (records). That fixes the canonical key order this module emits:
//!
//! ```text
//! entry  : type, id, <payload>, <unknown>, parentId, seq, timestamp
//! record : type, id, lane, <payload>, <unknown>, seq, timestamp
//! ```
//!
//! Unknown keys are kept in [`Entry::extra`] / [`LaneRecord::extra`] and
//! re-emitted after the known payload fields, so a file written by a newer
//! upstream survives a load/store round trip here.

use std::fmt;

use pi_core::{StopReason, Usage};
use serde::de::Error as _;
use serde::ser::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// `serde_json::Error` implements both `ser::Error` and `de::Error`, so
/// `Error::custom` needs disambiguating at every call site.
fn ser_error(message: String) -> serde_json::Error {
    <serde_json::Error as serde::ser::Error>::custom(message)
}
use serde_json::{Map, Value};

use crate::error::{SessionError, SessionResult};
use crate::messages::AgentMessage;

/// Envelope and storage-assigned keys, stripped before a payload is decoded.
const ENTRY_ENVELOPE_KEYS: &[&str] =
    &["kind", "lane", "type", "id", "parentId", "seq", "timestamp"];
const RECORD_ENVELOPE_KEYS: &[&str] = &["kind", "type", "id", "lane", "seq", "timestamp"];

// ---------------------------------------------------------------------------
// Entries
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryType {
    Message,
    ModelChange,
    ThinkingLevelChange,
    ActiveToolsChange,
    Compaction,
    BranchSummary,
    Custom,
}

impl EntryType {
    pub fn as_str(self) -> &'static str {
        match self {
            EntryType::Message => "message",
            EntryType::ModelChange => "model_change",
            EntryType::ThinkingLevelChange => "thinking_level_change",
            EntryType::ActiveToolsChange => "active_tools_change",
            EntryType::Compaction => "compaction",
            EntryType::BranchSummary => "branch_summary",
            EntryType::Custom => "custom",
        }
    }

    pub fn from_str_opt(value: &str) -> Option<EntryType> {
        Some(match value {
            "message" => EntryType::Message,
            "model_change" => EntryType::ModelChange,
            "thinking_level_change" => EntryType::ThinkingLevelChange,
            "active_tools_change" => EntryType::ActiveToolsChange,
            "compaction" => EntryType::Compaction,
            "branch_summary" => EntryType::BranchSummary,
            "custom" => EntryType::Custom,
            _ => return None,
        })
    }

    /// Field names owned by this payload, in declaration order. Anything else
    /// on the line is unknown and preserved in `extra`.
    fn payload_keys(self) -> &'static [&'static str] {
        match self {
            EntryType::Message => &["message", "terminate"],
            EntryType::ModelChange => &["provider", "modelId"],
            EntryType::ThinkingLevelChange => &["thinkingLevel"],
            EntryType::ActiveToolsChange => &["activeToolNames"],
            EntryType::Compaction => &[
                "summary",
                "retainedTail",
                "tokensBefore",
                "details",
                "usage",
            ],
            EntryType::BranchSummary => &["fromId", "summary", "details", "usage"],
            EntryType::Custom => &["customType", "data"],
        }
    }
}

impl fmt::Display for EntryType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageEntry {
    pub message: AgentMessage,
    /// `terminate?: true` upstream — only ever set, never cleared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelChangeEntry {
    pub provider: String,
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingLevelEntry {
    pub thinking_level: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveToolsEntry {
    pub active_tool_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionEntry {
    pub summary: String,
    #[serde(default)]
    pub retained_tail: Vec<AgentMessage>,
    pub tokens_before: i64,
    #[serde(
        default,
        deserialize_with = "deserialize_present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryEntry {
    pub from_id: String,
    pub summary: String,
    #[serde(
        default,
        deserialize_with = "deserialize_present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomEntry {
    pub custom_type: String,
    #[serde(
        default,
        deserialize_with = "deserialize_present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub data: Option<Value>,
}

/// A present-but-null JSON field must stay `Some(Value::Null)` so it survives a
/// round trip; serde's default `Option` handling would collapse it to `None`.
fn deserialize_present_value<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

/// See [`AgentMessage`] on why the size spread is accepted here.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum EntryPayload {
    Message(MessageEntry),
    ModelChange(ModelChangeEntry),
    ThinkingLevelChange(ThinkingLevelEntry),
    ActiveToolsChange(ActiveToolsEntry),
    Compaction(CompactionEntry),
    BranchSummary(BranchSummaryEntry),
    Custom(CustomEntry),
}

impl EntryPayload {
    pub fn entry_type(&self) -> EntryType {
        match self {
            EntryPayload::Message(_) => EntryType::Message,
            EntryPayload::ModelChange(_) => EntryType::ModelChange,
            EntryPayload::ThinkingLevelChange(_) => EntryType::ThinkingLevelChange,
            EntryPayload::ActiveToolsChange(_) => EntryType::ActiveToolsChange,
            EntryPayload::Compaction(_) => EntryType::Compaction,
            EntryPayload::BranchSummary(_) => EntryType::BranchSummary,
            EntryPayload::Custom(_) => EntryType::Custom,
        }
    }

    fn write_fields(&self, out: &mut Map<String, Value>) -> Result<(), serde_json::Error> {
        let value = match self {
            EntryPayload::Message(p) => serde_json::to_value(p)?,
            EntryPayload::ModelChange(p) => serde_json::to_value(p)?,
            EntryPayload::ThinkingLevelChange(p) => serde_json::to_value(p)?,
            EntryPayload::ActiveToolsChange(p) => serde_json::to_value(p)?,
            EntryPayload::Compaction(p) => serde_json::to_value(p)?,
            EntryPayload::BranchSummary(p) => serde_json::to_value(p)?,
            EntryPayload::Custom(p) => serde_json::to_value(p)?,
        };
        match value {
            Value::Object(fields) => out.extend(fields),
            other => {
                return Err(ser_error(format!(
                    "entry payload is not an object: {other}"
                )))
            }
        }
        Ok(())
    }

    fn from_fields(
        entry_type: EntryType,
        fields: Value,
    ) -> Result<EntryPayload, serde_json::Error> {
        Ok(match entry_type {
            EntryType::Message => EntryPayload::Message(serde_json::from_value(fields)?),
            EntryType::ModelChange => EntryPayload::ModelChange(serde_json::from_value(fields)?),
            EntryType::ThinkingLevelChange => {
                EntryPayload::ThinkingLevelChange(serde_json::from_value(fields)?)
            }
            EntryType::ActiveToolsChange => {
                EntryPayload::ActiveToolsChange(serde_json::from_value(fields)?)
            }
            EntryType::Compaction => EntryPayload::Compaction(serde_json::from_value(fields)?),
            EntryType::BranchSummary => {
                EntryPayload::BranchSummary(serde_json::from_value(fields)?)
            }
            EntryType::Custom => EntryPayload::Custom(serde_json::from_value(fields)?),
        })
    }
}

/// An entry before storage assigns `parentId`, `seq` and `timestamp`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProvisionedEntry {
    pub id: String,
    pub payload: EntryPayload,
    /// Unrecognized keys, preserved verbatim across a round trip.
    pub extra: Map<String, Value>,
}

impl ProvisionedEntry {
    pub fn new(id: impl Into<String>, payload: EntryPayload) -> Self {
        Self {
            id: id.into(),
            payload,
            extra: Map::new(),
        }
    }

    pub fn message(id: impl Into<String>, message: AgentMessage) -> Self {
        Self::new(
            id,
            EntryPayload::Message(MessageEntry {
                message,
                terminate: None,
            }),
        )
    }

    pub fn custom(
        id: impl Into<String>,
        custom_type: impl Into<String>,
        data: Option<Value>,
    ) -> Self {
        Self::new(
            id,
            EntryPayload::Custom(CustomEntry {
                custom_type: custom_type.into(),
                data,
            }),
        )
    }

    pub fn entry_type(&self) -> EntryType {
        self.payload.entry_type()
    }

    fn to_map(&self) -> Result<Map<String, Value>, serde_json::Error> {
        let mut map = Map::new();
        map.insert("type".into(), Value::String(self.entry_type().to_string()));
        map.insert("id".into(), Value::String(self.id.clone()));
        self.payload.write_fields(&mut map)?;
        for (key, value) in &self.extra {
            map.insert(key.clone(), value.clone());
        }
        Ok(map)
    }
}

impl Serialize for ProvisionedEntry {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_map()
            .map_err(S::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ProvisionedEntry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let map = Map::<String, Value>::deserialize(deserializer)?;
        decode_provisioned_entry(map).map_err(D::Error::custom)
    }
}

pub(crate) fn decode_provisioned_entry(
    map: Map<String, Value>,
) -> Result<ProvisionedEntry, String> {
    let id = require_string(&map, "id")?;
    let type_name = require_string(&map, "type")?;
    let entry_type = EntryType::from_str_opt(&type_name)
        .ok_or_else(|| format!("has unknown entry type {type_name}"))?;
    let payload = EntryPayload::from_fields(entry_type, Value::Object(map.clone()))
        .map_err(|error| error.to_string())?;
    let extra = extra_keys(&map, ENTRY_ENVELOPE_KEYS, entry_type.payload_keys());
    Ok(ProvisionedEntry { id, payload, extra })
}

/// A committed entry: a provisioned entry plus the storage-assigned fields.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: String,
    pub seq: i64,
    /// The appending lane's leaf at commit time; `None` at the root.
    pub parent_id: Option<String>,
    pub timestamp: i64,
    pub payload: EntryPayload,
    pub extra: Map<String, Value>,
}

impl Entry {
    pub fn entry_type(&self) -> EntryType {
        self.payload.entry_type()
    }

    pub fn as_message(&self) -> Option<&MessageEntry> {
        match &self.payload {
            EntryPayload::Message(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_compaction(&self) -> Option<&CompactionEntry> {
        match &self.payload {
            EntryPayload::Compaction(c) => Some(c),
            _ => None,
        }
    }

    pub fn as_branch_summary(&self) -> Option<&BranchSummaryEntry> {
        match &self.payload {
            EntryPayload::BranchSummary(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_custom(&self) -> Option<&CustomEntry> {
        match &self.payload {
            EntryPayload::Custom(c) => Some(c),
            _ => None,
        }
    }

    /// Strip the storage-assigned fields. `reducer.rs` compares this against a
    /// record's provisioned target (`matchesProvisionedEntry` upstream).
    pub fn to_provisioned(&self) -> ProvisionedEntry {
        ProvisionedEntry {
            id: self.id.clone(),
            payload: self.payload.clone(),
            extra: self.extra.clone(),
        }
    }

    pub(crate) fn to_map(&self) -> Result<Map<String, Value>, serde_json::Error> {
        let mut map = ProvisionedEntry {
            id: self.id.clone(),
            payload: self.payload.clone(),
            extra: self.extra.clone(),
        }
        .to_map()?;
        map.insert(
            "parentId".into(),
            match &self.parent_id {
                Some(id) => Value::String(id.clone()),
                None => Value::Null,
            },
        );
        map.insert("seq".into(), Value::from(self.seq));
        map.insert("timestamp".into(), Value::from(self.timestamp));
        Ok(map)
    }
}

impl Serialize for Entry {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_map()
            .map_err(S::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Entry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let map = Map::<String, Value>::deserialize(deserializer)?;
        let provisioned = decode_provisioned_entry(map.clone()).map_err(D::Error::custom)?;
        Ok(Entry {
            id: provisioned.id,
            seq: require_i64(&map, "seq").map_err(D::Error::custom)?,
            parent_id: require_nullable_string(&map, "parentId").map_err(D::Error::custom)?,
            timestamp: require_i64(&map, "timestamp").map_err(D::Error::custom)?,
            payload: provisioned.payload,
            extra: provisioned.extra,
        })
    }
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordType {
    OperationStarted,
    AbortRequested,
    OperationFinished,
    StepAttempt,
    ToolStarted,
    QueueEnqueued,
    QueueCancelled,
    WriteDeferred,
    Usage,
}

impl RecordType {
    pub fn as_str(self) -> &'static str {
        match self {
            RecordType::OperationStarted => "operation_started",
            RecordType::AbortRequested => "abort_requested",
            RecordType::OperationFinished => "operation_finished",
            RecordType::StepAttempt => "step_attempt",
            RecordType::ToolStarted => "tool_started",
            RecordType::QueueEnqueued => "queue_enqueued",
            RecordType::QueueCancelled => "queue_cancelled",
            RecordType::WriteDeferred => "write_deferred",
            RecordType::Usage => "usage",
        }
    }

    pub fn from_str_opt(value: &str) -> Option<RecordType> {
        Some(match value {
            "operation_started" => RecordType::OperationStarted,
            "abort_requested" => RecordType::AbortRequested,
            "operation_finished" => RecordType::OperationFinished,
            "step_attempt" => RecordType::StepAttempt,
            "tool_started" => RecordType::ToolStarted,
            "queue_enqueued" => RecordType::QueueEnqueued,
            "queue_cancelled" => RecordType::QueueCancelled,
            "write_deferred" => RecordType::WriteDeferred,
            "usage" => RecordType::Usage,
            _ => return None,
        })
    }

    fn payload_keys(self) -> &'static [&'static str] {
        match self {
            RecordType::OperationStarted => &["sourceLeafId", "intent"],
            RecordType::AbortRequested => &["runId"],
            RecordType::OperationFinished => &["runId", "outcome", "error"],
            RecordType::StepAttempt => &[
                "runId",
                "step",
                "attempt",
                "resultEntryId",
                "compactionReason",
            ],
            RecordType::ToolStarted => &[
                "runId",
                "assistantEntryId",
                "toolIndex",
                "toolCallId",
                "toolName",
                "effectiveArgs",
                "resultEntryId",
                "replay",
            ],
            RecordType::QueueEnqueued => &["queue", "runId", "target"],
            RecordType::QueueCancelled => &["runId", "entryId"],
            RecordType::WriteDeferred => &["runId", "target"],
            RecordType::Usage => &[
                "cause",
                "runId",
                "entryId",
                "toolCallId",
                "attempt",
                "stopReason",
                "details",
                "usage",
            ],
        }
    }
}

impl fmt::Display for RecordType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunIntent {
    /// Normalized caller input before `before_run`; kept for suspended operations.
    #[serde(default)]
    pub original_prompt: Vec<AgentMessage>,
    /// Captured `nextRun` items, then the prompt, then `before_run` injections.
    #[serde(default)]
    pub initial_messages: Vec<ProvisionedEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_data: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionIntent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
    pub result_entry_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigationIntent {
    pub target_id: Option<String>,
    pub summarize: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_entry_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum OperationIntent {
    Run(RunIntent),
    Compaction(CompactionIntent),
    Navigation(NavigationIntent),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OperationKind {
    Run,
    Compaction,
    Navigation,
}

impl OperationIntent {
    pub fn kind(&self) -> OperationKind {
        match self {
            OperationIntent::Run(_) => OperationKind::Run,
            OperationIntent::Compaction(_) => OperationKind::Compaction,
            OperationIntent::Navigation(_) => OperationKind::Navigation,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationStartedRecord {
    pub source_leaf_id: Option<String>,
    pub intent: OperationIntent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbortRequestedRecord {
    pub run_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OperationOutcome {
    Completed,
    Aborted,
    Failed,
    Declined,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationFinishedRecord {
    pub run_id: String,
    pub outcome: OperationOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<OperationError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    Assistant,
    Compaction,
    BranchSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CompactionReason {
    Manual,
    Threshold,
    Overflow,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepAttemptRecord {
    pub run_id: String,
    pub step: StepKind,
    pub attempt: i64,
    pub result_entry_id: String,
    /// Only valid — and mandatory — when `step` is `compaction`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_reason: Option<CompactionReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolReplay {
    Never,
    Safe,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolStartedRecord {
    pub run_id: String,
    pub assistant_entry_id: String,
    pub tool_index: i64,
    pub tool_call_id: String,
    pub tool_name: String,
    #[serde(default)]
    pub effective_args: Map<String, Value>,
    pub result_entry_id: String,
    pub replay: ToolReplay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QueueKind {
    Steer,
    FollowUp,
    NextRun,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEnqueuedRecord {
    pub queue: QueueKind,
    /// Absent for the `nextRun` queue, which is not owned by an operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub target: ProvisionedEntry,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueCancelledRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub entry_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteDeferredRecord {
    pub run_id: String,
    pub target: ProvisionedEntry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageCause {
    Assistant,
    Compaction,
    BranchSummary,
    DeferredFetch,
    Tool,
    Hook,
    Adjustment,
}

/// Upstream models this as a discriminated union over `cause`; flattening it
/// keeps the type object-safe to carry over FFI. `payload_keys` fixes the wire
/// order to the union member order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecord {
    pub cause: UsageCause,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    #[serde(
        default,
        deserialize_with = "deserialize_present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub details: Option<Value>,
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RecordPayload {
    OperationStarted(OperationStartedRecord),
    AbortRequested(AbortRequestedRecord),
    OperationFinished(OperationFinishedRecord),
    StepAttempt(StepAttemptRecord),
    ToolStarted(ToolStartedRecord),
    QueueEnqueued(QueueEnqueuedRecord),
    QueueCancelled(QueueCancelledRecord),
    WriteDeferred(WriteDeferredRecord),
    Usage(UsageRecord),
}

impl RecordPayload {
    pub fn record_type(&self) -> RecordType {
        match self {
            RecordPayload::OperationStarted(_) => RecordType::OperationStarted,
            RecordPayload::AbortRequested(_) => RecordType::AbortRequested,
            RecordPayload::OperationFinished(_) => RecordType::OperationFinished,
            RecordPayload::StepAttempt(_) => RecordType::StepAttempt,
            RecordPayload::ToolStarted(_) => RecordType::ToolStarted,
            RecordPayload::QueueEnqueued(_) => RecordType::QueueEnqueued,
            RecordPayload::QueueCancelled(_) => RecordType::QueueCancelled,
            RecordPayload::WriteDeferred(_) => RecordType::WriteDeferred,
            RecordPayload::Usage(_) => RecordType::Usage,
        }
    }

    /// The operation this record belongs to (`"runId" in record` upstream).
    /// `operation_started` has no `runId`: its own `id` is the operation id.
    pub fn run_id(&self) -> Option<&str> {
        match self {
            RecordPayload::OperationStarted(_) => None,
            RecordPayload::AbortRequested(r) => Some(&r.run_id),
            RecordPayload::OperationFinished(r) => Some(&r.run_id),
            RecordPayload::StepAttempt(r) => Some(&r.run_id),
            RecordPayload::ToolStarted(r) => Some(&r.run_id),
            RecordPayload::QueueEnqueued(r) => r.run_id.as_deref(),
            RecordPayload::QueueCancelled(r) => r.run_id.as_deref(),
            RecordPayload::WriteDeferred(r) => Some(&r.run_id),
            RecordPayload::Usage(r) => r.run_id.as_deref(),
        }
    }

    fn write_fields(&self, out: &mut Map<String, Value>) -> Result<(), serde_json::Error> {
        let value = match self {
            RecordPayload::OperationStarted(p) => serde_json::to_value(p)?,
            RecordPayload::AbortRequested(p) => serde_json::to_value(p)?,
            RecordPayload::OperationFinished(p) => serde_json::to_value(p)?,
            RecordPayload::StepAttempt(p) => serde_json::to_value(p)?,
            RecordPayload::ToolStarted(p) => serde_json::to_value(p)?,
            RecordPayload::QueueEnqueued(p) => serde_json::to_value(p)?,
            RecordPayload::QueueCancelled(p) => serde_json::to_value(p)?,
            RecordPayload::WriteDeferred(p) => serde_json::to_value(p)?,
            RecordPayload::Usage(p) => serde_json::to_value(p)?,
        };
        match value {
            Value::Object(fields) => out.extend(fields),
            other => {
                return Err(ser_error(format!(
                    "record payload is not an object: {other}"
                )))
            }
        }
        Ok(())
    }

    fn from_fields(
        record_type: RecordType,
        fields: Value,
    ) -> Result<RecordPayload, serde_json::Error> {
        Ok(match record_type {
            RecordType::OperationStarted => {
                RecordPayload::OperationStarted(serde_json::from_value(fields)?)
            }
            RecordType::AbortRequested => {
                RecordPayload::AbortRequested(serde_json::from_value(fields)?)
            }
            RecordType::OperationFinished => {
                RecordPayload::OperationFinished(serde_json::from_value(fields)?)
            }
            RecordType::StepAttempt => RecordPayload::StepAttempt(serde_json::from_value(fields)?),
            RecordType::ToolStarted => RecordPayload::ToolStarted(serde_json::from_value(fields)?),
            RecordType::QueueEnqueued => {
                RecordPayload::QueueEnqueued(serde_json::from_value(fields)?)
            }
            RecordType::QueueCancelled => {
                RecordPayload::QueueCancelled(serde_json::from_value(fields)?)
            }
            RecordType::WriteDeferred => {
                RecordPayload::WriteDeferred(serde_json::from_value(fields)?)
            }
            RecordType::Usage => RecordPayload::Usage(serde_json::from_value(fields)?),
        })
    }
}

/// A record before storage assigns `seq` and `timestamp`.
#[derive(Debug, Clone, PartialEq)]
pub struct NewRecord {
    pub id: String,
    pub lane: String,
    pub payload: RecordPayload,
    pub extra: Map<String, Value>,
}

impl NewRecord {
    pub fn new(id: impl Into<String>, lane: impl Into<String>, payload: RecordPayload) -> Self {
        Self {
            id: id.into(),
            lane: lane.into(),
            payload,
            extra: Map::new(),
        }
    }

    pub fn record_type(&self) -> RecordType {
        self.payload.record_type()
    }

    fn to_map(&self) -> Result<Map<String, Value>, serde_json::Error> {
        let mut map = Map::new();
        map.insert("type".into(), Value::String(self.record_type().to_string()));
        map.insert("id".into(), Value::String(self.id.clone()));
        map.insert("lane".into(), Value::String(self.lane.clone()));
        self.payload.write_fields(&mut map)?;
        for (key, value) in &self.extra {
            map.insert(key.clone(), value.clone());
        }
        Ok(map)
    }
}

impl Serialize for NewRecord {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_map()
            .map_err(S::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for NewRecord {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let map = Map::<String, Value>::deserialize(deserializer)?;
        decode_new_record(map).map_err(D::Error::custom)
    }
}

pub(crate) fn decode_new_record(map: Map<String, Value>) -> Result<NewRecord, String> {
    let id = require_string(&map, "id")?;
    let lane = require_string(&map, "lane")?;
    let type_name = require_string(&map, "type")?;
    let record_type = RecordType::from_str_opt(&type_name)
        .ok_or_else(|| format!("has unknown record type {type_name}"))?;
    let payload = RecordPayload::from_fields(record_type, Value::Object(map.clone()))
        .map_err(|error| error.to_string())?;
    let extra = extra_keys(&map, RECORD_ENVELOPE_KEYS, record_type.payload_keys());
    Ok(NewRecord {
        id,
        lane,
        payload,
        extra,
    })
}

/// A committed lane record.
#[derive(Debug, Clone, PartialEq)]
pub struct LaneRecord {
    pub id: String,
    pub seq: i64,
    pub lane: String,
    pub timestamp: i64,
    pub payload: RecordPayload,
    pub extra: Map<String, Value>,
}

impl LaneRecord {
    pub fn record_type(&self) -> RecordType {
        self.payload.record_type()
    }

    pub fn run_id(&self) -> Option<&str> {
        self.payload.run_id()
    }

    pub fn as_operation_started(&self) -> Option<&OperationStartedRecord> {
        match &self.payload {
            RecordPayload::OperationStarted(r) => Some(r),
            _ => None,
        }
    }

    pub fn as_step_attempt(&self) -> Option<&StepAttemptRecord> {
        match &self.payload {
            RecordPayload::StepAttempt(r) => Some(r),
            _ => None,
        }
    }

    pub fn as_tool_started(&self) -> Option<&ToolStartedRecord> {
        match &self.payload {
            RecordPayload::ToolStarted(r) => Some(r),
            _ => None,
        }
    }

    pub fn as_queue_enqueued(&self) -> Option<&QueueEnqueuedRecord> {
        match &self.payload {
            RecordPayload::QueueEnqueued(r) => Some(r),
            _ => None,
        }
    }

    pub fn as_usage(&self) -> Option<&UsageRecord> {
        match &self.payload {
            RecordPayload::Usage(r) => Some(r),
            _ => None,
        }
    }

    pub(crate) fn to_map(&self) -> Result<Map<String, Value>, serde_json::Error> {
        let mut map = NewRecord {
            id: self.id.clone(),
            lane: self.lane.clone(),
            payload: self.payload.clone(),
            extra: self.extra.clone(),
        }
        .to_map()?;
        map.insert("seq".into(), Value::from(self.seq));
        map.insert("timestamp".into(), Value::from(self.timestamp));
        Ok(map)
    }
}

impl Serialize for LaneRecord {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_map()
            .map_err(S::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for LaneRecord {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let map = Map::<String, Value>::deserialize(deserializer)?;
        let new_record = decode_new_record(map.clone()).map_err(D::Error::custom)?;
        Ok(LaneRecord {
            id: new_record.id,
            seq: require_i64(&map, "seq").map_err(D::Error::custom)?,
            lane: new_record.lane,
            timestamp: require_i64(&map, "timestamp").map_err(D::Error::custom)?,
            payload: new_record.payload,
            extra: new_record.extra,
        })
    }
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryOrder {
    #[default]
    NewestFirst,
    OldestFirst,
}

impl EntryOrder {
    pub fn is_oldest_first(self) -> bool {
        matches!(self, EntryOrder::OldestFirst)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryCursor {
    pub after_seq: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EntryQuery {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub entry_type: Option<EntryType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<EntryOrder>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<EntryCursor>,
}

impl EntryQuery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_type(mut self, entry_type: EntryType) -> Self {
        self.entry_type = Some(entry_type);
        self
    }

    pub fn with_custom_type(mut self, custom_type: impl Into<String>) -> Self {
        self.custom_type = Some(custom_type.into());
        self
    }

    pub fn with_order(mut self, order: EntryOrder) -> Self {
        self.order = Some(order);
        self
    }

    pub fn with_limit(mut self, limit: i64) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn with_cursor(mut self, after_seq: i64) -> Self {
        self.cursor = Some(EntryCursor { after_seq });
        self
    }

    pub fn order_or_default(&self) -> EntryOrder {
        self.order.unwrap_or_default()
    }

    pub fn validate(&self) -> SessionResult<()> {
        validate_limit(self.limit)?;
        validate_cursor(self.cursor.map(|cursor| cursor.after_seq))
    }
}

/// Bounds of a branch scan. Default: the whole path, leaf to root.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BranchBounds {
    /// Defaults to the view's lane leaf.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    /// The scan ends after the first match, inclusive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_at_type: Option<EntryType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_at_id: Option<String>,
}

/// `EntryQuery & BranchBounds` at the view level (`start` may be omitted).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BranchQuery {
    #[serde(flatten)]
    pub query: EntryQuery,
    #[serde(flatten)]
    pub bounds: BranchBounds,
}

impl BranchQuery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_start(mut self, start: impl Into<String>) -> Self {
        self.bounds.start = Some(start.into());
        self
    }

    pub fn with_stop_at_type(mut self, entry_type: EntryType) -> Self {
        self.bounds.stop_at_type = Some(entry_type);
        self
    }

    pub fn with_stop_at_id(mut self, id: impl Into<String>) -> Self {
        self.bounds.stop_at_id = Some(id.into());
        self
    }

    pub fn with_type(mut self, entry_type: EntryType) -> Self {
        self.query.entry_type = Some(entry_type);
        self
    }

    pub fn with_custom_type(mut self, custom_type: impl Into<String>) -> Self {
        self.query.custom_type = Some(custom_type.into());
        self
    }

    pub fn with_order(mut self, order: EntryOrder) -> Self {
        self.query.order = Some(order);
        self
    }

    pub fn with_limit(mut self, limit: i64) -> Self {
        self.query.limit = Some(limit);
        self
    }

    pub fn with_cursor(mut self, after_seq: i64) -> Self {
        self.query.cursor = Some(EntryCursor { after_seq });
        self
    }

    pub fn validate(&self) -> SessionResult<()> {
        self.query.validate()
    }

    /// Bind the scan to a concrete start entry, as `SessionStorage` requires.
    pub fn resolve(self, start: impl Into<String>) -> BoundBranchQuery {
        BoundBranchQuery {
            start: start.into(),
            query: self.query,
            bounds: self.bounds,
        }
    }
}

/// The storage-level branch query: `start` is mandatory. Defaulting to a lane's
/// leaf is view sugar handled by [`crate::session::Session`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundBranchQuery {
    pub start: String,
    #[serde(flatten)]
    pub query: EntryQuery,
    #[serde(flatten)]
    pub bounds: BranchBounds,
}

impl BoundBranchQuery {
    pub fn new(start: impl Into<String>) -> Self {
        Self {
            start: start.into(),
            query: EntryQuery::default(),
            bounds: BranchBounds::default(),
        }
    }

    pub fn validate(&self) -> SessionResult<()> {
        self.query.validate()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RecordQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub record_type: Option<RecordType>,
    /// Matches `OperationStartedRecord.id` and the `runId` of owned records.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Valid only together with `record_type == OperationStarted`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_kind: Option<OperationKind>,
    /// Exclusive lower bound: `seq > after_seq`, regardless of order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_seq: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<EntryOrder>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
}

impl RecordQuery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_lane(mut self, lane: impl Into<String>) -> Self {
        self.lane = Some(lane.into());
        self
    }

    pub fn with_type(mut self, record_type: RecordType) -> Self {
        self.record_type = Some(record_type);
        self
    }

    pub fn with_run_id(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = Some(run_id.into());
        self
    }

    pub fn with_operation_kind(mut self, kind: OperationKind) -> Self {
        self.operation_kind = Some(kind);
        self
    }

    pub fn with_after_seq(mut self, after_seq: i64) -> Self {
        self.after_seq = Some(after_seq);
        self
    }

    pub fn with_order(mut self, order: EntryOrder) -> Self {
        self.order = Some(order);
        self
    }

    pub fn with_limit(mut self, limit: i64) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn order_or_default(&self) -> EntryOrder {
        self.order.unwrap_or_default()
    }

    pub fn validate(&self) -> SessionResult<()> {
        validate_limit(self.limit)?;
        validate_cursor(self.after_seq)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LogOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_seq: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
}

impl LogOptions {
    pub fn validate(&self) -> SessionResult<()> {
        validate_limit(self.limit)?;
        validate_cursor(self.after_seq)
    }
}

pub fn validate_limit(limit: Option<i64>) -> SessionResult<()> {
    match limit {
        Some(limit) if limit <= 0 => Err(SessionError::invalid_query(
            "limit must be a positive integer",
        )),
        _ => Ok(()),
    }
}

pub fn validate_cursor(after_seq: Option<i64>) -> SessionResult<()> {
    match after_seq {
        Some(after_seq) if after_seq < 0 => Err(SessionError::invalid_query(
            "cursor sequence must be a non-negative integer",
        )),
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Session-level shapes
// ---------------------------------------------------------------------------

/// Session identity. Backend-specific fields (`cwd`, `path`, `modifiedAt`,
/// `sourceFormat`, `name`, `metadata`, ...) live in [`SessionMetadata::extra`]
/// because the trait surface cannot be generic over a metadata type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMetadata {
    pub id: String,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl SessionMetadata {
    pub fn new(id: impl Into<String>, created_at: i64) -> Self {
        Self {
            id: id.into(),
            created_at,
            parent_session_id: None,
            extra: Map::new(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.extra.get(key)
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.extra.get(key).and_then(Value::as_str)
    }

    pub fn set(&mut self, key: impl Into<String>, value: Value) -> &mut Self {
        self.extra.insert(key.into(), value);
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SessionCreateOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    /// Required by the JSONL and SQLite backends; ignored by the in-memory one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Opaque application-owned metadata stored on the session header.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

impl SessionCreateOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn with_parent_session_id(mut self, id: impl Into<String>) -> Self {
        self.parent_session_id = Some(id.into());
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SessionListOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ForkScope {
    #[default]
    Branch,
    Tree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ForkPosition {
    Before,
    At,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ForkOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<ForkScope>,
    /// Branch scope only. Defaults to the `main` lane leaf.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    /// Branch scope only. Defaults to `at` for the lane leaf, `before` otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<ForkPosition>,
}

impl ForkOptions {
    pub fn tree() -> Self {
        Self {
            scope: Some(ForkScope::Tree),
            ..Default::default()
        }
    }

    pub fn branch_at(entry_id: impl Into<String>) -> Self {
        Self {
            scope: Some(ForkScope::Branch),
            entry_id: Some(entry_id.into()),
            position: Some(ForkPosition::At),
        }
    }

    pub fn scope_or_default(&self) -> ForkScope {
        self.scope.unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStats {
    pub message_count: i64,
    pub cached_tokens: i64,
    pub uncached_tokens: i64,
    pub total_tokens: i64,
    pub cost_total: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LanePointer {
    pub lane: String,
    pub leaf_id: Option<String>,
}

/// One durable mutation in commit order. Mirrors upstream's `LogItem` union.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LogItem {
    Entry {
        seq: i64,
        entry: Entry,
    },
    Record {
        seq: i64,
        record: LaneRecord,
    },
    Lane {
        seq: i64,
        lane: String,
        #[serde(rename = "leafId")]
        leaf_id: Option<String>,
    },
    /// `{ kind: "fact", fact: "name" }` upstream.
    Name {
        seq: i64,
        name: Option<String>,
    },
    /// `{ kind: "fact", fact: "label" }` upstream.
    Label {
        seq: i64,
        #[serde(rename = "targetId")]
        target_id: String,
        label: Option<String>,
    },
}

impl LogItem {
    pub fn seq(&self) -> i64 {
        match self {
            LogItem::Entry { seq, .. }
            | LogItem::Record { seq, .. }
            | LogItem::Lane { seq, .. }
            | LogItem::Name { seq, .. }
            | LogItem::Label { seq, .. } => *seq,
        }
    }

    /// The discriminant string the conformance suite asserts on.
    pub fn kind(&self) -> &'static str {
        match self {
            LogItem::Entry { .. } => "entry",
            LogItem::Record { .. } => "record",
            LogItem::Lane { .. } => "lane",
            LogItem::Name { .. } | LogItem::Label { .. } => "fact",
        }
    }
}

// ---------------------------------------------------------------------------
// Decoding helpers
// ---------------------------------------------------------------------------

fn require_string(map: &Map<String, Value>, field: &str) -> Result<String, String> {
    match map.get(field) {
        Some(Value::String(value)) => Ok(value.clone()),
        _ => Err(format!("has invalid {field}")),
    }
}

fn require_nullable_string(
    map: &Map<String, Value>,
    field: &str,
) -> Result<Option<String>, String> {
    match map.get(field) {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(Value::Null) => Ok(None),
        _ => Err(format!("has invalid {field}")),
    }
}

fn require_i64(map: &Map<String, Value>, field: &str) -> Result<i64, String> {
    map.get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("has invalid {field}"))
}

fn extra_keys(map: &Map<String, Value>, envelope: &[&str], payload: &[&str]) -> Map<String, Value> {
    let mut extra = Map::new();
    for (key, value) in map {
        if envelope.contains(&key.as_str()) || payload.contains(&key.as_str()) {
            continue;
        }
        extra.insert(key.clone(), value.clone());
    }
    extra
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::UserMessage;

    fn user(text: &str) -> AgentMessage {
        AgentMessage::User(UserMessage {
            content: pi_core::UserContent::Blocks(vec![pi_core::InputContent::text(text)]),
            timestamp: 1,
        })
    }

    #[test]
    fn entry_key_order_matches_upstream() {
        let entry = Entry {
            id: "e1".into(),
            seq: 1,
            parent_id: None,
            timestamp: 100,
            payload: EntryPayload::Custom(CustomEntry {
                custom_type: "note".into(),
                data: Some(serde_json::json!({ "text": "hello" })),
            }),
            extra: Map::new(),
        };
        assert_eq!(
            serde_json::to_string(&entry).unwrap(),
            r#"{"type":"custom","id":"e1","customType":"note","data":{"text":"hello"},"parentId":null,"seq":1,"timestamp":100}"#
        );
    }

    #[test]
    fn record_key_order_matches_upstream() {
        let record = LaneRecord {
            id: "run-1".into(),
            seq: 1,
            lane: "main".into(),
            timestamp: 100,
            payload: RecordPayload::OperationStarted(OperationStartedRecord {
                source_leaf_id: None,
                intent: OperationIntent::Run(RunIntent {
                    original_prompt: vec![],
                    initial_messages: vec![],
                    system_prompt_override: None,
                    resume_data: None,
                }),
            }),
            extra: Map::new(),
        };
        assert_eq!(
            serde_json::to_string(&record).unwrap(),
            r#"{"type":"operation_started","id":"run-1","lane":"main","sourceLeafId":null,"intent":{"kind":"run","originalPrompt":[],"initialMessages":[]},"seq":1,"timestamp":100}"#
        );
    }

    #[test]
    fn unknown_entry_fields_survive_a_round_trip() {
        let line = r#"{"type":"custom","id":"e1","customType":"note","futureField":{"a":1},"parentId":null,"seq":1,"timestamp":100}"#;
        let entry: Entry = serde_json::from_str(line).unwrap();
        assert_eq!(
            entry.extra.get("futureField"),
            Some(&serde_json::json!({"a": 1}))
        );
        assert_eq!(serde_json::to_string(&entry).unwrap(), line);
    }

    #[test]
    fn message_entry_round_trips() {
        let line = r#"{"type":"message","id":"m1","message":{"role":"user","content":[{"type":"text","text":"hi"}],"timestamp":1},"parentId":null,"seq":1,"timestamp":100}"#;
        let entry: Entry = serde_json::from_str(line).unwrap();
        assert_eq!(entry.as_message().unwrap().message, user("hi"));
        assert_eq!(serde_json::to_string(&entry).unwrap(), line);
    }

    #[test]
    fn provisioned_view_drops_storage_assigned_fields() {
        let entry = Entry {
            id: "e1".into(),
            seq: 4,
            parent_id: Some("root".into()),
            timestamp: 100,
            payload: EntryPayload::Custom(CustomEntry {
                custom_type: "note".into(),
                data: None,
            }),
            extra: Map::new(),
        };
        assert_eq!(
            serde_json::to_string(&entry.to_provisioned()).unwrap(),
            r#"{"type":"custom","id":"e1","customType":"note"}"#
        );
    }

    #[test]
    fn limit_and_cursor_validation_matches_upstream() {
        assert_eq!(
            validate_limit(Some(0)).unwrap_err().code(),
            SessionError::invalid_query("").code()
        );
        assert!(validate_limit(Some(-1)).is_err());
        assert!(validate_limit(Some(1)).is_ok());
        assert!(validate_cursor(Some(-1)).is_err());
        assert!(validate_cursor(Some(0)).is_ok());
    }
}
