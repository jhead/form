//! Backend conformance suite. Port of `session/testing/conformance.ts`.
//!
//! These cases define what "a session backend" means. They are compiled into
//! the library (not behind `#[cfg(test)]`) so other crates can run them: W12's
//! `pi-session-sqlite` must pass the same suite the in-memory and JSONL
//! backends pass here.
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use pi_session::memory::InMemorySessionRepo;
//! # use pi_session::testing::{ConformanceFixture, run_session_backend_conformance};
//! # async fn example() {
//! run_session_backend_conformance(&|| {
//!     Box::pin(async {
//!         ConformanceFixture::new(Arc::new(InMemorySessionRepo::new()))
//!     })
//! })
//! .await;
//! # }
//! ```
//!
//! Cases assert with `assert!`/`panic!`, so a failure surfaces as a normal test
//! panic naming the case.

use std::any::Any;
use std::sync::Arc;

use futures::future::BoxFuture;
use pi_core::{
    AssistantContent, Cost, InputContent, StopReason, ToolResultMessage, Usage, UserContent,
    UserMessage,
};
use serde_json::json;

use crate::error::{SessionError, SessionResult};
use crate::messages::AgentMessage;
use crate::repo::SessionRepo;
use crate::session::Session;
use crate::types::{
    BranchQuery, CompactionEntry, CustomEntry, Entry, EntryOrder, EntryPayload, EntryQuery,
    EntryType, ForkOptions, ForkPosition, ForkScope, LanePointer, LogItem, LogOptions,
    MessageEntry, NewRecord, OperationFinishedRecord, OperationIntent, OperationKind,
    OperationOutcome, OperationStartedRecord, ProvisionedEntry, QueueCancelledRecord,
    QueueEnqueuedRecord, QueueKind, RecordPayload, RecordQuery, RecordType, RunIntent,
    SessionCreateOptions, SessionListOptions, SessionStats, StepAttemptRecord, StepKind,
    ToolReplay, ToolStartedRecord, UsageCause, UsageRecord,
};

/// A fresh backend instance owned by one conformance case.
pub struct ConformanceFixture {
    pub repository: Arc<dyn SessionRepo>,
    /// Defaults merged into every `create`/`fork`. Backends that require a
    /// `cwd` (JSONL, SQLite) put a temp directory here.
    pub defaults: SessionCreateOptions,
    /// Kept alive for the duration of the case: temp-dir guards and the like.
    pub guard: Option<Box<dyn Any + Send + Sync>>,
}

impl ConformanceFixture {
    pub fn new(repository: Arc<dyn SessionRepo>) -> Self {
        Self {
            repository,
            defaults: SessionCreateOptions::default(),
            guard: None,
        }
    }

    pub fn with_defaults(mut self, defaults: SessionCreateOptions) -> Self {
        self.defaults = defaults;
        self
    }

    pub fn with_guard(mut self, guard: Box<dyn Any + Send + Sync>) -> Self {
        self.guard = Some(guard);
        self
    }

    fn options(&self, id: &str) -> SessionCreateOptions {
        let mut options = self.defaults.clone();
        options.id = Some(id.to_string());
        options
    }

    async fn create(&self, id: &str) -> Session {
        self.repository
            .create(&self.options(id))
            .await
            .unwrap_or_else(|error| panic!("failed to create session {id}: {error}"))
    }
}

/// Creates an isolated fixture for one conformance case.
pub type ConformanceFixtureFactory =
    dyn Fn() -> BoxFuture<'static, ConformanceFixture> + Send + Sync;

/// A runner-independent case that can be registered with any test framework.
pub struct SessionBackendConformanceCase {
    pub group: &'static str,
    pub name: &'static str,
    run: fn(ConformanceFixture) -> BoxFuture<'static, ()>,
}

impl SessionBackendConformanceCase {
    /// Build a fresh fixture and run this case against it.
    pub async fn run(&self, factory: &ConformanceFixtureFactory) {
        let fixture = factory().await;
        (self.run)(fixture).await;
    }
}

impl std::fmt::Debug for SessionBackendConformanceCase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} / {}", self.group, self.name)
    }
}

/// Run every case in order, each against its own fixture.
pub async fn run_session_backend_conformance(factory: &ConformanceFixtureFactory) {
    for case in session_backend_conformance_cases() {
        case.run(factory).await;
    }
}

macro_rules! cases {
    ($($group:literal, $name:literal => $function:ident;)*) => {
        /// Every conformance case, in upstream's order.
        pub fn session_backend_conformance_cases() -> Vec<SessionBackendConformanceCase> {
            vec![$(SessionBackendConformanceCase {
                group: $group,
                name: $name,
                run: |fixture| Box::pin($function(fixture)),
            }),*]
        }
    };
}

cases! {
    "entries and lanes", "assigns parents and one sequence across every mutation" => assigns_parents_and_one_sequence;
    "records and log", "commits records and lane moves as separate mutations" => commits_records_and_lane_moves;
    "entries and lanes", "rejects duplicate ids without changing state" => rejects_duplicate_ids;
    "entries and lanes", "isolates lanes while sharing the tree" => isolates_lanes;
    "queries and facts", "rejects invalid queries before empty reads" => rejects_invalid_queries;
    "queries and facts", "supports bounded filtered and cursor-based queries" => bounded_filtered_queries;
    "records and log", "keeps lane names permanent with their recovery records" => lane_names_are_permanent;
    "records and log", "persists queue cancellation without consuming its target" => queue_cancellation;
    "records and log", "filters records by lane type run sequence and order" => filters_records;
    "records and log", "filters operation starts by operation kind" => filters_by_operation_kind;
    "records and log", "tracks and enforces one open operation per lane" => one_open_operation_per_lane;
    "records and log", "does not let an earlier finish close a later start" => earlier_finish_does_not_close;
    "records and log", "scopes open operations by lane and limit" => scopes_open_operations;
    "validation and immutability", "returns immutable open-operation records" => immutable_open_operations;
    "queries and facts", "keeps latest-value facts and computes ledger statistics" => facts_and_statistics;
    "queries and facts", "clears session names durably" => clears_session_names;
    "validation and immutability", "returns immutable copies from reads" => immutable_reads;
    "entries and lanes", "validates lane lifecycle and targets" => validates_lane_lifecycle;
    "entries and lanes", "binds lane views without caching leaves" => lane_views_do_not_cache_leaves;
    "entries and lanes", "appends provisioned entries with their existing ids" => appends_provisioned_entries;
    "entries and lanes", "persists tool-result termination decisions" => persists_termination;
    "validation and immutability", "rejects non-JSON entries before storage mutation" => rejects_non_json_entries;
    "validation and immutability", "rejects non-JSON records before storage mutation" => rejects_non_json_records;
    "entries and lanes", "linearizes concurrent writes across two lanes" => linearizes_concurrent_writes;
    "repository and forks", "creates lists and opens sessions" => creates_lists_and_opens;
    "repository and forks", "deletes sessions idempotently" => deletes_idempotently;
    "repository and forks", "forks one branch with selected facts and no records" => forks_one_branch;
    "repository and forks", "forks a complete tree with lanes and facts" => forks_a_tree;
    "repository and forks", "forks before an entry without modifying the source" => forks_before_an_entry;
    "repository and forks", "validates the default fork target" => validates_default_fork_target;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn user_message(text: &str) -> AgentMessage {
    AgentMessage::User(UserMessage {
        content: UserContent::Blocks(vec![InputContent::text(text)]),
        timestamp: 1,
    })
}

fn assistant_message(text: &str) -> AgentMessage {
    AgentMessage::Assistant(pi_core::AssistantMessage {
        content: vec![AssistantContent::text(text)],
        api: "anthropic-messages".into(),
        provider: "anthropic".into(),
        model: "claude-sonnet-4-5".into(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1,
    })
}

fn message_entry(id: &str, message: AgentMessage) -> ProvisionedEntry {
    ProvisionedEntry::message(id, message)
}

fn custom_entry(id: &str, custom_type: &str, data: Option<serde_json::Value>) -> ProvisionedEntry {
    ProvisionedEntry::custom(id, custom_type, data)
}

fn operation_started(id: &str, lane: &str, kind: OperationKind) -> NewRecord {
    let intent = match kind {
        OperationKind::Run => OperationIntent::Run(RunIntent {
            original_prompt: vec![],
            initial_messages: vec![],
            system_prompt_override: None,
            resume_data: None,
        }),
        OperationKind::Compaction => OperationIntent::Compaction(crate::types::CompactionIntent {
            custom_instructions: None,
            result_entry_id: format!("{id}-result"),
        }),
        OperationKind::Navigation => OperationIntent::Navigation(crate::types::NavigationIntent {
            target_id: None,
            summarize: false,
            custom_instructions: None,
            label: None,
            summary_entry_id: None,
        }),
    };
    NewRecord::new(
        id,
        lane,
        RecordPayload::OperationStarted(OperationStartedRecord {
            source_leaf_id: None,
            intent,
        }),
    )
}

fn operation_finished(id: &str, lane: &str, run_id: &str) -> NewRecord {
    NewRecord::new(
        id,
        lane,
        RecordPayload::OperationFinished(OperationFinishedRecord {
            run_id: run_id.into(),
            outcome: OperationOutcome::Completed,
            error: None,
        }),
    )
}

fn ids(entries: &[Entry]) -> Vec<String> {
    entries.iter().map(|entry| entry.id.clone()).collect()
}

fn record_ids(records: &[crate::types::LaneRecord]) -> Vec<String> {
    records.iter().map(|record| record.id.clone()).collect()
}

fn lane(name: &str, leaf: Option<&str>) -> LanePointer {
    LanePointer {
        lane: name.into(),
        leaf_id: leaf.map(str::to_string),
    }
}

#[track_caller]
fn expect_code<T: std::fmt::Debug>(result: SessionResult<T>, code: &str) {
    match result {
        Err(error) => assert_eq!(error.code(), code, "wrong error code: {error}"),
        Ok(value) => panic!("expected a {code} error, got {value:?}"),
    }
}

fn usage_with_cost_total(total: f64) -> Usage {
    Usage {
        cost: Cost {
            total,
            ..Default::default()
        },
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------

async fn assigns_parents_and_one_sequence(fixture: ConformanceFixture) {
    let session = fixture.create("session").await;
    let root = session
        .append_entry(&message_entry("root", user_message("root")), "main")
        .await
        .unwrap();
    session.create_lane("thread", Some(&root.id)).await.unwrap();
    let child = session
        .append_entry(
            &custom_entry("child", "note", Some(json!({ "value": 1 }))),
            "thread",
        )
        .await
        .unwrap();
    let record = session
        .append_record(&operation_started("run", "thread", OperationKind::Run))
        .await
        .unwrap();
    session.set_name(Some("Example")).await.unwrap();
    session
        .set_label(&root.id, Some("checkpoint"))
        .await
        .unwrap();
    session.move_lane("main", Some(&child.id)).await.unwrap();

    assert_eq!((root.parent_id.clone(), root.seq), (None, 1));
    assert_eq!(
        (child.parent_id.clone(), child.seq),
        (Some("root".into()), 3)
    );
    assert_eq!(record.seq, 4);
    for timestamp in [root.timestamp, child.timestamp, record.timestamp] {
        assert!(
            timestamp >= 0,
            "storage-assigned timestamps must be Unix milliseconds"
        );
    }
    let log = session.get_log(&LogOptions::default()).await.unwrap();
    assert_eq!(
        log.iter()
            .map(|item| (item.kind(), item.seq()))
            .collect::<Vec<_>>(),
        vec![
            ("entry", 1),
            ("lane", 2),
            ("entry", 3),
            ("record", 4),
            ("fact", 5),
            ("fact", 6),
            ("lane", 7),
        ]
    );
    assert_eq!(
        session.get_lanes().await.unwrap(),
        vec![lane("main", Some("child")), lane("thread", Some("child"))]
    );
}

async fn commits_records_and_lane_moves(fixture: ConformanceFixture) {
    let session = fixture.create("session").await;
    let root = session
        .append_entry(&message_entry("root", user_message("root")), "main")
        .await
        .unwrap();
    let finished = session
        .append_record(&operation_finished("finish", "main", "run"))
        .await
        .unwrap();

    assert_eq!(finished.seq, 2);
    assert_eq!(
        session.get_lanes().await.unwrap(),
        vec![lane("main", Some("root"))]
    );
    session.move_lane("main", None).await.unwrap();
    assert_eq!(session.get_lanes().await.unwrap(), vec![lane("main", None)]);
    assert_eq!(
        session.get_log(&LogOptions::default()).await.unwrap(),
        vec![
            LogItem::Entry {
                seq: 1,
                entry: root
            },
            LogItem::Record {
                seq: 2,
                record: finished
            },
            LogItem::Lane {
                seq: 3,
                lane: "main".into(),
                leaf_id: None
            },
        ]
    );

    expect_code(
        session.move_lane("main", Some("missing")).await,
        "not_found",
    );
    assert_eq!(
        session
            .find_records(&RecordQuery::new())
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        session
            .get_log(&LogOptions::default())
            .await
            .unwrap()
            .iter()
            .map(LogItem::seq)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}

async fn rejects_duplicate_ids(fixture: ConformanceFixture) {
    let session = fixture.create("session").await;
    session
        .append_entry(&message_entry("shared", user_message("root")), "main")
        .await
        .unwrap();
    expect_code(
        session
            .append_record(&operation_started("shared", "main", OperationKind::Run))
            .await,
        "already_exists",
    );
    session
        .append_record(&operation_started("run", "main", OperationKind::Run))
        .await
        .unwrap();
    expect_code(
        session
            .append_entry(&custom_entry("run", "note", None), "main")
            .await,
        "already_exists",
    );
    assert_eq!(
        session
            .get_log(&LogOptions::default())
            .await
            .unwrap()
            .iter()
            .map(LogItem::seq)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}

async fn isolates_lanes(fixture: ConformanceFixture) {
    let session = fixture.create("session").await;
    session
        .append_entry(&message_entry("root", user_message("root")), "main")
        .await
        .unwrap();
    session.create_lane("thread", Some("root")).await.unwrap();
    session
        .append_entry(&message_entry("main-child", user_message("main")), "main")
        .await
        .unwrap();
    session
        .append_entry(
            &message_entry("thread-child", user_message("thread")),
            "thread",
        )
        .await
        .unwrap();

    assert_eq!(
        session.get_lanes().await.unwrap(),
        vec![
            lane("main", Some("main-child")),
            lane("thread", Some("thread-child"))
        ]
    );
    let main_branch = session
        .find_entries_on_branch(
            &BranchQuery::new()
                .with_start("main-child")
                .with_order(EntryOrder::OldestFirst),
        )
        .await
        .unwrap();
    assert_eq!(ids(&main_branch), vec!["root", "main-child"]);
    let thread_branch = session
        .find_entries_on_branch(
            &BranchQuery::new()
                .with_start("thread-child")
                .with_order(EntryOrder::OldestFirst),
        )
        .await
        .unwrap();
    assert_eq!(ids(&thread_branch), vec!["root", "thread-child"]);
}

async fn rejects_invalid_queries(fixture: ConformanceFixture) {
    let session = fixture.create("invalid-queries").await;
    session.create_lane("thread", None).await.unwrap();
    let thread = session.view("thread");

    expect_code(
        session.find_entries(&EntryQuery::new().with_limit(0)).await,
        "invalid_query",
    );
    expect_code(
        session.find_entry(&EntryQuery::new().with_limit(0)).await,
        "invalid_query",
    );
    expect_code(
        session
            .find_entries_on_branch(&BranchQuery::new().with_limit(0))
            .await,
        "invalid_query",
    );
    expect_code(
        thread
            .find_entries_on_branch(&BranchQuery::new().with_cursor(-1))
            .await,
        "invalid_query",
    );
    expect_code(
        thread
            .find_entry_on_branch(&BranchQuery::new().with_limit(0))
            .await,
        "invalid_query",
    );
    expect_code(
        session
            .find_records(&RecordQuery::new().with_limit(0))
            .await,
        "invalid_query",
    );
    expect_code(
        session
            .find_records(&RecordQuery::new().with_operation_kind(OperationKind::Run))
            .await,
        "invalid_query",
    );
    expect_code(
        session
            .find_records(
                &RecordQuery::new()
                    .with_type(RecordType::StepAttempt)
                    .with_operation_kind(OperationKind::Run),
            )
            .await,
        "invalid_query",
    );
    expect_code(
        session.find_open_operations("main", Some(0)).await,
        "invalid_query",
    );
    expect_code(
        session.find_open_operations("main", Some(-1)).await,
        "invalid_query",
    );
    expect_code(
        session
            .get_log(&LogOptions {
                after_seq: Some(-1),
                limit: None,
            })
            .await,
        "invalid_query",
    );
}

async fn bounded_filtered_queries(fixture: ConformanceFixture) {
    let session = fixture.create("session").await;
    session
        .append_entry(&message_entry("root", user_message("root")), "main")
        .await
        .unwrap();
    session
        .append_entry(&custom_entry("old-note", "note", Some(json!(1))), "main")
        .await
        .unwrap();
    session
        .append_entry(
            &ProvisionedEntry::new(
                "compact",
                EntryPayload::Compaction(CompactionEntry {
                    summary: "summary".into(),
                    retained_tail: vec![],
                    tokens_before: 10,
                    details: None,
                    usage: None,
                }),
            ),
            "main",
        )
        .await
        .unwrap();
    session
        .append_entry(&custom_entry("new-note", "note", Some(json!(2))), "main")
        .await
        .unwrap();
    session
        .append_entry(&message_entry("tail", assistant_message("tail")), "main")
        .await
        .unwrap();

    assert_eq!(
        ids(&session.find_entries(&EntryQuery::new()).await.unwrap()),
        vec!["tail", "new-note", "compact", "old-note", "root"]
    );
    assert_eq!(
        ids(&session
            .find_entries(
                &EntryQuery::new()
                    .with_order(EntryOrder::OldestFirst)
                    .with_cursor(2)
                    .with_limit(2)
            )
            .await
            .unwrap()),
        vec!["compact", "new-note"]
    );
    assert_eq!(
        ids(&session
            .find_entries(&EntryQuery::new().with_custom_type("note"))
            .await
            .unwrap()),
        vec!["new-note", "old-note"]
    );
    assert_eq!(
        ids(&session
            .find_entries_on_branch(
                &BranchQuery::new()
                    .with_start("tail")
                    .with_custom_type("note")
                    .with_limit(1)
            )
            .await
            .unwrap()),
        vec!["new-note"]
    );
    assert_eq!(
        ids(&session
            .find_entries_on_branch(
                &BranchQuery::new()
                    .with_start("tail")
                    .with_stop_at_type(EntryType::Compaction)
                    .with_type(EntryType::Message)
            )
            .await
            .unwrap()),
        vec!["tail"]
    );
    assert!(session
        .find_entries_on_branch(
            &BranchQuery::new()
                .with_start("tail")
                .with_stop_at_id("tail")
                .with_type(EntryType::Custom)
        )
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        ids(&session
            .find_entries_on_branch(
                &BranchQuery::new()
                    .with_start("tail")
                    .with_stop_at_type(EntryType::Custom)
                    .with_order(EntryOrder::OldestFirst)
            )
            .await
            .unwrap()),
        vec!["root", "old-note"]
    );
    expect_code(
        session.find_entries(&EntryQuery::new().with_limit(0)).await,
        "invalid_query",
    );
    expect_code(
        session
            .find_entries_on_branch(&BranchQuery::new().with_start("missing"))
            .await,
        "not_found",
    );
}

async fn lane_names_are_permanent(fixture: ConformanceFixture) {
    let session = fixture.create("session").await;
    session.create_lane("thread", None).await.unwrap();
    session
        .append_record(&operation_started("old-run", "thread", OperationKind::Run))
        .await
        .unwrap();
    session
        .append_record(&NewRecord::new(
            "old-next-run",
            "thread",
            RecordPayload::QueueEnqueued(QueueEnqueuedRecord {
                queue: QueueKind::NextRun,
                run_id: None,
                target: message_entry("queued-message", user_message("queued")),
            }),
        ))
        .await
        .unwrap();

    assert_eq!(
        record_ids(
            &session
                .find_records(&RecordQuery::new().with_lane("thread"))
                .await
                .unwrap()
        ),
        vec!["old-next-run", "old-run"]
    );
    let logged: Vec<String> = session
        .get_log(&LogOptions::default())
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item {
            LogItem::Record { record, .. } => Some(record.id),
            _ => None,
        })
        .collect();
    assert_eq!(logged, vec!["old-run", "old-next-run"]);
    expect_code(session.create_lane("thread", None).await, "already_exists");
}

async fn queue_cancellation(fixture: ConformanceFixture) {
    let session = fixture.create("session").await;
    let enqueued = session
        .append_record(&NewRecord::new(
            "enqueue",
            "main",
            RecordPayload::QueueEnqueued(QueueEnqueuedRecord {
                queue: QueueKind::NextRun,
                run_id: None,
                target: message_entry("queued-message", user_message("queued")),
            }),
        ))
        .await
        .unwrap();
    let cancelled = session
        .append_record(&NewRecord::new(
            "cancel",
            "main",
            RecordPayload::QueueCancelled(QueueCancelledRecord {
                run_id: None,
                entry_id: "queued-message".into(),
            }),
        ))
        .await
        .unwrap();

    let cancellation = match &cancelled.payload {
        RecordPayload::QueueCancelled(record) => record,
        other => panic!("expected a queue_cancelled record, got {other:?}"),
    };
    assert_eq!(
        (cancelled.seq, cancellation.entry_id.as_str()),
        (2, "queued-message")
    );
    assert!(
        cancellation.run_id.is_none(),
        "cancellations carry no runId here"
    );
    assert!(session.get_entry("queued-message").await.unwrap().is_none());
    let cancellations = session
        .find_records(&RecordQuery::new().with_type(RecordType::QueueCancelled))
        .await
        .unwrap();
    assert_eq!(cancellations, vec![cancelled.clone()]);
    assert_eq!(
        session.get_log(&LogOptions::default()).await.unwrap(),
        vec![
            LogItem::Record {
                seq: enqueued.seq,
                record: enqueued
            },
            LogItem::Record {
                seq: cancelled.seq,
                record: cancelled
            },
        ]
    );
}

fn step_attempt(id: &str, lane: &str, run_id: &str, result_entry_id: &str) -> NewRecord {
    NewRecord::new(
        id,
        lane,
        RecordPayload::StepAttempt(StepAttemptRecord {
            run_id: run_id.into(),
            step: StepKind::Assistant,
            attempt: 1,
            result_entry_id: result_entry_id.into(),
            compaction_reason: None,
        }),
    )
}

async fn filters_records(fixture: ConformanceFixture) {
    let session = fixture.create("session").await;
    session
        .append_record(&operation_started("run-1", "main", OperationKind::Run))
        .await
        .unwrap();
    session
        .append_record(&step_attempt("attempt-1", "main", "run-1", "assistant-1"))
        .await
        .unwrap();
    session.create_lane("thread", None).await.unwrap();
    session
        .append_record(&operation_started("run-2", "thread", OperationKind::Run))
        .await
        .unwrap();
    session
        .append_record(&step_attempt("attempt-2", "thread", "run-2", "assistant-2"))
        .await
        .unwrap();

    assert_eq!(
        record_ids(
            &session
                .find_records(&RecordQuery::new().with_lane("thread"))
                .await
                .unwrap()
        ),
        vec!["attempt-2", "run-2"]
    );
    assert_eq!(
        record_ids(
            &session
                .find_records(
                    &RecordQuery::new()
                        .with_type(RecordType::StepAttempt)
                        .with_order(EntryOrder::OldestFirst)
                )
                .await
                .unwrap()
        ),
        vec!["attempt-1", "attempt-2"]
    );
    assert_eq!(
        record_ids(
            &session
                .find_records(&RecordQuery::new().with_run_id("run-1").with_after_seq(1))
                .await
                .unwrap()
        ),
        vec!["attempt-1"]
    );
    assert_eq!(
        record_ids(
            &session
                .find_records(&RecordQuery::new().with_limit(1))
                .await
                .unwrap()
        ),
        vec!["attempt-2"]
    );
}

async fn filters_by_operation_kind(fixture: ConformanceFixture) {
    let session = fixture.create("session").await;
    for (id, kind) in [
        ("run-old", OperationKind::Run),
        ("compaction", OperationKind::Compaction),
        ("navigation", OperationKind::Navigation),
    ] {
        session
            .append_record(&operation_started(id, "main", kind))
            .await
            .unwrap();
        session
            .append_record(&operation_finished(&format!("{id}-finished"), "main", id))
            .await
            .unwrap();
    }
    session
        .append_record(&operation_started("run-new", "main", OperationKind::Run))
        .await
        .unwrap();

    let by_kind = |kind, limit: Option<i64>, order: Option<EntryOrder>| {
        let mut query = RecordQuery::new()
            .with_type(RecordType::OperationStarted)
            .with_operation_kind(kind);
        query.limit = limit;
        query.order = order;
        query
    };
    assert_eq!(
        record_ids(
            &session
                .find_records(&by_kind(
                    OperationKind::Run,
                    None,
                    Some(EntryOrder::OldestFirst)
                ))
                .await
                .unwrap()
        ),
        vec!["run-old", "run-new"]
    );
    assert_eq!(
        record_ids(
            &session
                .find_records(&by_kind(OperationKind::Compaction, None, None))
                .await
                .unwrap()
        ),
        vec!["compaction"]
    );
    assert_eq!(
        record_ids(
            &session
                .find_records(&by_kind(OperationKind::Navigation, None, None))
                .await
                .unwrap()
        ),
        vec!["navigation"]
    );
    assert_eq!(
        record_ids(
            &session
                .find_records(&by_kind(OperationKind::Run, Some(1), None))
                .await
                .unwrap()
        ),
        vec!["run-new"]
    );
}

async fn one_open_operation_per_lane(fixture: ConformanceFixture) {
    let session = fixture.create("session").await;
    assert!(session
        .find_open_operations("main", Some(2))
        .await
        .unwrap()
        .is_empty());

    let first = session
        .append_record(&operation_started("first", "main", OperationKind::Run))
        .await
        .unwrap();
    assert_eq!(
        session.find_open_operations("main", Some(2)).await.unwrap(),
        vec![first.clone()]
    );
    expect_code(
        session
            .append_record(&operation_started("second", "main", OperationKind::Run))
            .await,
        "storage",
    );
    assert_eq!(
        session.find_open_operations("main", Some(2)).await.unwrap(),
        vec![first.clone()]
    );

    session
        .append_record(&operation_finished("finish-first", "main", &first.id))
        .await
        .unwrap();
    assert!(session
        .find_open_operations("main", Some(2))
        .await
        .unwrap()
        .is_empty());
}

async fn earlier_finish_does_not_close(fixture: ConformanceFixture) {
    let session = fixture.create("session").await;
    session
        .append_record(&operation_finished("finish-before-start", "main", "run"))
        .await
        .unwrap();
    let started = session
        .append_record(&operation_started("run", "main", OperationKind::Run))
        .await
        .unwrap();
    assert_eq!(
        session.find_open_operations("main", Some(2)).await.unwrap(),
        vec![started]
    );
}

async fn scopes_open_operations(fixture: ConformanceFixture) {
    let session = fixture.create("session").await;
    session.create_lane("thread", None).await.unwrap();
    let main_run = session
        .append_record(&operation_started("main-run", "main", OperationKind::Run))
        .await
        .unwrap();
    let thread_navigation = session
        .append_record(&operation_started(
            "thread-navigation",
            "thread",
            OperationKind::Navigation,
        ))
        .await
        .unwrap();

    assert_eq!(
        session.find_open_operations("main", None).await.unwrap(),
        vec![main_run.clone()]
    );
    assert_eq!(
        session.find_open_operations("main", Some(1)).await.unwrap(),
        vec![main_run]
    );
    assert_eq!(
        session
            .find_open_operations("thread", Some(2))
            .await
            .unwrap(),
        vec![thread_navigation]
    );
}

async fn immutable_open_operations(fixture: ConformanceFixture) {
    let session = fixture.create("session").await;
    let committed = session
        .append_record(&operation_started("run", "main", OperationKind::Run))
        .await
        .unwrap();
    let mut read = session.find_open_operations("main", None).await.unwrap();
    match &mut read[0].payload {
        RecordPayload::OperationStarted(started) => match &mut started.intent {
            OperationIntent::Run(run) => run.original_prompt.push(user_message("mutated")),
            other => panic!("expected a run intent, got {other:?}"),
        },
        other => panic!("expected an operation_started record, got {other:?}"),
    }

    assert_eq!(
        session.find_open_operations("main", None).await.unwrap(),
        vec![committed]
    );
}

async fn facts_and_statistics(fixture: ConformanceFixture) {
    let session = fixture.create("session").await;
    let mut assistant = match assistant_message("answer") {
        AgentMessage::Assistant(message) => message,
        other => panic!("expected an assistant message, got {other:?}"),
    };
    assistant.usage = Usage {
        input: 10,
        output: 5,
        cache_read: 3,
        cache_write: 2,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 20,
        cost: Cost {
            input: 1.0,
            output: 2.0,
            cache_read: 3.0,
            cache_write: 4.0,
            total: 10.0,
        },
    };
    let usage = assistant.usage.clone();
    session
        .append_entry(&message_entry("user", user_message("question")), "main")
        .await
        .unwrap();
    session
        .append_entry(
            &message_entry("assistant", AgentMessage::Assistant(assistant)),
            "main",
        )
        .await
        .unwrap();
    session
        .append_record(&NewRecord::new(
            "assistant-usage",
            "main",
            RecordPayload::Usage(UsageRecord {
                cause: UsageCause::Assistant,
                run_id: Some("run".into()),
                entry_id: Some("assistant".into()),
                tool_call_id: None,
                attempt: Some(1),
                stop_reason: Some(StopReason::Stop),
                details: None,
                usage: usage.clone(),
            }),
        ))
        .await
        .unwrap();
    session
        .append_record(&NewRecord::new(
            "deferred-usage",
            "main",
            RecordPayload::Usage(UsageRecord {
                cause: UsageCause::DeferredFetch,
                run_id: Some("run".into()),
                entry_id: Some("deferred-result".into()),
                tool_call_id: None,
                attempt: Some(1),
                stop_reason: Some(StopReason::Deferred),
                details: None,
                usage: Usage::default(),
            }),
        ))
        .await
        .unwrap();
    session
        .create_lane("thread", Some("assistant"))
        .await
        .unwrap();
    // Upstream's adjustment record also carries negative token counts; this
    // port cannot express those because `pi_core::Usage` counts are `u64`, so
    // only the (signed) cost correction is exercised here.
    session
        .append_record(&NewRecord::new(
            "correction",
            "thread",
            RecordPayload::Usage(UsageRecord {
                cause: UsageCause::Adjustment,
                run_id: None,
                entry_id: None,
                tool_call_id: None,
                attempt: None,
                stop_reason: None,
                details: Some(json!({ "reason": "provider correction" })),
                usage: usage_with_cost_total(-0.5),
            }),
        ))
        .await
        .unwrap();
    session.set_name(Some("First")).await.unwrap();
    session.set_name(Some("Second")).await.unwrap();
    session.set_label("user", Some("keep")).await.unwrap();
    session.set_label("user", None).await.unwrap();
    expect_code(
        session.set_label("missing", Some("checkpoint")).await,
        "not_found",
    );

    assert_eq!(session.get_name().await.unwrap().as_deref(), Some("Second"));
    assert_eq!(session.get_label("user").await.unwrap(), None);
    let usage_records = session
        .find_records(
            &RecordQuery::new()
                .with_type(RecordType::Usage)
                .with_order(EntryOrder::OldestFirst),
        )
        .await
        .unwrap();
    assert_eq!(
        usage_records
            .iter()
            .map(|record| record.as_usage().unwrap().cause)
            .collect::<Vec<_>>(),
        vec![
            UsageCause::Assistant,
            UsageCause::DeferredFetch,
            UsageCause::Adjustment
        ]
    );
    let deferred = usage_records
        .iter()
        .find(|record| record.as_usage().unwrap().cause == UsageCause::DeferredFetch)
        .expect("deferred usage record");
    assert_eq!(
        deferred.as_usage().unwrap().stop_reason,
        Some(StopReason::Deferred)
    );
    assert_eq!(
        session.get_stats().await.unwrap(),
        SessionStats {
            message_count: 2,
            cached_tokens: 3,
            uncached_tokens: 12,
            total_tokens: 20,
            cost_total: 9.5,
        }
    );
}

async fn clears_session_names(fixture: ConformanceFixture) {
    let session = fixture.create("session").await;
    session.set_name(Some("Temporary")).await.unwrap();
    session.set_name(None).await.unwrap();

    let expected_log = vec![
        LogItem::Name {
            seq: 1,
            name: Some("Temporary".into()),
        },
        LogItem::Name { seq: 2, name: None },
    ];
    assert_eq!(session.get_name().await.unwrap(), None);
    assert_eq!(
        session.get_log(&LogOptions::default()).await.unwrap(),
        expected_log
    );

    let metadata = session.get_metadata().await.unwrap();
    session.drain().await.unwrap();
    let reopened = fixture.repository.open(&metadata).await.unwrap();
    assert_eq!(reopened.get_name().await.unwrap(), None);
    assert_eq!(
        reopened.get_log(&LogOptions::default()).await.unwrap(),
        expected_log
    );

    let fork = fixture
        .repository
        .fork(&metadata, &ForkOptions::default(), &fixture.options("fork"))
        .await
        .unwrap();
    assert_eq!(fork.get_name().await.unwrap(), None);
}

async fn immutable_reads(fixture: ConformanceFixture) {
    let session = fixture.create("immutable").await;
    let metadata = session.get_metadata().await.unwrap();
    session
        .append_entry(
            &custom_entry("custom", "note", Some(json!({ "nested": { "value": 1 } }))),
            "main",
        )
        .await
        .unwrap();

    // Reads hand back owned copies; mutating one must not reach the store.
    let mut read = session
        .get_entry("custom")
        .await
        .unwrap()
        .expect("custom entry");
    let timestamp = read.timestamp;
    read.payload = EntryPayload::Custom(CustomEntry {
        custom_type: "note".into(),
        data: Some(json!({ "nested": { "value": 99 } })),
    });
    let mut log = session.get_log(&LogOptions::default()).await.unwrap();
    if let Some(LogItem::Entry { entry, .. }) = log.first_mut() {
        entry.id = "mutated".into();
    }

    assert_eq!(session.get_metadata().await.unwrap(), metadata);
    assert_eq!(
        session.get_entry("custom").await.unwrap().unwrap(),
        Entry {
            id: "custom".into(),
            seq: 1,
            parent_id: None,
            timestamp,
            payload: EntryPayload::Custom(CustomEntry {
                custom_type: "note".into(),
                data: Some(json!({ "nested": { "value": 1 } })),
            }),
            extra: serde_json::Map::new(),
        }
    );
}

async fn validates_lane_lifecycle(fixture: ConformanceFixture) {
    let session = fixture.create("session").await;
    expect_code(session.create_lane("main", None).await, "already_exists");
    expect_code(
        session.create_lane("thread", Some("missing")).await,
        "not_found",
    );
    expect_code(session.move_lane("missing", None).await, "invalid_lane");
}

async fn lane_views_do_not_cache_leaves(fixture: ConformanceFixture) {
    let session = fixture.create("session").await;
    let root = session.append_message(user_message("root")).await.unwrap();
    session.create_lane("thread", Some(&root)).await.unwrap();
    let thread = session.view("thread");
    let (main_child, thread_child) = tokio::join!(
        session.append_message(user_message("main")),
        thread.append_message(user_message("thread"))
    );
    let (main_child, thread_child) = (main_child.unwrap(), thread_child.unwrap());

    assert_eq!(
        session.get_leaf_id().await.unwrap(),
        Some(main_child.clone())
    );
    assert_eq!(
        thread.get_leaf_id().await.unwrap(),
        Some(thread_child.clone())
    );
    assert_eq!(
        ids(&session
            .find_entries_on_branch(&BranchQuery::new().with_order(EntryOrder::OldestFirst))
            .await
            .unwrap()),
        vec![root.clone(), main_child]
    );
    assert_eq!(
        ids(&thread
            .find_entries_on_branch(&BranchQuery::new().with_order(EntryOrder::OldestFirst))
            .await
            .unwrap()),
        vec![root, thread_child]
    );
    let empty = fixture.create("empty").await;
    assert!(empty
        .find_entries_on_branch(&BranchQuery::new())
        .await
        .unwrap()
        .is_empty());
}

async fn appends_provisioned_entries(fixture: ConformanceFixture) {
    let session = fixture.create("session").await;
    let entry = session
        .append_entry(
            &custom_entry("provisioned", "note", Some(json!({ "value": 1 }))),
            "main",
        )
        .await
        .unwrap();

    assert_eq!(entry.as_custom().unwrap().custom_type, "note");
    assert_eq!(
        (entry.id.as_str(), entry.parent_id.clone(), entry.seq),
        ("provisioned", None, 1)
    );
    assert_eq!(
        session.get_leaf_id().await.unwrap().as_deref(),
        Some("provisioned")
    );
}

async fn persists_termination(fixture: ConformanceFixture) {
    let session = fixture.create("session").await;
    let provisioned = ProvisionedEntry::new(
        "tool-result",
        EntryPayload::Message(MessageEntry {
            message: AgentMessage::ToolResult(ToolResultMessage {
                tool_call_id: "call-1".into(),
                tool_name: "example".into(),
                content: vec![InputContent::text("done")],
                details: None,
                usage: None,
                added_tool_names: None,
                is_error: false,
                timestamp: 1,
            }),
            terminate: Some(true),
        }),
    );
    let entry = session.append_entry(&provisioned, "main").await.unwrap();

    assert_eq!(entry.as_message().unwrap().terminate, Some(true));
    let stored = session
        .get_entry(&entry.id)
        .await
        .unwrap()
        .expect("message entry");
    assert_eq!(stored.as_message().unwrap().terminate, Some(true));
    assert_eq!(
        session.find_entries(&EntryQuery::new()).await.unwrap(),
        vec![entry.clone()]
    );
    assert_eq!(
        session.get_log(&LogOptions::default()).await.unwrap(),
        vec![LogItem::Entry {
            seq: entry.seq,
            entry
        }]
    );
}

/// A non-finite float is the one JSON-hostile value that survives Rust's type
/// system into a durable payload (upstream also tests `undefined`, `bigint`,
/// `Map` and cycles, none of which are representable here).
fn non_finite_compaction_entry(id: &str) -> ProvisionedEntry {
    ProvisionedEntry::new(
        id,
        EntryPayload::Compaction(CompactionEntry {
            summary: "summary".into(),
            retained_tail: vec![],
            tokens_before: 0,
            details: None,
            usage: Some(usage_with_cost_total(f64::NAN)),
        }),
    )
}

async fn rejects_non_json_entries(fixture: ConformanceFixture) {
    let session = fixture.create("session").await;
    expect_code(
        session
            .append_entry(&non_finite_compaction_entry("invalid"), "main")
            .await,
        "invalid_payload",
    );

    assert_eq!(session.get_leaf_id().await.unwrap(), None);
    assert!(session
        .find_entries(&EntryQuery::new())
        .await
        .unwrap()
        .is_empty());
    assert!(session
        .get_log(&LogOptions::default())
        .await
        .unwrap()
        .is_empty());
    let valid_id = session
        .append_custom_entry("valid", Some(json!({ "value": 1 })))
        .await
        .unwrap();
    assert_eq!(session.get_entry(&valid_id).await.unwrap().unwrap().seq, 1);
}

async fn rejects_non_json_records(fixture: ConformanceFixture) {
    let session = fixture.create("session").await;
    expect_code(
        session
            .append_record(&NewRecord::new(
                "nan-record",
                "main",
                RecordPayload::Usage(UsageRecord {
                    cause: UsageCause::Adjustment,
                    run_id: None,
                    entry_id: None,
                    tool_call_id: None,
                    attempt: None,
                    stop_reason: None,
                    details: None,
                    usage: usage_with_cost_total(f64::INFINITY),
                }),
            ))
            .await,
        "invalid_payload",
    );

    assert!(session
        .find_records(&RecordQuery::new())
        .await
        .unwrap()
        .is_empty());
    assert!(session
        .get_log(&LogOptions::default())
        .await
        .unwrap()
        .is_empty());
    // A well-formed record still lands on sequence 1: the rejection must not
    // have consumed a sequence number.
    let valid = session
        .append_record(&NewRecord::new(
            "valid-record",
            "main",
            RecordPayload::ToolStarted(ToolStartedRecord {
                run_id: "run".into(),
                assistant_entry_id: "assistant".into(),
                tool_index: 0,
                tool_call_id: "call".into(),
                tool_name: "example".into(),
                effective_args: serde_json::Map::new(),
                result_entry_id: "result".into(),
                replay: ToolReplay::Never,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(valid.seq, 1);
}

async fn linearizes_concurrent_writes(fixture: ConformanceFixture) {
    let session = fixture.create("session").await;
    session
        .append_entry(&message_entry("root", user_message("root")), "main")
        .await
        .unwrap();
    session.create_lane("thread", Some("root")).await.unwrap();
    let (main_1, thread_1, main_2, thread_2) = (
        custom_entry("main-1", "note", None),
        custom_entry("thread-1", "note", None),
        custom_entry("main-2", "note", None),
        custom_entry("thread-2", "note", None),
    );
    let results = tokio::join!(
        session.append_entry(&main_1, "main"),
        session.append_entry(&thread_1, "thread"),
        session.append_entry(&main_2, "main"),
        session.append_entry(&thread_2, "thread"),
    );
    let entries = vec![
        results.0.unwrap(),
        results.1.unwrap(),
        results.2.unwrap(),
        results.3.unwrap(),
    ];

    let mut sequences: Vec<i64> = entries.iter().map(|entry| entry.seq).collect();
    let unique: std::collections::HashSet<i64> = sequences.iter().copied().collect();
    assert_eq!(unique.len(), entries.len(), "sequences must be unique");
    sequences.sort_unstable();

    let concurrent: std::collections::HashSet<String> =
        entries.iter().map(|entry| entry.id.clone()).collect();
    let mut ordered = entries.clone();
    ordered.sort_by_key(|entry| entry.seq);
    let commit_order: Vec<String> = ordered.iter().map(|entry| entry.id.clone()).collect();
    let logged: Vec<String> = session
        .get_log(&LogOptions::default())
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item {
            LogItem::Entry { entry, .. } if concurrent.contains(&entry.id) => Some(entry.id),
            _ => None,
        })
        .collect();
    assert_eq!(logged, commit_order);

    let log_sequences: Vec<i64> = session
        .get_log(&LogOptions::default())
        .await
        .unwrap()
        .iter()
        .map(LogItem::seq)
        .collect();
    let mut sorted = log_sequences.clone();
    sorted.sort_unstable();
    assert_eq!(log_sequences, sorted, "the log must be in commit order");
}

async fn creates_lists_and_opens(fixture: ConformanceFixture) {
    let session = fixture.create("one").await;
    let entry_id = session
        .append_message(user_message("persisted"))
        .await
        .unwrap();
    session.drain().await.unwrap();
    let metadata = session.get_metadata().await.unwrap();

    let listed = fixture
        .repository
        .list(&SessionListOptions::default())
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, metadata.id);
    assert_eq!(listed[0].created_at, metadata.created_at);
    assert_eq!(listed[0].parent_session_id, metadata.parent_session_id);
    let reopened = fixture.repository.open(&metadata).await.unwrap();
    assert_eq!(
        ids(&reopened.find_entries(&EntryQuery::new()).await.unwrap()),
        vec![entry_id]
    );
    expect_code(
        fixture
            .repository
            .create(&fixture.options("one"))
            .await
            .map(|_| ()),
        "already_exists",
    );
}

async fn deletes_idempotently(fixture: ConformanceFixture) {
    let session = fixture.create("one").await;
    session.drain().await.unwrap();
    let metadata = session.get_metadata().await.unwrap();

    fixture.repository.delete(&metadata).await.unwrap();
    expect_code(
        fixture.repository.open(&metadata).await.map(|_| ()),
        "not_found",
    );
    fixture.repository.delete(&metadata).await.unwrap();
}

async fn forks_one_branch(fixture: ConformanceFixture) {
    let source = fixture.create("source").await;
    let root = source.append_message(user_message("root")).await.unwrap();
    let shared = source
        .append_message(assistant_message("shared"))
        .await
        .unwrap();
    source.create_lane("thread", Some(&shared)).await.unwrap();
    let thread_child = source
        .view("thread")
        .append_message(user_message("thread"))
        .await
        .unwrap();
    let main_child = source.append_message(user_message("main")).await.unwrap();
    source.set_name(Some("Source")).await.unwrap();
    source.set_label(&shared, Some("copied")).await.unwrap();
    source
        .set_label(&thread_child, Some("excluded"))
        .await
        .unwrap();
    source
        .append_record(&operation_started("run", "main", OperationKind::Run))
        .await
        .unwrap();
    source
        .append_record(&NewRecord::new(
            "source-usage",
            "main",
            RecordPayload::Usage(UsageRecord {
                cause: UsageCause::Adjustment,
                run_id: None,
                entry_id: None,
                tool_call_id: None,
                attempt: None,
                stop_reason: None,
                details: None,
                usage: Usage {
                    input: 10,
                    output: 5,
                    cache_read: 3,
                    cache_write: 2,
                    cache_write_1h: None,
                    reasoning: None,
                    total_tokens: 20,
                    cost: Cost {
                        input: 1.0,
                        output: 2.0,
                        cache_read: 3.0,
                        cache_write: 4.0,
                        total: 10.0,
                    },
                },
            }),
        ))
        .await
        .unwrap();
    source.drain().await.unwrap();

    let fork = fixture
        .repository
        .fork(
            &source.get_metadata().await.unwrap(),
            &ForkOptions {
                scope: Some(ForkScope::Branch),
                entry_id: Some(main_child.clone()),
                position: Some(ForkPosition::At),
            },
            &fixture.options("branch-fork"),
        )
        .await
        .unwrap();

    assert_eq!(
        ids(&fork
            .find_entries(&EntryQuery::new().with_order(EntryOrder::OldestFirst))
            .await
            .unwrap()),
        vec![root, shared.clone(), main_child]
    );
    assert_eq!(fork.get_lanes().await.unwrap().len(), 1);
    assert_eq!(fork.get_name().await.unwrap().as_deref(), Some("Source"));
    assert_eq!(
        fork.get_label(&shared).await.unwrap().as_deref(),
        Some("copied")
    );
    assert_eq!(fork.get_label(&thread_child).await.unwrap(), None);
    assert!(fork
        .find_records(&RecordQuery::new())
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        fork.get_stats().await.unwrap(),
        SessionStats {
            message_count: 3,
            ..Default::default()
        }
    );
    fork.append_message(user_message("after fork"))
        .await
        .unwrap();
    assert_eq!(fork.get_stats().await.unwrap().message_count, 4);
    let metadata = fork.get_metadata().await.unwrap();
    assert_eq!(
        (metadata.id.as_str(), metadata.parent_session_id.as_deref()),
        ("branch-fork", Some("source"))
    );
}

async fn forks_a_tree(fixture: ConformanceFixture) {
    let source = fixture.create("source").await;
    let root = source.append_message(user_message("root")).await.unwrap();
    source.create_lane("thread", Some(&root)).await.unwrap();
    let main_child = source.append_message(user_message("main")).await.unwrap();
    let thread_child = source
        .view("thread")
        .append_message(user_message("thread"))
        .await
        .unwrap();
    source
        .set_label(&thread_child, Some("thread-tip"))
        .await
        .unwrap();
    source.drain().await.unwrap();

    let fork = fixture
        .repository
        .fork(
            &source.get_metadata().await.unwrap(),
            &ForkOptions::tree(),
            &fixture.options("tree-fork"),
        )
        .await
        .unwrap();
    assert_eq!(
        ids(&fork
            .find_entries(&EntryQuery::new().with_order(EntryOrder::OldestFirst))
            .await
            .unwrap()),
        vec![root, main_child.clone(), thread_child.clone()]
    );
    assert_eq!(
        fork.get_lanes().await.unwrap(),
        vec![
            lane("main", Some(&main_child)),
            lane("thread", Some(&thread_child))
        ]
    );
    assert_eq!(
        fork.get_label(&thread_child).await.unwrap().as_deref(),
        Some("thread-tip")
    );
    assert_eq!(fork.get_stats().await.unwrap().message_count, 3);
    let lane_items: Vec<LogItem> = fork
        .get_log(&LogOptions::default())
        .await
        .unwrap()
        .into_iter()
        .filter(|item| matches!(item, LogItem::Lane { .. }))
        .collect();
    assert_eq!(
        lane_items,
        vec![
            LogItem::Lane {
                seq: 4,
                lane: "main".into(),
                leaf_id: Some(main_child)
            },
            LogItem::Lane {
                seq: 5,
                lane: "thread".into(),
                leaf_id: Some(thread_child)
            },
        ]
    );
}

async fn forks_before_an_entry(fixture: ConformanceFixture) {
    let source = fixture.create("source").await;
    let root = source.append_message(user_message("root")).await.unwrap();
    let tail = source.append_message(user_message("tail")).await.unwrap();
    source.drain().await.unwrap();
    let metadata = source.get_metadata().await.unwrap();

    let fork = fixture
        .repository
        .fork(
            &metadata,
            &ForkOptions {
                entry_id: Some(tail.clone()),
                ..Default::default()
            },
            &fixture.options("fork"),
        )
        .await
        .unwrap();
    assert_eq!(
        ids(&fork
            .find_entries(&EntryQuery::new().with_order(EntryOrder::OldestFirst))
            .await
            .unwrap()),
        vec![root.clone()]
    );
    assert_eq!(
        fork.get_leaf_id().await.unwrap().as_deref(),
        Some(root.as_str())
    );
    assert_eq!(
        source.get_leaf_id().await.unwrap().as_deref(),
        Some(tail.as_str())
    );

    let before_default = fixture
        .repository
        .fork(
            &metadata,
            &ForkOptions {
                position: Some(ForkPosition::Before),
                ..Default::default()
            },
            &fixture.options("before-default-target"),
        )
        .await
        .unwrap();
    assert_eq!(
        ids(&before_default
            .find_entries(&EntryQuery::new().with_order(EntryOrder::OldestFirst))
            .await
            .unwrap()),
        vec![root.clone()]
    );
    assert_eq!(
        before_default.get_leaf_id().await.unwrap().as_deref(),
        Some(root.as_str())
    );

    let at_default = fixture
        .repository
        .fork(
            &metadata,
            &ForkOptions {
                position: Some(ForkPosition::At),
                ..Default::default()
            },
            &fixture.options("at-default-target"),
        )
        .await
        .unwrap();
    assert_eq!(
        ids(&at_default
            .find_entries(&EntryQuery::new().with_order(EntryOrder::OldestFirst))
            .await
            .unwrap()),
        vec![root, tail.clone()]
    );
    assert_eq!(
        at_default.get_leaf_id().await.unwrap().as_deref(),
        Some(tail.as_str())
    );
    expect_code(
        fixture
            .repository
            .fork(
                &metadata,
                &ForkOptions {
                    entry_id: Some("missing".into()),
                    ..Default::default()
                },
                &fixture.options("missing-fork"),
            )
            .await
            .map(|_| ()),
        "invalid_fork_target",
    );
}

async fn validates_default_fork_target(fixture: ConformanceFixture) {
    let source = fixture.create("source-with-custom-leaf").await;
    source
        .append_custom_entry("not-a-message", None)
        .await
        .unwrap();
    source.drain().await.unwrap();

    expect_code(
        fixture
            .repository
            .fork(
                &source.get_metadata().await.unwrap(),
                &ForkOptions::default(),
                &fixture.options("fork"),
            )
            .await
            .map(|_| ()),
        "invalid_fork_target",
    );
}

/// Never constructed; keeps the unused-import lint honest about re-exports the
/// suite deliberately exposes for backend authors.
#[allow(dead_code)]
fn _type_anchors(_: Option<SessionError>) {}
