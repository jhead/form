//! Reconstructing a lane's orchestration state from its durable records.
//!
//! Port of `harness/reducer.ts`. Both functions are pure: they read a bounded
//! recovery slice and never touch storage, which is what lets the agent loop
//! (W11) resume a suspended operation after a crash without replaying effects.
//!
//! [`validate_record_log`] rejects states the single-writer record protocol
//! cannot produce. These are corruption, not ordinary failures — an incomplete
//! intent/result prefix is normal and recoverable, two open operations on one
//! lane is not.

use std::collections::{HashMap, HashSet};

use pi_core::{AssistantMessage, DeferredHandle, StopReason, ToolCall};
use serde::{Deserialize, Serialize};

use crate::messages::AgentMessage;
use crate::types::{
    CompactionReason, Entry, EntryPayload, EntryType, LaneRecord, OperationIntent, OperationKind,
    ProvisionedEntry, QueueKind, RecordPayload, StepKind, ToolStartedRecord, UsageCause,
};

/// Machine-readable category for a contradiction in a lane's recovery slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordLogCorruptionReason {
    MultipleOpenOperations,
    UnknownOperation,
    RecordAfterFinish,
    NonConsecutiveAttempt,
    InvalidCompactionReason,
    QueueAfterAbort,
    InvalidQueueCancellation,
    InconsistentStep,
    ToolCallMismatch,
    DuplicateToolInvocation,
    ProvisionedEntryMismatch,
    InvalidDeferredHandle,
}

impl RecordLogCorruptionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MultipleOpenOperations => "multiple_open_operations",
            Self::UnknownOperation => "unknown_operation",
            Self::RecordAfterFinish => "record_after_finish",
            Self::NonConsecutiveAttempt => "non_consecutive_attempt",
            Self::InvalidCompactionReason => "invalid_compaction_reason",
            Self::QueueAfterAbort => "queue_after_abort",
            Self::InvalidQueueCancellation => "invalid_queue_cancellation",
            Self::InconsistentStep => "inconsistent_step",
            Self::ToolCallMismatch => "tool_call_mismatch",
            Self::DuplicateToolInvocation => "duplicate_tool_invocation",
            Self::ProvisionedEntryMismatch => "provisioned_entry_mismatch",
            Self::InvalidDeferredHandle => "invalid_deferred_handle",
        }
    }
}

/// One error kind with a machine-readable `reason`, mirroring upstream's
/// `RecordLogCorruption`. Recovery must reject, never repair.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[error("{message}")]
#[serde(rename_all = "camelCase")]
pub struct RecordLogCorruption {
    pub reason: RecordLogCorruptionReason,
    pub message: String,
}

impl RecordLogCorruption {
    pub fn code(&self) -> &'static str {
        self.reason.as_str()
    }
}

type Corrupt<T> = Result<T, RecordLogCorruption>;

fn corrupt<T>(reason: RecordLogCorruptionReason, message: impl Into<String>) -> Corrupt<T> {
    Err(RecordLogCorruption {
        reason,
        message: message.into(),
    })
}

/// A bounded slice of one lane's durable state.
#[derive(Debug, Clone, Default)]
pub struct RecordLogSlice {
    pub lane: String,
    /// `operation_started` records still open on this lane, newest first.
    pub open_operations: Vec<LaneRecord>,
    pub records: Vec<LaneRecord>,
    /// Operation-owned entries plus entries fetched by provisioned or
    /// referenced id.
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelection {
    pub provider: String,
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveLaneConfiguration {
    pub model: ModelSelection,
    pub thinking_level: String,
    pub active_tool_names: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalFailureSource {
    Step,
    DeferredFetch,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalFailureState {
    pub entry_id: String,
    pub source: TerminalFailureSource,
    pub message: AssistantMessage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolBatchCall {
    pub tool_index: i64,
    pub tool_call: ToolCall,
    /// The `tool_started` record, when the call was already dispatched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<ToolStartedRecord>,
    pub result_exists: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolBatchState {
    pub assistant_entry_id: String,
    pub calls: Vec<ToolBatchCall>,
    /// The assistant turn hit the output limit, so the batch may be incomplete.
    pub truncated: bool,
    pub unresolved: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepState {
    pub kind: StepKind,
    pub attempts: i64,
    pub result_entry_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_reason: Option<CompactionReason>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewestOwnEntry {
    pub entry_id: String,
    #[serde(rename = "type")]
    pub entry_type: EntryType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationTargets {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneOperationState {
    pub id: String,
    pub kind: OperationKind,
    pub intent: OperationIntent,
    pub aborting: bool,
    pub step: Option<StepState>,
    pub tool_batch: Option<ToolBatchState>,
    pub missing_initial_messages: Vec<ProvisionedEntry>,
    pub pending_steer: Vec<ProvisionedEntry>,
    pub pending_follow_up: Vec<ProvisionedEntry>,
    pub pending_writes: Vec<ProvisionedEntry>,
    pub deferred: Option<DeferredHandle>,
    pub overflow_recovery_used: bool,
    pub newest_own: Option<NewestOwnEntry>,
    pub targets: OperationTargets,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneState {
    pub lane: String,
    pub leaf_id: Option<String>,
    pub operation: Option<LaneOperationState>,
    pub pending_next_run: Vec<ProvisionedEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct LaneReductionInput {
    pub slice: RecordLogSlice,
    pub leaf_id: Option<String>,
    /// Entries appended by the open operation, oldest first. Empty when idle.
    pub own_entries: Vec<Entry>,
    /// Bounded effective-state lookups at the operation anchor or idle leaf.
    pub configuration_entries: Vec<Entry>,
    /// Harness option fallbacks used when no persisted value exists.
    pub defaults: Option<EffectiveLaneConfiguration>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LaneReductionResult {
    pub lane_state: LaneState,
    pub effective_configuration: EffectiveLaneConfiguration,
    pub terminal_failure: Option<TerminalFailureState>,
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn entries_by_id(entries: &[Entry]) -> HashMap<&str, &Entry> {
    entries
        .iter()
        .map(|entry| (entry.id.as_str(), entry))
        .collect()
}

fn by_sequence(records: &[LaneRecord]) -> Vec<LaneRecord> {
    let mut sorted = records.to_vec();
    sorted.sort_by_key(|record| record.seq);
    sorted
}

fn entries_by_sequence(entries: &[Entry]) -> Vec<Entry> {
    let mut sorted = entries.to_vec();
    sorted.sort_by_key(|entry| entry.seq);
    sorted
}

fn validate_exact_provisioned_entry(
    entries: &HashMap<&str, &Entry>,
    target: &ProvisionedEntry,
) -> Corrupt<()> {
    if let Some(entry) = entries.get(target.id.as_str()) {
        if &entry.to_provisioned() != target {
            return corrupt(
                RecordLogCorruptionReason::ProvisionedEntryMismatch,
                format!(
                    "Provisioned entry {} exists with content different from its intent",
                    target.id
                ),
            );
        }
    }
    Ok(())
}

fn validate_result_entry(
    entries: &HashMap<&str, &Entry>,
    result_entry_id: &str,
    matches: impl Fn(&Entry) -> bool,
    description: &str,
) -> Corrupt<()> {
    if let Some(entry) = entries.get(result_entry_id) {
        if !matches(entry) {
            return corrupt(
                RecordLogCorruptionReason::ProvisionedEntryMismatch,
                format!("Provisioned {description} entry {result_entry_id} exists with different content"),
            );
        }
    }
    Ok(())
}

fn is_assistant_message(entry: &Entry) -> bool {
    matches!(
        entry.as_message().map(|message| &message.message),
        Some(AgentMessage::Assistant(_))
    )
}

fn validate_attempt_reason(record: &LaneRecord) -> Corrupt<()> {
    let attempt = record.as_step_attempt().expect("step_attempt record");
    if attempt.step == StepKind::Compaction {
        if attempt.compaction_reason.is_none() {
            return corrupt(
                RecordLogCorruptionReason::InvalidCompactionReason,
                format!(
                    "Compaction attempt {} has no valid compaction reason",
                    record.id
                ),
            );
        }
    } else if attempt.compaction_reason.is_some() {
        return corrupt(
            RecordLogCorruptionReason::InvalidCompactionReason,
            format!(
                "{:?} attempt {} has a compaction reason",
                attempt.step, record.id
            ),
        );
    }
    Ok(())
}

fn validate_attempt_sequence(
    record: &LaneRecord,
    previous: Option<&LaneRecord>,
    entries: &HashMap<&str, &Entry>,
) -> Corrupt<()> {
    let attempt = record.as_step_attempt().expect("step_attempt record");
    let previous_attempt = previous.and_then(LaneRecord::as_step_attempt);
    let previous_result =
        previous_attempt.and_then(|previous| entries.get(previous.result_entry_id.as_str()));
    // A series continues while the previous attempt's result entry has not yet
    // been committed ahead of this attempt.
    let continues_series = previous_attempt.is_some_and(|previous| {
        previous.step == attempt.step
            && previous_result.is_none_or(|result| result.seq >= record.seq)
    });
    let expected_attempt = match (continues_series, previous_attempt) {
        (true, Some(previous)) => previous.attempt + 1,
        _ => 1,
    };
    if attempt.attempt != expected_attempt {
        return corrupt(
            RecordLogCorruptionReason::NonConsecutiveAttempt,
            format!(
                "{:?} attempt {} is {}; expected {expected_attempt}",
                attempt.step, record.id, attempt.attempt
            ),
        );
    }
    let Some(previous) = previous_attempt else {
        return Ok(());
    };
    if !continues_series || attempt.step == StepKind::Assistant {
        return Ok(());
    }
    if attempt.result_entry_id != previous.result_entry_id {
        return corrupt(
            RecordLogCorruptionReason::InconsistentStep,
            format!(
                "{:?} attempts disagree on their result entry id",
                attempt.step
            ),
        );
    }
    if attempt.compaction_reason != previous.compaction_reason {
        return corrupt(
            RecordLogCorruptionReason::InconsistentStep,
            format!(
                "{:?} attempts disagree on their compaction reason",
                attempt.step
            ),
        );
    }
    Ok(())
}

fn validate_attempt_result(entries: &HashMap<&str, &Entry>, record: &LaneRecord) -> Corrupt<()> {
    let attempt = record.as_step_attempt().expect("step_attempt record");
    match attempt.step {
        StepKind::Assistant => validate_result_entry(
            entries,
            &attempt.result_entry_id,
            is_assistant_message,
            "assistant result",
        ),
        StepKind::Compaction => validate_result_entry(
            entries,
            &attempt.result_entry_id,
            |entry| entry.entry_type() == EntryType::Compaction,
            "compaction result",
        ),
        StepKind::BranchSummary => validate_result_entry(
            entries,
            &attempt.result_entry_id,
            |entry| entry.entry_type() == EntryType::BranchSummary,
            "branch-summary result",
        ),
    }
}

fn tool_calls_of(entry: &Entry) -> Vec<ToolCall> {
    match entry.as_message().map(|message| &message.message) {
        Some(AgentMessage::Assistant(assistant)) => assistant.tool_calls().cloned().collect(),
        _ => Vec::new(),
    }
}

fn validate_tool_start(
    record: &LaneRecord,
    entries: &HashMap<&str, &Entry>,
    invocations: &mut HashSet<String>,
) -> Corrupt<()> {
    let started = record.as_tool_started().expect("tool_started record");
    let invocation = format!("{}\0{}", started.assistant_entry_id, started.tool_index);
    if !invocations.insert(invocation) {
        return corrupt(
            RecordLogCorruptionReason::DuplicateToolInvocation,
            format!(
                "Tool invocation {}:{} is duplicated",
                started.assistant_entry_id, started.tool_index
            ),
        );
    }

    let assistant_entry = entries.get(started.assistant_entry_id.as_str());
    let Some(assistant_entry) = assistant_entry.filter(|entry| is_assistant_message(entry)) else {
        return corrupt(
            RecordLogCorruptionReason::ToolCallMismatch,
            format!(
                "Tool start {} does not reference an assistant entry",
                record.id
            ),
        );
    };
    let tool_calls = tool_calls_of(assistant_entry);
    let matches_ordinal = usize::try_from(started.tool_index)
        .ok()
        .and_then(|index| tool_calls.get(index))
        .is_some_and(|call| call.id == started.tool_call_id && call.name == started.tool_name);
    if !matches_ordinal {
        return corrupt(
            RecordLogCorruptionReason::ToolCallMismatch,
            format!(
                "Tool start {} does not match its assistant tool-call ordinal",
                record.id
            ),
        );
    }

    validate_result_entry(
        entries,
        &started.result_entry_id,
        |entry| match entry.as_message().map(|message| &message.message) {
            Some(AgentMessage::ToolResult(result)) => {
                result.tool_call_id == started.tool_call_id && result.tool_name == started.tool_name
            }
            _ => false,
        },
        "tool result",
    )
}

fn validate_deferred_handles(entries: &[Entry]) -> Corrupt<()> {
    for entry in entries {
        if let Some(AgentMessage::Assistant(assistant)) =
            entry.as_message().map(|message| &message.message)
        {
            if assistant.stop_reason == StopReason::Deferred && assistant.deferred.is_none() {
                return corrupt(
                    RecordLogCorruptionReason::InvalidDeferredHandle,
                    format!(
                        "Deferred assistant entry {} does not carry a handle",
                        entry.id
                    ),
                );
            }
        }
    }
    Ok(())
}

fn validate_operation_result(entries: &HashMap<&str, &Entry>, record: &LaneRecord) -> Corrupt<()> {
    let started = record
        .as_operation_started()
        .expect("operation_started record");
    match &started.intent {
        OperationIntent::Run(run) => run
            .initial_messages
            .iter()
            .try_for_each(|target| validate_exact_provisioned_entry(entries, target)),
        OperationIntent::Compaction(compaction) => validate_result_entry(
            entries,
            &compaction.result_entry_id,
            |entry| entry.entry_type() == EntryType::Compaction,
            "manual compaction",
        ),
        OperationIntent::Navigation(navigation) => match &navigation.summary_entry_id {
            Some(summary_entry_id) => validate_result_entry(
                entries,
                summary_entry_id,
                |entry| entry.entry_type() == EntryType::BranchSummary,
                "navigation summary",
            ),
            None => Ok(()),
        },
    }
}

/// Validate a bounded lane recovery slice without reading or mutating storage.
pub fn validate_record_log(slice: &RecordLogSlice) -> Corrupt<()> {
    if slice.open_operations.len() > 1 {
        return corrupt(
            RecordLogCorruptionReason::MultipleOpenOperations,
            format!("Lane {} has at least two open operations", slice.lane),
        );
    }

    let entries = entries_by_id(&slice.entries);
    validate_deferred_handles(&slice.entries)?;
    let mut starts: HashSet<String> = HashSet::new();
    let mut finished_at: HashMap<String, i64> = HashMap::new();
    let mut aborted_at: HashMap<String, i64> = HashMap::new();
    let mut queue_enqueues: HashMap<String, LaneRecord> = HashMap::new();
    let mut latest_attempt: HashMap<String, LaneRecord> = HashMap::new();
    let mut tool_invocations: HashSet<String> = HashSet::new();

    for record in by_sequence(&slice.records) {
        if record.record_type() == crate::types::RecordType::OperationStarted {
            starts.insert(record.id.clone());
            validate_operation_result(&entries, &record)?;
            continue;
        }

        if let Some(run_id) = record.run_id() {
            if !starts.contains(run_id) {
                return corrupt(
                    RecordLogCorruptionReason::UnknownOperation,
                    format!("Record {} references unknown operation {run_id}", record.id),
                );
            }
            if finished_at
                .get(run_id)
                .is_some_and(|finish| record.seq > *finish)
            {
                return corrupt(
                    RecordLogCorruptionReason::RecordAfterFinish,
                    format!(
                        "Record {} follows the finish of operation {run_id}",
                        record.id
                    ),
                );
            }
        }

        match &record.payload {
            RecordPayload::OperationFinished(finished) => {
                finished_at.insert(finished.run_id.clone(), record.seq);
            }
            RecordPayload::AbortRequested(abort) => {
                aborted_at.insert(abort.run_id.clone(), record.seq);
            }
            RecordPayload::StepAttempt(attempt) => {
                validate_attempt_reason(&record)?;
                validate_attempt_sequence(&record, latest_attempt.get(&attempt.run_id), &entries)?;
                validate_attempt_result(&entries, &record)?;
                latest_attempt.insert(attempt.run_id.clone(), record.clone());
            }
            RecordPayload::ToolStarted(_) => {
                validate_tool_start(&record, &entries, &mut tool_invocations)?
            }
            RecordPayload::QueueEnqueued(enqueued) => {
                if enqueued.queue != QueueKind::NextRun {
                    if let Some(run_id) = &enqueued.run_id {
                        if aborted_at
                            .get(run_id)
                            .is_some_and(|abort| record.seq > *abort)
                        {
                            return corrupt(
                                RecordLogCorruptionReason::QueueAfterAbort,
                                format!(
                                    "{:?} item {} was enqueued after abort",
                                    enqueued.queue, enqueued.target.id
                                ),
                            );
                        }
                    }
                }
                queue_enqueues.insert(enqueued.target.id.clone(), record.clone());
                validate_exact_provisioned_entry(&entries, &enqueued.target)?;
            }
            RecordPayload::QueueCancelled(cancelled) => {
                let enqueue = queue_enqueues.get(&cancelled.entry_id);
                let valid = enqueue.is_some_and(|enqueue| {
                    enqueue.seq < record.seq
                        && enqueue.run_id() == cancelled.run_id.as_deref()
                        && !entries.contains_key(cancelled.entry_id.as_str())
                });
                if !valid {
                    return corrupt(
                        RecordLogCorruptionReason::InvalidQueueCancellation,
                        format!(
                            "Queue cancellation {} has no pending matching enqueue",
                            record.id
                        ),
                    );
                }
            }
            RecordPayload::WriteDeferred(deferred) => {
                validate_exact_provisioned_entry(&entries, &deferred.target)?
            }
            RecordPayload::Usage(_) | RecordPayload::OperationStarted(_) => {}
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Reduction
// ---------------------------------------------------------------------------

fn default_configuration() -> EffectiveLaneConfiguration {
    EffectiveLaneConfiguration {
        model: ModelSelection {
            provider: String::new(),
            model_id: String::new(),
        },
        thinking_level: "off".into(),
        active_tool_names: Vec::new(),
    }
}

fn derive_effective_configuration(input: &LaneReductionInput) -> EffectiveLaneConfiguration {
    let mut configuration = input.defaults.clone().unwrap_or_else(default_configuration);
    // Later entries win when an id appears in both slices, then the merged set
    // is applied in sequence order.
    let mut merged: HashMap<String, Entry> = HashMap::new();
    for entry in input
        .configuration_entries
        .iter()
        .chain(input.own_entries.iter())
    {
        merged.insert(entry.id.clone(), entry.clone());
    }
    let mut entries: Vec<Entry> = merged.into_values().collect();
    entries.sort_by_key(|entry| entry.seq);

    for entry in entries {
        match &entry.payload {
            EntryPayload::ModelChange(change) => {
                configuration.model = ModelSelection {
                    provider: change.provider.clone(),
                    model_id: change.model_id.clone(),
                }
            }
            EntryPayload::ThinkingLevelChange(change) => {
                configuration.thinking_level = change.thinking_level.clone()
            }
            EntryPayload::ActiveToolsChange(change) => {
                configuration.active_tool_names = change.active_tool_names.clone()
            }
            EntryPayload::Message(message) => {
                if let AgentMessage::Assistant(assistant) = &message.message {
                    configuration.model = ModelSelection {
                        provider: assistant.provider.clone(),
                        model_id: assistant.model.clone(),
                    };
                }
            }
            _ => {}
        }
    }
    configuration
}

fn derive_newest_own(entry: Option<&Entry>) -> Option<NewestOwnEntry> {
    let entry = entry?;
    let Some(message) = entry.as_message() else {
        return Some(NewestOwnEntry {
            entry_id: entry.id.clone(),
            entry_type: entry.entry_type(),
            role: None,
            stop_reason: None,
        });
    };
    let stop_reason = match &message.message {
        AgentMessage::Assistant(assistant) => Some(assistant.stop_reason),
        _ => None,
    };
    Some(NewestOwnEntry {
        entry_id: entry.id.clone(),
        entry_type: entry.entry_type(),
        role: Some(message.message.role().to_string()),
        stop_reason,
    })
}

fn derive_tool_batch(
    operation_id: &str,
    records: &[LaneRecord],
    own_entries: &[Entry],
    entries: &HashMap<&str, &Entry>,
    deferred_write_ids: &HashSet<String>,
) -> Option<ToolBatchState> {
    let assistant_entry = own_entries
        .iter()
        .rev()
        .find(|entry| is_assistant_message(entry) && !tool_calls_of(entry).is_empty())?;
    let tool_calls = tool_calls_of(assistant_entry);

    let mut starts: HashMap<i64, &ToolStartedRecord> = HashMap::new();
    for record in records {
        if let Some(started) = record.as_tool_started() {
            if started.run_id == operation_id && started.assistant_entry_id == assistant_entry.id {
                starts.insert(started.tool_index, started);
            }
        }
    }

    let calls: Vec<ToolBatchCall> = tool_calls
        .into_iter()
        .enumerate()
        .map(|(index, tool_call)| {
            let tool_index = index as i64;
            let started = starts.get(&tool_index).copied();
            let started_result = started.and_then(|started| entries.get(started.result_entry_id.as_str()).copied());
            // A result written by a blocked (deferred) write does not count as
            // resolved; only a real committed tool-result entry does.
            let blocked_result = own_entries.iter().find(|entry| {
                entry.seq > assistant_entry.seq
                    && !deferred_write_ids.contains(&entry.id)
                    && matches!(
                        entry.as_message().map(|message| &message.message),
                        Some(AgentMessage::ToolResult(result)) if result.tool_call_id == tool_call.id
                    )
            });
            let result = started_result.or(blocked_result);
            let terminate = result
                .and_then(|entry| entry.as_message())
                .and_then(|message| message.terminate)
                .filter(|terminate| *terminate)
                .map(|_| true);
            ToolBatchCall {
                tool_index,
                tool_call,
                started: started.cloned(),
                result_exists: result.is_some(),
                terminate,
            }
        })
        .collect();

    let truncated = matches!(
        assistant_entry.as_message().map(|message| &message.message),
        Some(AgentMessage::Assistant(assistant)) if assistant.stop_reason == StopReason::Length
    );
    let unresolved = calls.iter().any(|call| !call.result_exists);
    Some(ToolBatchState {
        assistant_entry_id: assistant_entry.id.clone(),
        calls,
        truncated,
        unresolved,
    })
}

/// Purely reconstruct one lane's orchestration state from its recovery inputs.
pub fn reduce_lane_state(input: &LaneReductionInput) -> Corrupt<LaneReductionResult> {
    validate_record_log(&input.slice)?;

    let records = by_sequence(&input.slice.records);
    let own_entries = entries_by_sequence(&input.own_entries);
    let mut merged_entries: Vec<Entry> = input.slice.entries.clone();
    merged_entries.extend(own_entries.iter().cloned());
    let entries = entries_by_id(&merged_entries);

    let cancelled_queue_ids: HashSet<&str> = records
        .iter()
        .filter_map(|record| match &record.payload {
            RecordPayload::QueueCancelled(cancelled) => Some(cancelled.entry_id.as_str()),
            _ => None,
        })
        .collect();
    let pending_queue_records: Vec<&LaneRecord> = records
        .iter()
        .filter(|record| match record.as_queue_enqueued() {
            Some(enqueued) => {
                !entries.contains_key(enqueued.target.id.as_str())
                    && !cancelled_queue_ids.contains(enqueued.target.id.as_str())
            }
            None => false,
        })
        .collect();

    let started = input.slice.open_operations.first();
    let captured_initial_message_ids: HashSet<&str> = started
        .and_then(LaneRecord::as_operation_started)
        .and_then(|record| match &record.intent {
            OperationIntent::Run(run) => Some(
                run.initial_messages
                    .iter()
                    .map(|target| target.id.as_str())
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default();
    let pending_next_run: Vec<ProvisionedEntry> = pending_queue_records
        .iter()
        .filter_map(|record| {
            let enqueued = record.as_queue_enqueued()?;
            (enqueued.queue == QueueKind::NextRun
                && !captured_initial_message_ids.contains(enqueued.target.id.as_str()))
            .then(|| enqueued.target.clone())
        })
        .collect();
    let effective_configuration = derive_effective_configuration(input);

    let Some(started) = started else {
        return Ok(LaneReductionResult {
            lane_state: LaneState {
                lane: input.slice.lane.clone(),
                leaf_id: input.leaf_id.clone(),
                operation: None,
                pending_next_run,
            },
            effective_configuration,
            terminal_failure: None,
        });
    };
    let operation_id = started.id.clone();
    let started_intent = started
        .as_operation_started()
        .expect("open operations are operation_started records")
        .intent
        .clone();

    let operation_records: Vec<&LaneRecord> = records
        .iter()
        .filter(|record| match record.record_type() {
            crate::types::RecordType::OperationStarted => record.id == operation_id,
            _ => record.run_id() == Some(operation_id.as_str()),
        })
        .collect();
    let aborting = operation_records
        .iter()
        .any(|record| matches!(record.payload, RecordPayload::AbortRequested(_)));

    let pending_of = |queue: QueueKind| -> Vec<ProvisionedEntry> {
        if aborting {
            return Vec::new();
        }
        pending_queue_records
            .iter()
            .filter_map(|record| {
                let enqueued = record.as_queue_enqueued()?;
                (enqueued.queue == queue
                    && enqueued.run_id.as_deref() == Some(operation_id.as_str()))
                .then(|| enqueued.target.clone())
            })
            .collect()
    };
    let pending_steer = pending_of(QueueKind::Steer);
    let pending_follow_up = pending_of(QueueKind::FollowUp);
    let pending_writes: Vec<ProvisionedEntry> = operation_records
        .iter()
        .filter_map(|record| match &record.payload {
            RecordPayload::WriteDeferred(deferred)
                if !entries.contains_key(deferred.target.id.as_str()) =>
            {
                Some(deferred.target.clone())
            }
            _ => None,
        })
        .collect();
    let missing_initial_messages: Vec<ProvisionedEntry> = match &started_intent {
        OperationIntent::Run(run) => run
            .initial_messages
            .iter()
            .filter(|target| !entries.contains_key(target.id.as_str()))
            .cloned()
            .collect(),
        _ => Vec::new(),
    };

    let newest_attempt = operation_records
        .iter()
        .filter(|record| matches!(record.payload, RecordPayload::StepAttempt(_)))
        .next_back()
        .copied();
    let step = newest_attempt
        .and_then(LaneRecord::as_step_attempt)
        .filter(|attempt| !entries.contains_key(attempt.result_entry_id.as_str()))
        .map(|attempt| StepState {
            kind: attempt.step,
            attempts: attempt.attempt,
            result_entry_id: attempt.result_entry_id.clone(),
            compaction_reason: match attempt.step {
                StepKind::Compaction => attempt.compaction_reason,
                _ => None,
            },
        });

    // An overflow compaction only counts as "used" when it happened after the
    // newest input this operation consumed; otherwise a fresh prompt resets it.
    let mut consumed_input_ids: HashSet<&str> = HashSet::new();
    if let OperationIntent::Run(run) = &started_intent {
        for target in &run.initial_messages {
            consumed_input_ids.insert(target.id.as_str());
        }
    }
    for record in &operation_records {
        if let Some(enqueued) = record.as_queue_enqueued() {
            if enqueued.queue != QueueKind::NextRun {
                consumed_input_ids.insert(enqueued.target.id.as_str());
            }
        }
    }
    let newest_consumed_input_sequence = consumed_input_ids
        .iter()
        .filter_map(|id| entries.get(id))
        .filter(|entry| entry.entry_type() == EntryType::Message)
        .map(|entry| entry.seq)
        .max();
    let overflow_recovery_used = operation_records.iter().any(|record| {
        record.as_step_attempt().is_some_and(|attempt| {
            attempt.step == StepKind::Compaction
                && attempt.compaction_reason == Some(CompactionReason::Overflow)
                && newest_consumed_input_sequence.is_none_or(|newest| record.seq > newest)
        })
    });

    let newest_own_entry = own_entries.last();
    let newest_own = derive_newest_own(newest_own_entry);
    let deferred = newest_own_entry
        .and_then(|entry| entry.as_message())
        .and_then(|message| match &message.message {
            AgentMessage::Assistant(assistant) if assistant.stop_reason == StopReason::Deferred => {
                assistant.deferred.clone()
            }
            _ => None,
        });
    let targets = match &started_intent {
        OperationIntent::Compaction(compaction) => OperationTargets {
            result: Some(entries.contains_key(compaction.result_entry_id.as_str())),
            summary: None,
        },
        OperationIntent::Navigation(navigation) => match &navigation.summary_entry_id {
            Some(summary_entry_id) => OperationTargets {
                result: None,
                summary: Some(entries.contains_key(summary_entry_id.as_str())),
            },
            None => OperationTargets::default(),
        },
        OperationIntent::Run(_) => OperationTargets::default(),
    };

    let deferred_write_ids: HashSet<String> = operation_records
        .iter()
        .filter_map(|record| match &record.payload {
            RecordPayload::WriteDeferred(deferred) => Some(deferred.target.id.clone()),
            _ => None,
        })
        .collect();

    let mut terminal_failure = None;
    if let Some(entry) = newest_own_entry {
        if let Some(AgentMessage::Assistant(assistant)) =
            entry.as_message().map(|message| &message.message)
        {
            if assistant.stop_reason == StopReason::Error && !deferred_write_ids.contains(&entry.id)
            {
                let produced_by_step = operation_records.iter().any(|record| {
                    record
                        .as_step_attempt()
                        .is_some_and(|attempt| attempt.result_entry_id == entry.id)
                });
                let previous_own_entry = own_entries
                    .len()
                    .checked_sub(2)
                    .and_then(|index| own_entries.get(index));
                let produced_by_deferred_fetch = operation_records.iter().any(|record| {
                    record.as_usage().is_some_and(|usage| {
                        usage.cause == UsageCause::DeferredFetch
                            && usage.entry_id.as_deref() == Some(entry.id.as_str())
                    })
                }) || matches!(
                    previous_own_entry.and_then(|entry| entry.as_message()).map(|message| &message.message),
                    Some(AgentMessage::Assistant(previous)) if previous.stop_reason == StopReason::Deferred
                );
                if produced_by_step || produced_by_deferred_fetch {
                    terminal_failure = Some(TerminalFailureState {
                        entry_id: entry.id.clone(),
                        source: if produced_by_step {
                            TerminalFailureSource::Step
                        } else {
                            TerminalFailureSource::DeferredFetch
                        },
                        message: assistant.clone(),
                    });
                }
            }
        }
    }

    Ok(LaneReductionResult {
        lane_state: LaneState {
            lane: input.slice.lane.clone(),
            leaf_id: input.leaf_id.clone(),
            operation: Some(LaneOperationState {
                id: operation_id.clone(),
                kind: started_intent.kind(),
                intent: started_intent,
                aborting,
                step,
                tool_batch: derive_tool_batch(
                    &operation_id,
                    &operation_records
                        .iter()
                        .map(|record| (*record).clone())
                        .collect::<Vec<_>>(),
                    &own_entries,
                    &entries,
                    &deferred_write_ids,
                ),
                missing_initial_messages,
                pending_steer,
                pending_follow_up,
                pending_writes,
                deferred,
                overflow_recovery_used,
                newest_own,
                targets,
            }),
            pending_next_run,
        },
        effective_configuration,
        terminal_failure,
    })
}
