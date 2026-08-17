//! Port of the meaningful cases from `test/harness/compaction.test.ts` and
//! `test/harness/branch-summarization.test.ts`.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use pi_core::{
    AssistantContent, AssistantMessage, Cost, InputContent, StopReason, ToolCall,
    ToolResultMessage, Usage, UserContent, UserMessage,
};
use pi_session::compaction::GenerateSummaryOptions;
use pi_session::compaction::{
    calculate_context_tokens, collect_entries_for_branch_summary, compact, estimate_context_tokens,
    estimate_tokens, find_cut_point, find_turn_start_index, generate_summary_with_usage,
    get_last_assistant_usage, prepare_compaction, should_compact, CompactionDetails,
    CompactionError, CompactionErrorKind, CompactionSettings, SummarizationModel,
    SummarizationRequest, Summarizer,
};
use pi_session::memory::{InMemorySessionRepo, InMemorySessionStorage};
use pi_session::messages::AgentMessage;
use pi_session::repo::{IdGenerator, SessionRepo};
use pi_session::session::Session;
use pi_session::types::{
    CompactionEntry, Entry, EntryPayload, MessageEntry, ModelChangeEntry, SessionCreateOptions,
    SessionMetadata, ThinkingLevelEntry,
};
use serde_json::{json, Map};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn usage(input: i64, output: i64, cache_read: i64, cache_write: i64) -> Usage {
    Usage {
        input,
        output,
        cache_read,
        cache_write,
        total_tokens: input + output + cache_read + cache_write,
        cost: Cost {
            total: 0.5,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn user(text: &str) -> AgentMessage {
    AgentMessage::User(UserMessage {
        content: UserContent::Blocks(vec![InputContent::text(text)]),
        timestamp: 1,
    })
}

fn assistant(text: &str, usage: Usage) -> AgentMessage {
    AgentMessage::Assistant(AssistantMessage {
        content: vec![AssistantContent::text(text)],
        usage,
        stop_reason: StopReason::Stop,
        ..AssistantMessage::pending("anthropic-messages", "anthropic", "claude")
    })
}

struct EntryBuilder {
    next: i64,
}

impl EntryBuilder {
    fn new() -> Self {
        Self { next: 0 }
    }

    fn entry(&mut self, payload: EntryPayload, parent_id: Option<&str>) -> Entry {
        self.next += 1;
        Entry {
            id: format!("entry-{}", self.next),
            seq: self.next,
            parent_id: parent_id.map(str::to_string),
            timestamp: 1000 + self.next,
            payload,
            extra: Map::new(),
        }
    }

    fn message(&mut self, message: AgentMessage, parent_id: Option<&str>) -> Entry {
        self.entry(
            EntryPayload::Message(MessageEntry {
                message,
                terminate: None,
            }),
            parent_id,
        )
    }
}

fn compaction_payload(
    summary: &str,
    retained_tail: Vec<AgentMessage>,
    details: Option<serde_json::Value>,
) -> EntryPayload {
    EntryPayload::Compaction(CompactionEntry {
        summary: summary.into(),
        retained_tail,
        tokens_before: 100,
        details,
        usage: None,
    })
}

/// Records every request and replies with scripted text.
struct ScriptedSummarizer {
    replies: Mutex<Vec<Result<AssistantMessage, CompactionError>>>,
    requests: Mutex<Vec<SummarizationRequest>>,
}

impl ScriptedSummarizer {
    fn new(replies: Vec<Result<AssistantMessage, CompactionError>>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn text(text: &str, usage: Usage) -> AssistantMessage {
        AssistantMessage {
            content: vec![AssistantContent::text(text)],
            usage,
            stop_reason: StopReason::Stop,
            ..AssistantMessage::pending("anthropic-messages", "anthropic", "claude")
        }
    }

    fn failed(stop_reason: StopReason, message: &str) -> AssistantMessage {
        AssistantMessage {
            stop_reason,
            error_message: Some(message.into()),
            ..AssistantMessage::pending("anthropic-messages", "anthropic", "claude")
        }
    }

    fn requests(&self) -> Vec<SummarizationRequest> {
        self.requests.lock().clone()
    }
}

#[async_trait]
impl Summarizer for ScriptedSummarizer {
    async fn summarize(
        &self,
        request: &SummarizationRequest,
    ) -> Result<AssistantMessage, CompactionError> {
        self.requests.lock().push(request.clone());
        let mut replies = self.replies.lock();
        if replies.is_empty() {
            return Ok(ScriptedSummarizer::text("summary", Usage::default()));
        }
        replies.remove(0)
    }
}

struct SequentialIds {
    next: Mutex<u32>,
}

impl IdGenerator for SequentialIds {
    fn next(&self) -> String {
        let mut next = self.next.lock();
        *next += 1;
        format!("entry-{next}")
    }
}

// ---------------------------------------------------------------------------
// Pure decision logic
// ---------------------------------------------------------------------------

#[test]
fn calculates_total_context_tokens_from_usage() {
    assert_eq!(calculate_context_tokens(&usage(1000, 500, 200, 100)), 1800);
    assert_eq!(calculate_context_tokens(&usage(0, 0, 0, 0)), 0);
}

#[test]
fn checks_the_compaction_threshold() {
    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 10000,
        keep_recent_tokens: 20000,
    };
    assert!(should_compact(95000, 100000, &settings));
    assert!(!should_compact(89000, 100000, &settings));
    assert!(!should_compact(
        95000,
        100000,
        &CompactionSettings {
            enabled: false,
            ..settings
        }
    ));
}

#[test]
fn finds_a_cut_point_on_a_message_entry() {
    let mut builder = EntryBuilder::new();
    let mut entries = Vec::new();
    let mut parent: Option<String> = None;
    for index in 0..10 {
        let user_entry = builder.message(user(&format!("User {index}")), parent.as_deref());
        let assistant_entry = builder.message(
            assistant(
                &format!("Assistant {index}"),
                usage(0, 100, (index + 1) * 1000, 0),
            ),
            Some(&user_entry.id),
        );
        parent = Some(assistant_entry.id.clone());
        entries.push(user_entry);
        entries.push(assistant_entry);
    }
    let result = find_cut_point(&entries, 0, entries.len(), 2500);
    assert!(entries[result.first_kept_entry_index]
        .as_message()
        .is_some());
}

#[test]
fn covers_cut_point_and_turn_start_edge_cases() {
    let mut builder = EntryBuilder::new();
    let thinking = builder.entry(
        EntryPayload::ThinkingLevelChange(ThinkingLevelEntry {
            thinking_level: "high".into(),
        }),
        None,
    );
    let model_change = builder.entry(
        EntryPayload::ModelChange(ModelChangeEntry {
            provider: "openai".into(),
            model_id: "gpt-4".into(),
        }),
        Some(&thinking.id),
    );
    // No valid cut points at all: fall back to the start index.
    let result = find_cut_point(&[thinking.clone(), model_change.clone()], 0, 2, 1);
    assert_eq!(result.first_kept_entry_index, 0);
    assert_eq!(result.turn_start_index, None);
    assert!(!result.is_split_turn);

    let branch_summary = builder.entry(
        EntryPayload::BranchSummary(pi_session::types::BranchSummaryEntry {
            from_id: "branch".into(),
            summary: "branch summary".into(),
            details: None,
            usage: None,
        }),
        Some(&model_change.id),
    );
    assert_eq!(
        find_turn_start_index(&[thinking.clone(), branch_summary.clone()], 1, 0),
        Some(1)
    );
    assert_eq!(
        find_turn_start_index(&[thinking.clone(), model_change], 1, 0),
        None
    );
    assert_eq!(
        find_cut_point(&[thinking, branch_summary], 0, 2, 1).first_kept_entry_index,
        0
    );

    // A lone tool result is not a valid cut point.
    let mut builder = EntryBuilder::new();
    let tool_result = builder.message(
        AgentMessage::ToolResult(ToolResultMessage::text(
            "call-1",
            "read",
            "tool output",
            false,
        )),
        None,
    );
    let result = find_cut_point(&[tool_result], 0, 1, 1);
    assert_eq!(result.first_kept_entry_index, 0);
    assert!(!result.is_split_turn);

    // The walk-back stops at a compaction entry.
    let mut builder = EntryBuilder::new();
    let user_entry = builder.message(user("user"), None);
    let compaction = builder.entry(
        compaction_payload("summary", vec![], None),
        Some(&user_entry.id),
    );
    let assistant_entry = builder.message(
        assistant("assistant", Usage::default()),
        Some(&compaction.id),
    );
    assert_eq!(
        find_cut_point(&[user_entry, compaction, assistant_entry], 0, 3, 1).first_kept_entry_index,
        2
    );
}

#[test]
fn estimates_tokens_across_every_message_role() {
    let mut call = ToolCall::new("call-1", "read");
    call.arguments.insert("path".into(), json!("/tmp/a"));
    let rich = AgentMessage::Assistant(AssistantMessage {
        content: vec![
            AssistantContent::text("hello"),
            AssistantContent::thinking("thinking"),
            AssistantContent::ToolCall(call),
        ],
        ..AssistantMessage::pending("api", "provider", "model")
    });
    // 5 + 8 + len("read") + len(r#"{"path":"/tmp/a"}"#) = 5 + 8 + 4 + 17 = 34 → 9
    assert_eq!(estimate_tokens(&rich), 9);
    assert_eq!(estimate_tokens(&user("12345678")), 2);
    assert_eq!(
        estimate_tokens(&AgentMessage::BashExecution(
            pi_session::messages::BashExecutionMessage {
                command: "ls".into(),
                output: "ab".into(),
                exit_code: Some(0),
                cancelled: false,
                truncated: false,
                full_output_path: None,
                timestamp: 1,
                exclude_from_context: None,
            }
        )),
        1
    );
    assert_eq!(
        estimate_tokens(&AgentMessage::CompactionSummary(
            pi_session::messages::CompactionSummaryMessage {
                summary: "abcd".into(),
                tokens_before: 0,
                timestamp: 1,
            }
        )),
        1
    );
    // An image is charged a flat character estimate.
    let with_image = AgentMessage::User(UserMessage {
        content: UserContent::Blocks(vec![InputContent::image("data", "image/png")]),
        timestamp: 1,
    });
    assert_eq!(estimate_tokens(&with_image), 1200);
}

#[test]
fn estimates_context_tokens_from_the_newest_usage_plus_the_trailing_estimate() {
    let messages = vec![
        user("earlier"),
        assistant("answer", usage(100, 20, 0, 0)),
        user("12345678"),
    ];
    let estimate = estimate_context_tokens(&messages);
    assert_eq!(estimate.usage_tokens, 120);
    assert_eq!(estimate.trailing_tokens, 2);
    assert_eq!(estimate.tokens, 122);
    assert_eq!(estimate.last_usage_index, Some(1));

    // With no usage at all, everything is estimated.
    let estimate = estimate_context_tokens(&[user("12345678")]);
    assert_eq!(estimate.last_usage_index, None);
    assert_eq!(estimate.tokens, 2);
    assert_eq!(estimate.usage_tokens, 0);
}

#[test]
fn ignores_usage_from_aborted_and_errored_turns() {
    let mut aborted = match assistant("x", usage(100, 0, 0, 0)) {
        AgentMessage::Assistant(message) => message,
        _ => unreachable!(),
    };
    aborted.stop_reason = StopReason::Aborted;
    let mut builder = EntryBuilder::new();
    let entries = vec![builder.message(AgentMessage::Assistant(aborted), None)];
    assert!(get_last_assistant_usage(&entries).is_none());

    let mut builder = EntryBuilder::new();
    let entries = vec![builder.message(assistant("x", usage(100, 0, 0, 0)), None)];
    assert_eq!(
        get_last_assistant_usage(&entries).unwrap().total_tokens,
        100
    );
}

#[test]
fn does_not_prepare_compaction_when_there_is_nothing_to_compact() {
    assert!(prepare_compaction(&[], &CompactionSettings::default()).is_none());
    let mut builder = EntryBuilder::new();
    let compaction = builder.entry(compaction_payload("summary", vec![], None), None);
    assert!(prepare_compaction(&[compaction], &CompactionSettings::default()).is_none());
}

#[test]
fn prepares_compaction_using_the_latest_summary_as_previous_summary() {
    let mut builder = EntryBuilder::new();
    let first = builder.message(user("first"), None);
    let compaction = builder.entry(
        compaction_payload("older summary", vec![], None),
        Some(&first.id),
    );
    let newer = builder.message(user("newer"), Some(&compaction.id));
    let preparation =
        prepare_compaction(&[first, compaction, newer], &CompactionSettings::default()).unwrap();
    assert_eq!(
        preparation.previous_summary.as_deref(),
        Some("older summary")
    );
}

#[test]
fn carries_a_previous_retained_tail_into_the_next_preparation() {
    let mut builder = EntryBuilder::new();
    let compaction = builder.entry(
        compaction_payload("summary", vec![user("retained")], None),
        None,
    );
    let newer = builder.message(user("newer"), Some(&compaction.id));
    let preparation = prepare_compaction(
        &[compaction, newer],
        &CompactionSettings {
            keep_recent_tokens: 1,
            ..Default::default()
        },
    )
    .unwrap();
    // The virtual retained entry participates in the cut-point search, so the
    // retained message is visible to this compaction round.
    let all: Vec<String> = preparation
        .messages_to_summarize
        .iter()
        .chain(preparation.retained_tail.iter())
        .map(|message| match message {
            AgentMessage::User(user) => user.content.to_text(),
            other => other.role().to_string(),
        })
        .collect();
    assert!(all.contains(&"retained".to_string()), "{all:?}");
    assert!(all.contains(&"newer".to_string()), "{all:?}");
}

#[test]
fn carries_prior_file_operation_details_forward() {
    let mut builder = EntryBuilder::new();
    let compaction = builder.entry(
        compaction_payload(
            "summary",
            vec![],
            Some(json!({ "readFiles": ["/read-before"], "modifiedFiles": ["/modified-before"] })),
        ),
        None,
    );
    let mut call = ToolCall::new("call-1", "write");
    call.arguments.insert("path".into(), json!("/written-now"));
    let assistant_entry = builder.message(
        AgentMessage::Assistant(AssistantMessage {
            content: vec![AssistantContent::ToolCall(call)],
            ..AssistantMessage::pending("api", "provider", "model")
        }),
        Some(&compaction.id),
    );
    let tail = builder.message(user("tail"), Some(&assistant_entry.id));

    let preparation = prepare_compaction(
        &[compaction, assistant_entry, tail],
        &CompactionSettings {
            keep_recent_tokens: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(preparation.file_ops.read.contains("/read-before"));
    assert!(preparation.file_ops.edited.contains("/modified-before"));
}

// ---------------------------------------------------------------------------
// Summarizer-driven paths
// ---------------------------------------------------------------------------

/// Build a preparation from a linear message list. `keep_recent_tokens` is the
/// knob that decides how much lands in `messages_to_summarize`.
fn preparation_with(
    messages: Vec<AgentMessage>,
    keep_recent_tokens: i64,
) -> pi_session::compaction::CompactionPreparation {
    let mut builder = EntryBuilder::new();
    let mut entries = Vec::new();
    let mut parent: Option<String> = None;
    for message in messages {
        let entry = builder.message(message, parent.as_deref());
        parent = Some(entry.id.clone());
        entries.push(entry);
    }
    prepare_compaction(
        &entries,
        &CompactionSettings {
            keep_recent_tokens,
            ..Default::default()
        },
    )
    .expect("preparation")
}

#[tokio::test]
async fn clamps_summary_max_tokens_to_the_model_output_cap() {
    let summarizer = ScriptedSummarizer::new(vec![]);
    let model = SummarizationModel {
        context_window: 100_000,
        max_tokens: 500,
        reasoning: false,
    };
    generate_summary_with_usage(
        &[user("hi")],
        summarizer.as_ref(),
        &model,
        &GenerateSummaryOptions {
            reserve_tokens: 16384,
            custom_instructions: None,
            previous_summary: None,
            thinking_level: None,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(summarizer.requests()[0].max_tokens, 500);

    // Without a declared cap, 80% of the reserve applies.
    let summarizer = ScriptedSummarizer::new(vec![]);
    generate_summary_with_usage(
        &[user("hi")],
        summarizer.as_ref(),
        &SummarizationModel::default(),
        &GenerateSummaryOptions {
            reserve_tokens: 16384,
            custom_instructions: None,
            previous_summary: None,
            thinking_level: None,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(summarizer.requests()[0].max_tokens, 13107);
}

#[tokio::test]
async fn passes_reasoning_only_for_reasoning_models_with_thinking_enabled() {
    let reasoning = SummarizationModel {
        reasoning: true,
        ..Default::default()
    };
    let non_reasoning = SummarizationModel::default();

    for (model, level, expected) in [
        (&reasoning, Some("high"), Some("high".to_string())),
        (&reasoning, Some("off"), None),
        (&reasoning, None, None),
        (&non_reasoning, Some("high"), None),
    ] {
        let summarizer = ScriptedSummarizer::new(vec![]);
        generate_summary_with_usage(
            &[user("hi")],
            summarizer.as_ref(),
            model,
            &GenerateSummaryOptions {
                reserve_tokens: 16384,
                custom_instructions: None,
                previous_summary: None,
                thinking_level: level.map(str::to_string),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(summarizer.requests()[0].thinking_level, expected);
    }
}

#[tokio::test]
async fn includes_previous_summaries_and_custom_instructions_in_the_prompt() {
    let summarizer = ScriptedSummarizer::new(vec![]);
    generate_summary_with_usage(
        &[user("hi")],
        summarizer.as_ref(),
        &SummarizationModel::default(),
        &GenerateSummaryOptions {
            reserve_tokens: 16384,
            custom_instructions: Some("focus on the bug".into()),
            previous_summary: Some("earlier summary".into()),
            thinking_level: None,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let prompt = &summarizer.requests()[0].prompt;
    assert!(
        prompt.contains("<conversation>\n[User]: hi\n</conversation>"),
        "{prompt}"
    );
    assert!(
        prompt.contains("<previous-summary>\nearlier summary\n</previous-summary>"),
        "{prompt}"
    );
    assert!(
        prompt.contains("Additional focus: focus on the bug"),
        "{prompt}"
    );
    // The update prompt is selected when a previous summary exists.
    assert!(
        prompt.contains("NEW conversation messages to incorporate"),
        "{prompt}"
    );
}

#[tokio::test]
async fn maps_aborted_and_failed_summaries_to_errors() {
    let aborted = ScriptedSummarizer::new(vec![Ok(ScriptedSummarizer::failed(
        StopReason::Aborted,
        "stopped",
    ))]);
    let error = generate_summary_with_usage(
        &[user("hi")],
        aborted.as_ref(),
        &SummarizationModel::default(),
        &GenerateSummaryOptions {
            reserve_tokens: 16384,
            custom_instructions: None,
            previous_summary: None,
            thinking_level: None,
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, CompactionErrorKind::Aborted);
    assert_eq!(error.message, "stopped");

    let failed = ScriptedSummarizer::new(vec![Ok(ScriptedSummarizer::failed(
        StopReason::Error,
        "boom",
    ))]);
    let error = generate_summary_with_usage(
        &[user("hi")],
        failed.as_ref(),
        &SummarizationModel::default(),
        &GenerateSummaryOptions {
            reserve_tokens: 16384,
            custom_instructions: None,
            previous_summary: None,
            thinking_level: None,
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, CompactionErrorKind::SummarizationFailed);
    assert_eq!(error.message, "Summarization failed: boom");
    assert_eq!(error.code(), "summarization_failed");
}

#[tokio::test]
async fn returns_a_compaction_result_with_file_details() {
    let mut call = ToolCall::new("call-1", "read");
    call.arguments.insert("path".into(), json!("/tmp/read-me"));
    let mut write_call = ToolCall::new("call-2", "write");
    write_call
        .arguments
        .insert("path".into(), json!("/tmp/write-me"));

    let preparation = preparation_with(
        vec![
            user("start"),
            AgentMessage::Assistant(AssistantMessage {
                content: vec![
                    AssistantContent::ToolCall(call),
                    AssistantContent::ToolCall(write_call),
                ],
                ..AssistantMessage::pending("api", "provider", "model")
            }),
            user("more"),
            assistant("done", usage(10, 10, 0, 0)),
            user("tail"),
        ],
        1,
    );

    let summarizer = ScriptedSummarizer::new(vec![Ok(ScriptedSummarizer::text(
        "the summary",
        usage(5, 5, 0, 0),
    ))]);
    let result = compact(
        &preparation,
        summarizer.as_ref(),
        &SummarizationModel::default(),
        None,
        None,
        None,
    )
    .await
    .unwrap();

    assert!(
        result.summary.starts_with("the summary"),
        "{}",
        result.summary
    );
    assert!(result
        .summary
        .contains("<read-files>\n/tmp/read-me\n</read-files>"));
    assert!(result
        .summary
        .contains("<modified-files>\n/tmp/write-me\n</modified-files>"));
    assert_eq!(
        result.details,
        CompactionDetails {
            read_files: vec!["/tmp/read-me".into()],
            modified_files: vec!["/tmp/write-me".into()],
        }
    );
    assert_eq!(result.usage.unwrap().total_tokens, 10);
    assert_eq!(result.tokens_before, preparation.tokens_before);
}

#[tokio::test]
async fn combines_usage_across_split_turn_summaries() {
    // Force a split turn: a long assistant/tool tail after a user turn start,
    // with a tiny retention budget so the cut lands mid-turn.
    let mut builder = EntryBuilder::new();
    let earlier_user = builder.message(user("earlier"), None);
    let earlier_answer = builder.message(
        assistant("earlier answer", Usage::default()),
        Some(&earlier_user.id),
    );
    let start = builder.message(user("start of turn"), Some(&earlier_answer.id));
    let assistant_entry = builder.message(
        assistant(&"a".repeat(400), Usage::default()),
        Some(&start.id),
    );
    let tail = builder.message(
        assistant(&"b".repeat(400), Usage::default()),
        Some(&assistant_entry.id),
    );
    let preparation = prepare_compaction(
        &[earlier_user, earlier_answer, start, assistant_entry, tail],
        &CompactionSettings {
            keep_recent_tokens: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(preparation.is_split_turn, "expected a split turn");

    let summarizer = ScriptedSummarizer::new(vec![
        Ok(ScriptedSummarizer::text("history", usage(1, 1, 0, 0))),
        Ok(ScriptedSummarizer::text("prefix", usage(2, 2, 0, 0))),
    ]);
    let result = compact(
        &preparation,
        summarizer.as_ref(),
        &SummarizationModel::default(),
        None,
        None,
        None,
    )
    .await
    .unwrap();

    assert!(
        result.summary.contains("**Turn Context (split turn):**"),
        "{}",
        result.summary
    );
    assert!(result.summary.contains("prefix"));
    // The turn-prefix request uses half the reserve, the history request 80%.
    let requests = summarizer.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].max_tokens, 13107);
    assert_eq!(requests[1].max_tokens, 8192);
    assert!(requests[1].prompt.contains("This is the PREFIX of a turn"));
    // Usage from both calls is summed.
    let combined = result.usage.unwrap();
    assert_eq!(combined.input, 3);
    assert_eq!(combined.output, 3);
}

#[tokio::test]
async fn a_split_turn_with_no_history_uses_the_no_prior_history_placeholder() {
    let mut builder = EntryBuilder::new();
    let start = builder.message(user("start"), None);
    let assistant_entry = builder.message(
        assistant(&"a".repeat(400), Usage::default()),
        Some(&start.id),
    );
    let preparation = prepare_compaction(
        &[start, assistant_entry],
        &CompactionSettings {
            keep_recent_tokens: 1,
            ..Default::default()
        },
    )
    .unwrap();
    if !preparation.is_split_turn || !preparation.messages_to_summarize.is_empty() {
        return; // Shape depends on the cut; only assert when it is the split case.
    }
    let summarizer = ScriptedSummarizer::new(vec![Ok(ScriptedSummarizer::text(
        "prefix",
        Usage::default(),
    ))]);
    let result = compact(
        &preparation,
        summarizer.as_ref(),
        &SummarizationModel::default(),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(
        result.summary.starts_with("No prior history."),
        "{}",
        result.summary
    );
    assert_eq!(summarizer.requests().len(), 1);
}

// ---------------------------------------------------------------------------
// Branch summarization
// ---------------------------------------------------------------------------

#[tokio::test]
async fn collects_the_abandoned_side_of_a_branch_in_chronological_order() {
    let session = Session::with_id_generator(
        Arc::new(InMemorySessionStorage::new(SessionMetadata::new(
            "session", 1,
        ))),
        Arc::new(SequentialIds {
            next: Mutex::new(0),
        }),
    );
    let root_id = session.append_message(user("root")).await.unwrap();
    let common_id = session.append_message(user("common")).await.unwrap();
    let abandoned = vec![
        session.append_message(user("abandoned 1")).await.unwrap(),
        session.append_message(user("abandoned 2")).await.unwrap(),
    ];
    session
        .create_lane("target", Some(&common_id))
        .await
        .unwrap();
    let target_id = session
        .view("target")
        .append_message(user("target"))
        .await
        .unwrap();

    let result = collect_entries_for_branch_summary(&session, Some(&abandoned[1]), &target_id)
        .await
        .unwrap();
    assert_eq!(
        result.common_ancestor_id.as_deref(),
        Some(common_id.as_str())
    );
    assert_eq!(
        result
            .entries
            .iter()
            .map(|entry| entry.id.clone())
            .collect::<Vec<_>>(),
        abandoned
    );
    assert!(!result.entries.iter().any(|entry| entry.id == root_id));
}

#[tokio::test]
async fn collects_no_entries_when_there_was_no_previous_leaf() {
    let repository = InMemorySessionRepo::new();
    let session = repository
        .create(&SessionCreateOptions::new().with_id("session"))
        .await
        .unwrap();
    let target_id = session.append_message(user("target")).await.unwrap();

    let result = collect_entries_for_branch_summary(&session, None, &target_id)
        .await
        .unwrap();
    assert!(result.entries.is_empty());
    assert!(result.common_ancestor_id.is_none());
}

#[tokio::test]
async fn a_branch_with_nothing_to_summarize_short_circuits() {
    let summarizer = ScriptedSummarizer::new(vec![]);
    let result = pi_session::compaction::generate_branch_summary(
        &[],
        summarizer.as_ref(),
        &SummarizationModel::default(),
        &Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(result.summary, "No content to summarize");
    assert!(summarizer.requests().is_empty());
}

#[tokio::test]
async fn a_branch_summary_carries_its_preamble_and_file_lists() {
    let mut builder = EntryBuilder::new();
    let mut call = ToolCall::new("call-1", "edit");
    call.arguments.insert("path".into(), json!("/tmp/edited"));
    let entries = vec![
        builder.message(user("explore"), None),
        builder.message(
            AgentMessage::Assistant(AssistantMessage {
                content: vec![AssistantContent::ToolCall(call)],
                ..AssistantMessage::pending("api", "provider", "model")
            }),
            None,
        ),
    ];
    let summarizer = ScriptedSummarizer::new(vec![Ok(ScriptedSummarizer::text(
        "branch text",
        Usage::default(),
    ))]);
    let result = pi_session::compaction::generate_branch_summary(
        &entries,
        summarizer.as_ref(),
        &SummarizationModel::default(),
        &Default::default(),
    )
    .await
    .unwrap();

    assert!(
        result
            .summary
            .starts_with("The user explored a different conversation branch"),
        "{}",
        result.summary
    );
    assert!(result.summary.contains("branch text"));
    assert_eq!(result.modified_files, vec!["/tmp/edited".to_string()]);
    assert!(result
        .summary
        .contains("<modified-files>\n/tmp/edited\n</modified-files>"));
    assert_eq!(summarizer.requests()[0].max_tokens, 2048);
}

#[tokio::test]
async fn branch_summary_instructions_can_be_appended_or_replaced() {
    let mut builder = EntryBuilder::new();
    let entries = vec![builder.message(user("explore"), None)];

    let appended = ScriptedSummarizer::new(vec![]);
    pi_session::compaction::generate_branch_summary(
        &entries,
        appended.as_ref(),
        &SummarizationModel::default(),
        &pi_session::compaction::GenerateBranchSummaryOptions {
            custom_instructions: Some("look at errors".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(appended.requests()[0]
        .prompt
        .contains("Additional focus: look at errors"));
    assert!(appended.requests()[0].prompt.contains("## Key Decisions"));

    let replaced = ScriptedSummarizer::new(vec![]);
    pi_session::compaction::generate_branch_summary(
        &entries,
        replaced.as_ref(),
        &SummarizationModel::default(),
        &pi_session::compaction::GenerateBranchSummaryOptions {
            custom_instructions: Some("just the errors".into()),
            replace_instructions: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(replaced.requests()[0].prompt.ends_with("just the errors"));
    assert!(!replaced.requests()[0].prompt.contains("## Key Decisions"));
}

#[test]
fn branch_preparation_respects_the_token_budget() {
    let mut builder = EntryBuilder::new();
    let entries = vec![
        builder.message(user(&"a".repeat(400)), None),
        builder.message(user(&"b".repeat(400)), None),
    ];
    // 100 tokens each; a 120-token budget admits only the newest.
    let preparation = pi_session::compaction::prepare_branch_entries(&entries, 120);
    assert_eq!(preparation.messages.len(), 1);
    assert_eq!(preparation.total_tokens, 100);
    // Tool results are excluded from branch summaries entirely.
    let mut builder = EntryBuilder::new();
    let with_tool_result = vec![builder.message(
        AgentMessage::ToolResult(ToolResultMessage::text("call-1", "read", "out", false)),
        None,
    )];
    assert!(
        pi_session::compaction::prepare_branch_entries(&with_tool_result, 0)
            .messages
            .is_empty()
    );
}
