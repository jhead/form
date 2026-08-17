//! Port of `test/harness/reducer.test.ts`.

use pi_core::{
    AssistantContent, AssistantMessage, DeferredHandle, InputContent, StopReason,
    ToolResultMessage, Usage, UserContent, UserMessage,
};
use pi_session::messages::AgentMessage;
use pi_session::reducer::{
    reduce_lane_state, validate_record_log, EffectiveLaneConfiguration, LaneReductionInput,
    ModelSelection, OperationTargets, RecordLogCorruptionReason, RecordLogSlice,
    TerminalFailureSource,
};
use pi_session::types::{
    AbortRequestedRecord, BranchSummaryEntry, CompactionEntry, CompactionIntent, CompactionReason,
    Entry, EntryPayload, LaneRecord, MessageEntry, NavigationIntent, OperationFinishedRecord,
    OperationIntent, OperationKind, OperationOutcome, OperationStartedRecord, ProvisionedEntry,
    QueueCancelledRecord, QueueEnqueuedRecord, QueueKind, RecordPayload, RecordType, RunIntent,
    StepAttemptRecord, StepKind, ToolReplay, ToolStartedRecord, UsageCause, UsageRecord,
};
use serde_json::Map;

// ---------------------------------------------------------------------------
// Helpers (mirroring the upstream test module)
// ---------------------------------------------------------------------------

fn usage() -> Usage {
    Usage {
        input: 1,
        output: 1,
        total_tokens: 2,
        ..Default::default()
    }
}

fn user_message(text: &str) -> AgentMessage {
    AgentMessage::User(UserMessage {
        content: UserContent::Text(text.into()),
        timestamp: 1,
    })
}

fn assistant_message(content: Vec<AssistantContent>, stop_reason: StopReason) -> AgentMessage {
    AgentMessage::Assistant(AssistantMessage {
        content,
        api: "openai-responses".into(),
        provider: "openai".into(),
        model: "test-model".into(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: usage(),
        stop_reason,
        deferred: (stop_reason == StopReason::Deferred).then(|| DeferredHandle {
            provider: "openai".into(),
            model_id: "test-model".into(),
            api: "openai-responses".into(),
            id: "deferred-1".into(),
            expires_at: None,
            poll_after_ms: None,
            data: None,
        }),
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1,
    })
}

fn assistant_error(message: &str) -> AgentMessage {
    let mut assistant = match assistant_message(vec![], StopReason::Error) {
        AgentMessage::Assistant(assistant) => assistant,
        _ => unreachable!(),
    };
    assistant.error_message = Some(message.into());
    AgentMessage::Assistant(assistant)
}

fn tool_call(id: &str, name: &str) -> AssistantContent {
    AssistantContent::ToolCall(pi_core::ToolCall::new(id, name))
}

fn tool_result_message(tool_call_id: &str, tool_name: &str) -> AgentMessage {
    AgentMessage::ToolResult(ToolResultMessage {
        tool_call_id: tool_call_id.into(),
        tool_name: tool_name.into(),
        content: vec![InputContent::text("result")],
        details: None,
        usage: None,
        added_tool_names: None,
        is_error: false,
        timestamp: 1,
    })
}

fn message_target(id: &str, message: AgentMessage) -> ProvisionedEntry {
    ProvisionedEntry::message(id, message)
}

fn persisted(target: &ProvisionedEntry, seq: i64, parent_id: Option<&str>) -> Entry {
    Entry {
        id: target.id.clone(),
        seq,
        parent_id: parent_id.map(str::to_string),
        timestamp: seq,
        payload: target.payload.clone(),
        extra: target.extra.clone(),
    }
}

fn record(id: &str, seq: i64, payload: RecordPayload) -> LaneRecord {
    LaneRecord {
        id: id.into(),
        seq,
        lane: "main".into(),
        timestamp: seq,
        payload,
        extra: Map::new(),
    }
}

fn run_started(seq: i64, id: &str, initial_messages: Vec<ProvisionedEntry>) -> LaneRecord {
    record(
        id,
        seq,
        RecordPayload::OperationStarted(OperationStartedRecord {
            source_leaf_id: None,
            intent: OperationIntent::Run(RunIntent {
                original_prompt: vec![],
                initial_messages,
                system_prompt_override: None,
                resume_data: None,
            }),
        }),
    )
}

fn run(seq: i64) -> LaneRecord {
    run_started(seq, "run-1", vec![])
}

fn compaction_started(seq: i64, result_entry_id: &str) -> LaneRecord {
    record(
        "compact-1",
        seq,
        RecordPayload::OperationStarted(OperationStartedRecord {
            source_leaf_id: Some("source".into()),
            intent: OperationIntent::Compaction(CompactionIntent {
                custom_instructions: None,
                result_entry_id: result_entry_id.into(),
            }),
        }),
    )
}

fn navigation_started(seq: i64, summary_entry_id: Option<&str>) -> LaneRecord {
    record(
        "navigate-1",
        seq,
        RecordPayload::OperationStarted(OperationStartedRecord {
            source_leaf_id: Some("source".into()),
            intent: OperationIntent::Navigation(NavigationIntent {
                target_id: Some("target".into()),
                summarize: true,
                custom_instructions: None,
                label: None,
                summary_entry_id: summary_entry_id.map(str::to_string),
            }),
        }),
    )
}

fn attempt(
    seq: i64,
    run_id: &str,
    step: StepKind,
    attempt_number: i64,
    result_entry_id: &str,
    compaction_reason: Option<CompactionReason>,
) -> LaneRecord {
    record(
        &format!("attempt-{seq}"),
        seq,
        RecordPayload::StepAttempt(StepAttemptRecord {
            run_id: run_id.into(),
            step,
            attempt: attempt_number,
            result_entry_id: result_entry_id.into(),
            compaction_reason: match step {
                StepKind::Compaction => compaction_reason.or(Some(CompactionReason::Manual)),
                _ => compaction_reason,
            },
        }),
    )
}

fn abort_requested(seq: i64, run_id: &str) -> LaneRecord {
    record(
        &format!("abort-{seq}"),
        seq,
        RecordPayload::AbortRequested(AbortRequestedRecord {
            run_id: run_id.into(),
        }),
    )
}

fn operation_finished(seq: i64, run_id: &str) -> LaneRecord {
    record(
        &format!("finish-{seq}"),
        seq,
        RecordPayload::OperationFinished(OperationFinishedRecord {
            run_id: run_id.into(),
            outcome: OperationOutcome::Completed,
            error: None,
        }),
    )
}

struct ToolStartOverrides {
    assistant_entry_id: &'static str,
    tool_index: i64,
    tool_call_id: &'static str,
    tool_name: &'static str,
    result_entry_id: &'static str,
}

impl Default for ToolStartOverrides {
    fn default() -> Self {
        Self {
            assistant_entry_id: "assistant-tools",
            tool_index: 0,
            tool_call_id: "call-1",
            tool_name: "tool-1",
            result_entry_id: "tool-result-1",
        }
    }
}

fn tool_started(seq: i64, overrides: ToolStartOverrides) -> LaneRecord {
    record(
        &format!("tool-start-{seq}"),
        seq,
        RecordPayload::ToolStarted(ToolStartedRecord {
            run_id: "run-1".into(),
            assistant_entry_id: overrides.assistant_entry_id.into(),
            tool_index: overrides.tool_index,
            tool_call_id: overrides.tool_call_id.into(),
            tool_name: overrides.tool_name.into(),
            effective_args: Map::new(),
            result_entry_id: overrides.result_entry_id.into(),
            replay: ToolReplay::Never,
        }),
    )
}

fn queue_enqueued(seq: i64, target: ProvisionedEntry, queue: QueueKind) -> LaneRecord {
    record(
        &format!("queue-{seq}"),
        seq,
        RecordPayload::QueueEnqueued(QueueEnqueuedRecord {
            queue,
            run_id: (queue != QueueKind::NextRun).then(|| "run-1".to_string()),
            target,
        }),
    )
}

fn queue_cancelled(seq: i64, entry_id: &str, run_id: Option<&str>) -> LaneRecord {
    record(
        &format!("cancel-{seq}"),
        seq,
        RecordPayload::QueueCancelled(QueueCancelledRecord {
            run_id: run_id.map(str::to_string),
            entry_id: entry_id.into(),
        }),
    )
}

fn write_deferred(seq: i64, target: ProvisionedEntry) -> LaneRecord {
    record(
        &format!("write-{seq}"),
        seq,
        RecordPayload::WriteDeferred(pi_session::types::WriteDeferredRecord {
            run_id: "run-1".into(),
            target,
        }),
    )
}

fn usage_record(
    seq: i64,
    entry_id: &str,
    cause: UsageCause,
    stop_reason: StopReason,
) -> LaneRecord {
    record(
        &format!("usage-{seq}"),
        seq,
        RecordPayload::Usage(UsageRecord {
            cause,
            run_id: Some("run-1".into()),
            entry_id: Some(entry_id.into()),
            tool_call_id: None,
            attempt: Some(1),
            stop_reason: Some(stop_reason),
            details: None,
            usage: usage(),
        }),
    )
}

fn compaction_entry(id: &str, seq: i64) -> Entry {
    Entry {
        id: id.into(),
        seq,
        parent_id: None,
        timestamp: seq,
        payload: EntryPayload::Compaction(CompactionEntry {
            summary: "summary".into(),
            retained_tail: vec![],
            tokens_before: 10,
            details: None,
            usage: None,
        }),
        extra: Map::new(),
    }
}

fn branch_summary_entry(id: &str, seq: i64) -> Entry {
    Entry {
        id: id.into(),
        seq,
        parent_id: Some("target".into()),
        timestamp: seq,
        payload: EntryPayload::BranchSummary(BranchSummaryEntry {
            from_id: "source".into(),
            summary: "summary".into(),
            details: None,
            usage: None,
        }),
        extra: Map::new(),
    }
}

/// Mirrors the upstream helper: open operations are the unfinished starts,
/// newest first.
fn recovery_slice(records: Vec<LaneRecord>, entries: Vec<Entry>) -> RecordLogSlice {
    let finished: Vec<String> = records
        .iter()
        .filter_map(|record| match &record.payload {
            RecordPayload::OperationFinished(finished) => Some(finished.run_id.clone()),
            _ => None,
        })
        .collect();
    let mut open_operations: Vec<LaneRecord> = records
        .iter()
        .filter(|record| {
            record.record_type() == RecordType::OperationStarted && !finished.contains(&record.id)
        })
        .cloned()
        .collect();
    open_operations.sort_by_key(|record| std::cmp::Reverse(record.seq));
    RecordLogSlice {
        lane: "main".into(),
        open_operations,
        records,
        entries,
    }
}

fn defaults() -> EffectiveLaneConfiguration {
    EffectiveLaneConfiguration {
        model: ModelSelection {
            provider: "default-provider".into(),
            model_id: "default-model".into(),
        },
        thinking_level: "off".into(),
        active_tool_names: vec!["default-tool".into()],
    }
}

#[derive(Default)]
struct ReductionOptions {
    entries: Vec<Entry>,
    configuration_entries: Vec<Entry>,
    leaf_id: Option<Option<String>>,
}

fn reduction_input(
    records: Vec<LaneRecord>,
    own_entries: Vec<Entry>,
    options: ReductionOptions,
) -> LaneReductionInput {
    let mut all_entries = own_entries.clone();
    all_entries.extend(options.entries.clone());
    let slice = recovery_slice(records, all_entries);
    LaneReductionInput {
        leaf_id: options
            .leaf_id
            .unwrap_or_else(|| own_entries.last().map(|entry| entry.id.clone())),
        slice,
        own_entries,
        configuration_entries: options.configuration_entries,
        defaults: Some(defaults()),
    }
}

#[track_caller]
fn expect_corruption(slice: &RecordLogSlice, reason: RecordLogCorruptionReason) {
    match validate_record_log(slice) {
        Err(error) => assert_eq!(error.reason, reason, "{}", error.message),
        Ok(()) => panic!("expected {reason:?}"),
    }
}

fn assistant_tools_entry() -> Entry {
    persisted(
        &message_target(
            "assistant-tools",
            assistant_message(vec![tool_call("call-1", "tool-1")], StopReason::ToolUse),
        ),
        3,
        None,
    )
}

// ---------------------------------------------------------------------------
// record-log validity
// ---------------------------------------------------------------------------

#[test]
fn rejects_multiple_open_operations() {
    expect_corruption(
        &recovery_slice(vec![run(1), run_started(2, "run-2", vec![])], vec![]),
        RecordLogCorruptionReason::MultipleOpenOperations,
    );
}

#[test]
fn rejects_a_record_for_an_unknown_operation() {
    expect_corruption(
        &recovery_slice(vec![abort_requested(1, "missing")], vec![]),
        RecordLogCorruptionReason::UnknownOperation,
    );
}

#[test]
fn rejects_a_record_after_its_operation_finish() {
    expect_corruption(
        &recovery_slice(
            vec![
                run(1),
                operation_finished(2, "run-1"),
                abort_requested(3, "run-1"),
            ],
            vec![],
        ),
        RecordLogCorruptionReason::RecordAfterFinish,
    );
}

#[test]
fn rejects_skipped_attempt_numbers() {
    expect_corruption(
        &recovery_slice(
            vec![
                run(1),
                attempt(2, "run-1", StepKind::Assistant, 1, "assistant-1", None),
                attempt(3, "run-1", StepKind::Assistant, 3, "assistant-2", None),
            ],
            vec![],
        ),
        RecordLogCorruptionReason::NonConsecutiveAttempt,
    );
}

#[test]
fn rejects_a_compaction_reason_on_a_non_compaction_attempt() {
    expect_corruption(
        &recovery_slice(
            vec![
                run(1),
                attempt(
                    2,
                    "run-1",
                    StepKind::Assistant,
                    1,
                    "assistant-1",
                    Some(CompactionReason::Manual),
                ),
            ],
            vec![],
        ),
        RecordLogCorruptionReason::InvalidCompactionReason,
    );
}

#[test]
fn rejects_a_compaction_attempt_without_a_reason() {
    let mut attempt = attempt(2, "run-1", StepKind::Compaction, 1, "compaction-1", None);
    if let RecordPayload::StepAttempt(step) = &mut attempt.payload {
        step.compaction_reason = None;
    }
    expect_corruption(
        &recovery_slice(vec![run(1), attempt], vec![]),
        RecordLogCorruptionReason::InvalidCompactionReason,
    );
}

#[test]
fn rejects_steering_enqueued_after_abort() {
    expect_corruption(
        &recovery_slice(
            vec![
                run(1),
                abort_requested(2, "run-1"),
                queue_enqueued(
                    3,
                    message_target("queue-1", user_message("queued")),
                    QueueKind::Steer,
                ),
            ],
            vec![],
        ),
        RecordLogCorruptionReason::QueueAfterAbort,
    );
}

#[test]
fn rejects_a_cancellation_without_an_enqueue() {
    expect_corruption(
        &recovery_slice(
            vec![run(1), queue_cancelled(2, "queue-1", Some("run-1"))],
            vec![],
        ),
        RecordLogCorruptionReason::InvalidQueueCancellation,
    );
}

#[test]
fn rejects_a_cancellation_whose_target_was_committed() {
    let target = message_target("queue-1", user_message("queued"));
    expect_corruption(
        &recovery_slice(
            vec![
                run(1),
                queue_enqueued(2, target.clone(), QueueKind::Steer),
                queue_cancelled(4, "queue-1", Some("run-1")),
            ],
            vec![persisted(&target, 3, None)],
        ),
        RecordLogCorruptionReason::InvalidQueueCancellation,
    );
}

#[test]
fn rejects_structural_attempts_that_disagree() {
    expect_corruption(
        &recovery_slice(
            vec![
                run(1),
                attempt(
                    2,
                    "run-1",
                    StepKind::Compaction,
                    1,
                    "compaction-1",
                    Some(CompactionReason::Threshold),
                ),
                attempt(
                    3,
                    "run-1",
                    StepKind::Compaction,
                    2,
                    "compaction-2",
                    Some(CompactionReason::Threshold),
                ),
            ],
            vec![],
        ),
        RecordLogCorruptionReason::InconsistentStep,
    );
    expect_corruption(
        &recovery_slice(
            vec![
                run(1),
                attempt(
                    2,
                    "run-1",
                    StepKind::Compaction,
                    1,
                    "compaction-1",
                    Some(CompactionReason::Threshold),
                ),
                attempt(
                    3,
                    "run-1",
                    StepKind::Compaction,
                    2,
                    "compaction-1",
                    Some(CompactionReason::Overflow),
                ),
            ],
            vec![],
        ),
        RecordLogCorruptionReason::InconsistentStep,
    );
}

#[test]
fn rejects_a_tool_start_that_does_not_match_its_assistant_call() {
    expect_corruption(
        &recovery_slice(
            vec![
                run(1),
                tool_started(
                    4,
                    ToolStartOverrides {
                        tool_call_id: "different-call",
                        ..Default::default()
                    },
                ),
            ],
            vec![assistant_tools_entry()],
        ),
        RecordLogCorruptionReason::ToolCallMismatch,
    );
}

#[test]
fn rejects_duplicate_tool_invocations() {
    let mut duplicate = tool_started(
        5,
        ToolStartOverrides {
            result_entry_id: "tool-result-2",
            ..Default::default()
        },
    );
    duplicate.id = "tool-start-duplicate".into();
    expect_corruption(
        &recovery_slice(
            vec![
                run(1),
                tool_started(4, ToolStartOverrides::default()),
                duplicate,
            ],
            vec![assistant_tools_entry()],
        ),
        RecordLogCorruptionReason::DuplicateToolInvocation,
    );
}

#[test]
fn rejects_a_provisioned_id_committed_with_different_content() {
    expect_corruption(
        &recovery_slice(
            vec![run_started(
                1,
                "run-1",
                vec![message_target("prompt-1", user_message("expected"))],
            )],
            vec![persisted(
                &message_target("prompt-1", user_message("different")),
                2,
                None,
            )],
        ),
        RecordLogCorruptionReason::ProvisionedEntryMismatch,
    );
}

#[test]
fn rejects_a_deferred_assistant_message_without_a_handle() {
    let mut deferred = match assistant_message(vec![], StopReason::Deferred) {
        AgentMessage::Assistant(assistant) => assistant,
        _ => unreachable!(),
    };
    deferred.deferred = None;
    expect_corruption(
        &recovery_slice(
            vec![run(1)],
            vec![persisted(
                &message_target("assistant-deferred", AgentMessage::Assistant(deferred)),
                2,
                None,
            )],
        ),
        RecordLogCorruptionReason::InvalidDeferredHandle,
    );
}

#[test]
fn accepts_every_prefix_of_a_one_tool_run() {
    // X1–X5 from upstream: each durable action must leave a valid log.
    let prompt = message_target("prompt-1", user_message("fix the bug"));
    let assistant_tools = message_target(
        "assistant-tools",
        assistant_message(vec![tool_call("call-1", "tool-1")], StopReason::ToolUse),
    );
    let tool_result = message_target("tool-result-1", tool_result_message("call-1", "tool-1"));
    let assistant_final = message_target(
        "assistant-final",
        assistant_message(vec![AssistantContent::text("done")], StopReason::Stop),
    );

    let records = vec![
        (1, Some(run_started(1, "run-1", vec![prompt.clone()])), None),
        (2, None, Some(persisted(&prompt, 2, None))),
        (
            3,
            Some(attempt(
                3,
                "run-1",
                StepKind::Assistant,
                1,
                "assistant-tools",
                None,
            )),
            None,
        ),
        (
            4,
            None,
            Some(persisted(&assistant_tools, 4, Some("prompt-1"))),
        ),
        (
            5,
            Some(tool_started(5, ToolStartOverrides::default())),
            None,
        ),
        (
            6,
            None,
            Some(persisted(&tool_result, 6, Some("assistant-tools"))),
        ),
        (
            7,
            Some(attempt(
                7,
                "run-1",
                StepKind::Assistant,
                1,
                "assistant-final",
                None,
            )),
            None,
        ),
        (
            8,
            None,
            Some(persisted(&assistant_final, 8, Some("tool-result-1"))),
        ),
        (9, Some(operation_finished(9, "run-1")), None),
    ];

    for prefix_length in 1..=records.len() {
        let prefix = &records[..prefix_length];
        let slice = recovery_slice(
            prefix
                .iter()
                .filter_map(|(_, record, _)| record.clone())
                .collect(),
            prefix
                .iter()
                .filter_map(|(_, _, entry)| entry.clone())
                .collect(),
        );
        validate_record_log(&slice).unwrap_or_else(|error| {
            panic!("prefix of {prefix_length} actions must be valid: {error}")
        });
    }
}

#[test]
fn validation_does_not_mutate_its_inputs() {
    let target = message_target("prompt-1", user_message("hello"));
    let slice = recovery_slice(
        vec![run_started(1, "run-1", vec![target.clone()])],
        vec![persisted(&target, 2, None)],
    );
    let before = format!("{slice:?}");
    validate_record_log(&slice).unwrap();
    assert_eq!(format!("{slice:?}"), before);
}

// ---------------------------------------------------------------------------
// lane-state reduction
// ---------------------------------------------------------------------------

#[test]
fn reduces_an_idle_lane_to_pending_next_run_input() {
    let pending = message_target("next-pending", user_message("pending"));
    let cancelled = message_target("next-cancelled", user_message("cancelled"));
    let consumed = message_target("next-consumed", user_message("consumed"));
    let input = reduction_input(
        vec![
            queue_enqueued(1, pending.clone(), QueueKind::NextRun),
            queue_enqueued(2, cancelled.clone(), QueueKind::NextRun),
            queue_cancelled(3, &cancelled.id, None),
            queue_enqueued(4, consumed.clone(), QueueKind::NextRun),
        ],
        vec![],
        ReductionOptions {
            entries: vec![persisted(&consumed, 5, None)],
            leaf_id: Some(Some("idle-leaf".into())),
            ..Default::default()
        },
    );

    let result = reduce_lane_state(&input).unwrap();
    assert_eq!(result.lane_state.lane, "main");
    assert_eq!(result.lane_state.leaf_id.as_deref(), Some("idle-leaf"));
    assert!(result.lane_state.operation.is_none());
    assert_eq!(result.lane_state.pending_next_run, vec![pending]);
    assert_eq!(result.effective_configuration, defaults());
    assert!(result.terminal_failure.is_none());
}

#[test]
fn folds_persisted_configuration_over_defaults_in_sequence() {
    let configuration_entries = vec![
        Entry {
            id: "model-change".into(),
            seq: 1,
            parent_id: None,
            timestamp: 1,
            payload: EntryPayload::ModelChange(pi_session::types::ModelChangeEntry {
                provider: "persisted-provider".into(),
                model_id: "persisted-model".into(),
            }),
            extra: Map::new(),
        },
        Entry {
            id: "thinking-change".into(),
            seq: 2,
            parent_id: Some("model-change".into()),
            timestamp: 2,
            payload: EntryPayload::ThinkingLevelChange(pi_session::types::ThinkingLevelEntry {
                thinking_level: "high".into(),
            }),
            extra: Map::new(),
        },
        Entry {
            id: "tools-change".into(),
            seq: 3,
            parent_id: Some("thinking-change".into()),
            timestamp: 3,
            payload: EntryPayload::ActiveToolsChange(pi_session::types::ActiveToolsEntry {
                active_tool_names: vec!["persisted-tool".into()],
            }),
            extra: Map::new(),
        },
    ];
    let input = reduction_input(
        vec![],
        vec![],
        ReductionOptions {
            configuration_entries,
            ..Default::default()
        },
    );

    assert_eq!(
        reduce_lane_state(&input).unwrap().effective_configuration,
        EffectiveLaneConfiguration {
            model: ModelSelection {
                provider: "persisted-provider".into(),
                model_id: "persisted-model".into()
            },
            thinking_level: "high".into(),
            active_tool_names: vec!["persisted-tool".into()],
        }
    );
    // The defaults the caller passed in are untouched.
    assert_eq!(input.defaults, Some(defaults()));
}

#[test]
fn applies_operation_owned_configuration_after_the_anchor() {
    let mut assistant =
        match assistant_message(vec![AssistantContent::text("response")], StopReason::Stop) {
            AgentMessage::Assistant(assistant) => assistant,
            _ => unreachable!(),
        };
    assistant.provider = "response-provider".into();
    assistant.model = "response-model".into();
    let assistant_entry = persisted(
        &message_target("assistant-config", AgentMessage::Assistant(assistant)),
        2,
        None,
    );
    let tools = Entry {
        id: "operation-tools".into(),
        seq: 3,
        parent_id: Some(assistant_entry.id.clone()),
        timestamp: 3,
        payload: EntryPayload::ActiveToolsChange(pi_session::types::ActiveToolsEntry {
            active_tool_names: vec!["operation-tool".into()],
        }),
        extra: Map::new(),
    };
    let result = reduce_lane_state(&reduction_input(
        vec![run(1)],
        vec![assistant_entry, tools],
        ReductionOptions::default(),
    ))
    .unwrap();

    assert_eq!(
        result.effective_configuration,
        EffectiveLaneConfiguration {
            model: ModelSelection {
                provider: "response-provider".into(),
                model_id: "response-model".into()
            },
            thinking_level: "off".into(),
            active_tool_names: vec!["operation-tool".into()],
        }
    );
}

#[test]
fn keeps_captured_next_run_input_with_the_open_run() {
    let captured = message_target("next-captured", user_message("captured"));
    let later = message_target("next-later", user_message("later"));
    let result = reduce_lane_state(&reduction_input(
        vec![
            queue_enqueued(1, captured.clone(), QueueKind::NextRun),
            run_started(2, "run-1", vec![captured.clone()]),
            queue_enqueued(3, later.clone(), QueueKind::NextRun),
        ],
        vec![],
        ReductionOptions::default(),
    ))
    .unwrap();

    assert_eq!(result.lane_state.pending_next_run, vec![later]);
    assert_eq!(
        result
            .lane_state
            .operation
            .unwrap()
            .missing_initial_messages,
        vec![captured]
    );
}

#[test]
fn derives_missing_input_queues_writes_and_the_unfinished_attempt() {
    let missing_prompt = message_target("prompt-missing", user_message("missing"));
    let committed_prompt = message_target("prompt-committed", user_message("committed"));
    let steer = message_target("steer-pending", user_message("steer"));
    let consumed_follow_up = message_target("follow-consumed", user_message("follow"));
    let next_run = message_target("next-run", user_message("next"));
    let pending_write = message_target("write-pending", user_message("write"));
    let applied_write = message_target("write-applied", user_message("applied"));

    let result = reduce_lane_state(&reduction_input(
        vec![
            run_started(
                1,
                "run-1",
                vec![missing_prompt.clone(), committed_prompt.clone()],
            ),
            queue_enqueued(3, steer.clone(), QueueKind::Steer),
            queue_enqueued(4, consumed_follow_up.clone(), QueueKind::FollowUp),
            queue_enqueued(5, next_run.clone(), QueueKind::NextRun),
            write_deferred(7, pending_write.clone()),
            write_deferred(8, applied_write.clone()),
            attempt(
                10,
                "run-1",
                StepKind::Assistant,
                1,
                "assistant-pending",
                None,
            ),
        ],
        vec![
            persisted(&committed_prompt, 2, None),
            persisted(&consumed_follow_up, 6, Some(&committed_prompt.id)),
            persisted(&applied_write, 9, Some(&consumed_follow_up.id)),
        ],
        ReductionOptions::default(),
    ))
    .unwrap();

    assert_eq!(result.lane_state.pending_next_run, vec![next_run]);
    let operation = result.lane_state.operation.unwrap();
    assert_eq!(operation.id, "run-1");
    assert_eq!(operation.kind, OperationKind::Run);
    assert!(!operation.aborting);
    assert_eq!(operation.missing_initial_messages, vec![missing_prompt]);
    assert_eq!(operation.pending_steer, vec![steer]);
    assert!(operation.pending_follow_up.is_empty());
    assert_eq!(operation.pending_writes, vec![pending_write]);
    let step = operation.step.unwrap();
    assert_eq!(step.kind, StepKind::Assistant);
    assert_eq!(step.attempts, 1);
    assert_eq!(step.result_entry_id, "assistant-pending");
    let newest = operation.newest_own.unwrap();
    assert_eq!(newest.entry_id, applied_write.id);
    assert_eq!(newest.role.as_deref(), Some("user"));
}

#[test]
fn abort_kills_steer_and_follow_up_but_keeps_writes_and_next_run() {
    let steer = message_target("steer-aborted", user_message("steer"));
    let follow_up = message_target("follow-aborted", user_message("follow"));
    let next_run = message_target("next-after-abort", user_message("next"));
    let pending_write = message_target("write-after-abort", user_message("write"));
    let result = reduce_lane_state(&reduction_input(
        vec![
            run(1),
            queue_enqueued(2, steer, QueueKind::Steer),
            queue_enqueued(3, follow_up, QueueKind::FollowUp),
            queue_enqueued(4, next_run.clone(), QueueKind::NextRun),
            write_deferred(5, pending_write.clone()),
            abort_requested(6, "run-1"),
        ],
        vec![],
        ReductionOptions::default(),
    ))
    .unwrap();

    assert_eq!(result.lane_state.pending_next_run, vec![next_run]);
    let operation = result.lane_state.operation.unwrap();
    assert!(operation.aborting);
    assert!(operation.pending_steer.is_empty());
    assert!(operation.pending_follow_up.is_empty());
    assert_eq!(operation.pending_writes, vec![pending_write]);
}

#[test]
fn closes_the_newest_attempt_only_when_its_result_exists() {
    let target = message_target(
        "result",
        assistant_message(vec![AssistantContent::text("done")], StopReason::Stop),
    );
    let result = reduce_lane_state(&reduction_input(
        vec![
            run(1),
            attempt(2, "run-1", StepKind::Assistant, 1, &target.id, None),
        ],
        vec![persisted(&target, 3, None)],
        ReductionOptions::default(),
    ))
    .unwrap();
    assert!(result.lane_state.operation.unwrap().step.is_none());
}

#[test]
fn ignores_unfulfilled_result_ids_from_earlier_attempts() {
    let target = message_target(
        "attempt-2-result",
        assistant_message(vec![AssistantContent::text("done")], StopReason::Stop),
    );
    let result = reduce_lane_state(&reduction_input(
        vec![
            run(1),
            attempt(2, "run-1", StepKind::Assistant, 1, "attempt-1-result", None),
            attempt(3, "run-1", StepKind::Assistant, 2, &target.id, None),
        ],
        vec![persisted(&target, 4, None)],
        ReductionOptions::default(),
    ))
    .unwrap();
    assert!(result.lane_state.operation.unwrap().step.is_none());
}

#[test]
fn reduces_tool_batch_state_across_the_dispatch_lifecycle() {
    let assistant = assistant_tools_entry();
    let tool_result = message_target("tool-result-1", tool_result_message("call-1", "tool-1"));

    // X1: attempt only, no dispatch.
    let x1 = reduce_lane_state(&reduction_input(
        vec![
            run(1),
            attempt(2, "run-1", StepKind::Assistant, 1, "assistant-tools", None),
        ],
        vec![assistant.clone()],
        ReductionOptions::default(),
    ))
    .unwrap();
    let batch = x1.lane_state.operation.unwrap().tool_batch.unwrap();
    assert_eq!(batch.assistant_entry_id, "assistant-tools");
    assert!(!batch.truncated);
    assert!(batch.unresolved);
    assert_eq!(batch.calls[0].tool_index, 0);
    assert_eq!(batch.calls[0].tool_call.id, "call-1");
    assert!(!batch.calls[0].result_exists);
    assert!(batch.calls[0].started.is_none());

    // X3: dispatched, no result yet.
    let x3 = reduce_lane_state(&reduction_input(
        vec![
            run(1),
            attempt(2, "run-1", StepKind::Assistant, 1, "assistant-tools", None),
            tool_started(4, ToolStartOverrides::default()),
        ],
        vec![assistant.clone()],
        ReductionOptions::default(),
    ))
    .unwrap();
    let batch = x3.lane_state.operation.unwrap().tool_batch.unwrap();
    assert!(batch.unresolved);
    assert!(batch.calls[0].started.is_some());

    // X5: result committed with a termination decision.
    let mut result_entry = persisted(&tool_result, 5, Some("assistant-tools"));
    if let EntryPayload::Message(message) = &mut result_entry.payload {
        message.terminate = Some(true);
    }
    let x5 = reduce_lane_state(&reduction_input(
        vec![
            run(1),
            attempt(2, "run-1", StepKind::Assistant, 1, "assistant-tools", None),
            tool_started(4, ToolStartOverrides::default()),
        ],
        vec![assistant, result_entry],
        ReductionOptions::default(),
    ))
    .unwrap();
    let batch = x5.lane_state.operation.unwrap().tool_batch.unwrap();
    assert!(!batch.unresolved);
    assert!(batch.calls[0].result_exists);
    assert_eq!(batch.calls[0].terminate, Some(true));
}

#[test]
fn does_not_resolve_a_tool_batch_from_a_deferred_write_result() {
    let assistant = assistant_tools_entry();
    let written = message_target(
        "written-tool-result",
        tool_result_message("call-1", "tool-1"),
    );
    let result = reduce_lane_state(&reduction_input(
        vec![
            run(1),
            attempt(2, "run-1", StepKind::Assistant, 1, &assistant.id, None),
            write_deferred(4, written.clone()),
        ],
        vec![
            assistant.clone(),
            persisted(&written, 5, Some(&assistant.id)),
        ],
        ReductionOptions::default(),
    ))
    .unwrap();

    let batch = result.lane_state.operation.unwrap().tool_batch.unwrap();
    assert!(!batch.calls[0].result_exists);
    assert!(batch.unresolved);
}

#[test]
fn matches_blocked_results_without_tool_start_and_preserves_order() {
    let assistant = persisted(
        &message_target(
            "assistant-two-tools",
            assistant_message(
                vec![tool_call("call-1", "tool-1"), tool_call("call-2", "tool-2")],
                StopReason::ToolUse,
            ),
        ),
        3,
        None,
    );
    let blocked = persisted(
        &message_target("blocked-result", tool_result_message("call-1", "tool-1")),
        4,
        Some(&assistant.id),
    );
    let second_start = tool_started(
        5,
        ToolStartOverrides {
            assistant_entry_id: "assistant-two-tools",
            tool_index: 1,
            tool_call_id: "call-2",
            tool_name: "tool-2",
            result_entry_id: "call-2-result",
        },
    );
    let result = reduce_lane_state(&reduction_input(
        vec![
            run(1),
            attempt(2, "run-1", StepKind::Assistant, 1, &assistant.id, None),
            second_start,
        ],
        vec![assistant, blocked],
        ReductionOptions::default(),
    ))
    .unwrap();

    let calls = result
        .lane_state
        .operation
        .unwrap()
        .tool_batch
        .unwrap()
        .calls;
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].tool_call.id, "call-1");
    assert!(calls[0].result_exists);
    assert_eq!(calls[1].tool_call.id, "call-2");
    assert!(calls[1].started.is_some());
    assert!(!calls[1].result_exists);
}

#[test]
fn marks_a_length_stopped_tool_batch_as_truncated() {
    let truncated = persisted(
        &message_target(
            "assistant-truncated",
            assistant_message(vec![tool_call("call-1", "tool-1")], StopReason::Length),
        ),
        3,
        None,
    );
    let result = reduce_lane_state(&reduction_input(
        vec![
            run(1),
            attempt(2, "run-1", StepKind::Assistant, 1, &truncated.id, None),
        ],
        vec![truncated],
        ReductionOptions::default(),
    ))
    .unwrap();
    let batch = result.lane_state.operation.unwrap().tool_batch.unwrap();
    assert!(batch.truncated);
    assert!(batch.unresolved);
}

#[test]
fn detects_an_unredeemed_deferred_handle_only_at_the_operation_tail() {
    let deferred_message = assistant_message(vec![], StopReason::Deferred);
    let deferred_entry = persisted(
        &message_target("assistant-deferred", deferred_message.clone()),
        3,
        None,
    );
    let pending = reduce_lane_state(&reduction_input(
        vec![
            run(1),
            attempt(2, "run-1", StepKind::Assistant, 1, &deferred_entry.id, None),
        ],
        vec![deferred_entry.clone()],
        ReductionOptions::default(),
    ))
    .unwrap();
    assert_eq!(
        pending.lane_state.operation.unwrap().deferred,
        deferred_message.as_assistant().unwrap().deferred.clone()
    );

    let successor = persisted(
        &message_target(
            "assistant-ready",
            assistant_message(vec![AssistantContent::text("ready")], StopReason::Stop),
        ),
        4,
        Some(&deferred_entry.id),
    );
    let redeemed = reduce_lane_state(&reduction_input(
        vec![
            run(1),
            attempt(2, "run-1", StepKind::Assistant, 1, &deferred_entry.id, None),
        ],
        vec![deferred_entry, successor],
        ReductionOptions::default(),
    ))
    .unwrap();
    assert!(redeemed.lane_state.operation.unwrap().deferred.is_none());
}

#[test]
fn derives_terminal_failure_provenance() {
    // From a step attempt.
    let step = reduce_lane_state(&reduction_input(
        vec![
            run(1),
            attempt(2, "run-1", StepKind::Assistant, 1, "assistant-error", None),
        ],
        vec![persisted(
            &message_target("assistant-error", assistant_error("failed")),
            3,
            None,
        )],
        ReductionOptions::default(),
    ))
    .unwrap();
    assert_eq!(
        step.terminal_failure.unwrap().source,
        TerminalFailureSource::Step
    );

    // From a deferred fetch, inferred from the preceding deferred entry.
    let deferred_entry = persisted(
        &message_target(
            "assistant-deferred",
            assistant_message(vec![], StopReason::Deferred),
        ),
        3,
        None,
    );
    let fetch = reduce_lane_state(&reduction_input(
        vec![
            run(1),
            attempt(
                2,
                "run-1",
                StepKind::Assistant,
                1,
                "assistant-deferred",
                None,
            ),
        ],
        vec![
            deferred_entry.clone(),
            persisted(
                &message_target("deferred-error", assistant_error("expired")),
                4,
                Some(&deferred_entry.id),
            ),
        ],
        ReductionOptions::default(),
    ))
    .unwrap();
    assert_eq!(
        fetch.terminal_failure.unwrap().source,
        TerminalFailureSource::DeferredFetch
    );

    // From an explicit deferred_fetch usage record.
    let usage_provenance = reduce_lane_state(&reduction_input(
        vec![
            run(1),
            usage_record(
                3,
                "deferred-error",
                UsageCause::DeferredFetch,
                StopReason::Error,
            ),
        ],
        vec![persisted(
            &message_target("deferred-error", assistant_error("expired")),
            2,
            None,
        )],
        ReductionOptions::default(),
    ))
    .unwrap();
    assert_eq!(
        usage_provenance.terminal_failure.unwrap().source,
        TerminalFailureSource::DeferredFetch
    );
}

#[test]
fn an_error_shaped_deferred_write_is_not_a_terminal_failure() {
    let target = message_target("written-error", assistant_error("note"));
    let result = reduce_lane_state(&reduction_input(
        vec![run(1), write_deferred(2, target.clone())],
        vec![persisted(&target, 3, None)],
        ReductionOptions::default(),
    ))
    .unwrap();
    assert!(result.terminal_failure.is_none());
}

#[test]
fn derives_structural_target_state() {
    let cases: Vec<(Vec<LaneRecord>, Vec<Entry>, OperationTargets)> = vec![
        (
            vec![compaction_started(1, "compaction-1")],
            vec![],
            OperationTargets {
                result: Some(false),
                summary: None,
            },
        ),
        (
            vec![compaction_started(1, "compaction-1")],
            vec![compaction_entry("compaction-1", 2)],
            OperationTargets {
                result: Some(true),
                summary: None,
            },
        ),
        (
            vec![navigation_started(1, Some("summary-1"))],
            vec![],
            OperationTargets {
                result: None,
                summary: Some(false),
            },
        ),
        (
            vec![navigation_started(1, Some("summary-1"))],
            vec![branch_summary_entry("summary-1", 2)],
            OperationTargets {
                result: None,
                summary: Some(true),
            },
        ),
    ];
    for (records, entries, expected) in cases {
        let result = reduce_lane_state(&reduction_input(
            records,
            entries,
            ReductionOptions::default(),
        ))
        .unwrap();
        assert_eq!(result.lane_state.operation.unwrap().targets, expected);
    }
}

#[test]
fn resets_the_overflow_guard_only_after_newer_input_is_consumed() {
    let initial = message_target("initial", user_message("initial"));
    let steer = message_target("steer", user_message("steer"));
    let records = vec![
        run_started(1, "run-1", vec![initial.clone()]),
        attempt(
            3,
            "run-1",
            StepKind::Compaction,
            1,
            "overflow-summary",
            Some(CompactionReason::Overflow),
        ),
        queue_enqueued(5, steer.clone(), QueueKind::Steer),
    ];
    let initial_entry = persisted(&initial, 2, None);

    let used = reduce_lane_state(&reduction_input(
        records.clone(),
        vec![initial_entry.clone()],
        ReductionOptions::default(),
    ))
    .unwrap();
    assert!(used.lane_state.operation.unwrap().overflow_recovery_used);

    let reset = reduce_lane_state(&reduction_input(
        records,
        vec![initial_entry, persisted(&steer, 6, Some(&initial.id))],
        ReductionOptions::default(),
    ))
    .unwrap();
    assert!(!reset.lane_state.operation.unwrap().overflow_recovery_used);
}

#[test]
fn reduction_is_deterministic_and_does_not_alias_its_inputs() {
    let pending = message_target("next", user_message("next"));
    let input = reduction_input(
        vec![queue_enqueued(1, pending, QueueKind::NextRun)],
        vec![],
        ReductionOptions::default(),
    );
    let before = format!("{:?}", input.slice);

    let first = reduce_lane_state(&input).unwrap();
    let second = reduce_lane_state(&input).unwrap();
    assert_eq!(first, second);
    assert_eq!(format!("{:?}", input.slice), before);

    let mut mutated = first;
    mutated.lane_state.pending_next_run[0].id = "mutated-output".into();
    assert_eq!(
        input.slice.records[0]
            .as_queue_enqueued()
            .unwrap()
            .target
            .id,
        "next"
    );
}

#[test]
fn message_entry_helper_types_are_reachable() {
    // `MessageEntry` is part of the public payload surface the reducer reads.
    let entry = MessageEntry {
        message: user_message("x"),
        terminate: None,
    };
    assert!(entry.terminate.is_none());
}
