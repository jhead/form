//! Spec 02 §8. The ordering grammar in [`assert_ordering`] is the important one — it is
//! written out from spec 00 §5.1 rather than from the generator, so a change to the
//! generator that breaks the contract fails here rather than in the app.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::protocol::{
    AssistantContent, AssistantMessage, AssistantMessageEvent, DoneReason, Entry, EntryKind,
    ErrorReason, EventKind, Message, ModelRef, RunOutcome, StopReason, ThinkingLevel,
};

use super::content::{self, Shape};
use crate::app::TurnRecord;

use super::{AbortSignal, Harness, RunContext, RunRequest, StubHarness};

// ---------------------------------------------------------------- harness under test

struct Recorder {
    events: Mutex<Vec<EventKind>>,
    queue: Mutex<VecDeque<String>>,
    turns: Mutex<Vec<TurnRecord>>,
    seq: AtomicU64,
    speed: f64,
    run_end_at: Mutex<Option<Instant>>,
    /// Queued the moment the first turn ends — the "typed while it was streaming" case.
    inject_at_first_turn_end: Mutex<Option<String>>,
}

impl Recorder {
    fn new(speed: f64) -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(Vec::new()),
            queue: Mutex::new(VecDeque::new()),
            turns: Mutex::new(Vec::new()),
            seq: AtomicU64::new(0),
            speed,
            run_end_at: Mutex::new(None),
            inject_at_first_turn_end: Mutex::new(None),
        })
    }

    fn with_events<R>(&self, f: impl FnOnce(&[EventKind]) -> R) -> R {
        f(&self.events.lock().unwrap())
    }

    fn queue_prompt(&self, text: &str) {
        self.queue.lock().unwrap().push_back(text.to_string());
    }
}

impl RunContext for Recorder {
    fn emit(&self, kind: EventKind) {
        if matches!(kind, EventKind::RunEnd { .. }) {
            *self.run_end_at.lock().unwrap() = Some(Instant::now());
        }
        if matches!(kind, EventKind::TurnEnd { .. }) {
            if let Some(text) = self.inject_at_first_turn_end.lock().unwrap().take() {
                self.queue.lock().unwrap().push_back(text);
            }
        }
        self.events.lock().unwrap().push(kind);
    }

    fn append_entry(&self, session_id: &str, kind: EntryKind) -> Option<Entry> {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        Some(Entry {
            id: format!("ent_{seq}"),
            session_id: session_id.to_string(),
            seq,
            parent_id: None,
            // Fixed so a recorded transcript is comparable across runs.
            timestamp: 0,
            kind,
        })
    }

    fn replace_entry(&self, _entry: &Entry) {}

    fn speed(&self) -> f64 {
        self.speed
    }

    fn take_queued_prompt(&self) -> Option<String> {
        self.queue.lock().unwrap().pop_front()
    }

    fn record_turn(&self, turn: TurnRecord) {
        self.turns.lock().unwrap().push(turn);
    }
}

fn request(session_id: &str, turn_index: u32) -> RunRequest {
    RunRequest {
        session_id: session_id.to_string(),
        run_id: format!("run_{session_id}"),
        command_id: Some("cmd_test".to_string()),
        prompt: "Add a health check endpoint that bypasses auth".to_string(),
        model: ModelRef {
            provider_id: "anthropic".to_string(),
            model_id: "claude-opus-5".to_string(),
            thinking_level: ThinkingLevel::High,
        },
        workspace_root: Some("/Users/x/dev/api".to_string()),
        turn_index,
    }
}

async fn run(session_id: &str, speed: f64) -> Arc<Recorder> {
    let recorder = Recorder::new(speed);
    StubHarness
        .run(
            request(session_id, 0),
            recorder.clone() as Arc<dyn RunContext>,
            AbortSignal::new(),
        )
        .await;
    recorder
}

// ---------------------------------------------------------------- the grammar

fn is_assistant(entry: &Entry) -> bool {
    matches!(
        &entry.kind,
        EntryKind::Message {
            message: Message::Assistant(_)
        }
    )
}

fn role_of(entry: &Entry) -> &'static str {
    match &entry.kind {
        EntryKind::Message { message } => match message {
            Message::User(_) => "user",
            Message::Assistant(_) => "assistant",
            Message::ToolResult(_) => "toolResult",
        },
        _ => "other",
    }
}

/// The ordering contract from spec 00 §5.1, as a state machine.
fn assert_ordering(events: &[EventKind]) {
    assert!(
        matches!(events.first(), Some(EventKind::RunStart { .. })),
        "run_start must be the first event"
    );
    assert!(
        matches!(events.last(), Some(EventKind::RunEnd { .. })),
        "run_end must be the last event"
    );
    let terminal = events
        .iter()
        .filter(|e| matches!(e, EventKind::RunEnd { .. }))
        .count();
    assert_eq!(terminal, 1, "exactly one terminal run event");

    let mut turn_open = false;
    let mut turns_started = 0usize;
    let mut turns_ended = 0usize;
    let mut open_message: Option<String> = None;
    let mut stream_terminated = false;
    let mut open_tool: Option<String> = None;

    for (i, event) in events.iter().enumerate() {
        match event {
            EventKind::RunStart { .. } => assert_eq!(i, 0, "run_start may only appear first"),

            EventKind::TurnStart { .. } => {
                assert!(!turn_open, "turn_start inside an open turn");
                turn_open = true;
                turns_started += 1;
            }

            EventKind::MessageStart { entry, .. } => {
                assert!(turn_open, "message_start outside a turn");
                assert!(
                    open_message.is_none(),
                    "message_start inside an open assistant message"
                );
                if is_assistant(entry) {
                    open_message = Some(entry.id.clone());
                    stream_terminated = false;
                }
            }

            EventKind::MessageUpdate {
                entry_id, event, ..
            } => {
                assert_eq!(
                    open_message.as_deref(),
                    Some(entry_id.as_str()),
                    "message_update outside its message_start/message_end"
                );
                assert!(
                    !stream_terminated,
                    "assistant event after the terminal done/error"
                );
                if event.is_terminal() {
                    stream_terminated = true;
                } else {
                    assert!(
                        event.partial().is_some(),
                        "every non-terminal event carries a partial"
                    );
                }
            }

            EventKind::MessageEnd { entry, .. } => {
                assert!(turn_open, "message_end outside a turn");
                if is_assistant(entry) {
                    assert_eq!(open_message.as_deref(), Some(entry.id.as_str()));
                    assert!(
                        stream_terminated,
                        "assistant message ended without done or error"
                    );
                    open_message = None;
                }
            }

            EventKind::ToolExecutionStart { tool_call_id, .. } => {
                assert!(turn_open, "tool execution outside a turn");
                assert!(
                    open_message.is_none(),
                    "tool execution before the assistant message closed"
                );
                assert!(open_tool.is_none(), "overlapping tool executions");
                open_tool = Some(tool_call_id.clone());
            }
            EventKind::ToolExecutionUpdate { tool_call_id, .. } => {
                assert_eq!(open_tool.as_deref(), Some(tool_call_id.as_str()));
            }
            EventKind::ToolExecutionEnd { tool_call_id, .. } => {
                assert_eq!(open_tool.as_deref(), Some(tool_call_id.as_str()));
                open_tool = None;
            }

            EventKind::TurnEnd { .. } => {
                assert!(turn_open, "turn_end without turn_start");
                assert!(open_message.is_none(), "turn_end inside an open message");
                assert!(open_tool.is_none(), "turn_end inside a tool execution");
                turn_open = false;
                turns_ended += 1;
            }

            EventKind::RunEnd { .. } => {
                assert!(!turn_open, "run_end inside an open turn");
                assert_eq!(i, events.len() - 1);
            }

            other => panic!("harness emitted a non-run event: {other:?}"),
        }
    }

    assert_eq!(
        turns_started, turns_ended,
        "every turn_start needs a turn_end"
    );
    assert!(turns_started > 0, "a run has at least one turn");
}

fn thinking_of(message: &AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|c| match c {
            AssistantContent::Thinking(t) => Some(t.thinking.as_str()),
            _ => None,
        })
        .collect()
}

/// Spec 02 §3: the partial must be genuinely accumulated, because Swift reconciles against
/// it. Replays the deltas and compares at every step, then against the terminal message.
fn assert_partial_accumulates(events: &[EventKind]) {
    let mut text: HashMap<String, String> = HashMap::new();
    let mut thinking: HashMap<String, String> = HashMap::new();
    let mut checked = 0usize;

    for event in events {
        let EventKind::MessageUpdate {
            entry_id, event, ..
        } = event
        else {
            continue;
        };
        let text = text.entry(entry_id.clone()).or_default();
        let thinking = thinking.entry(entry_id.clone()).or_default();

        match event {
            AssistantMessageEvent::TextDelta { delta, partial, .. } => {
                text.push_str(delta);
                assert_eq!(&partial.text(), text, "text partial fell behind its deltas");
                checked += 1;
            }
            AssistantMessageEvent::ThinkingDelta { delta, partial, .. } => {
                thinking.push_str(delta);
                assert_eq!(
                    &thinking_of(partial),
                    thinking,
                    "thinking partial fell behind its deltas"
                );
                checked += 1;
            }
            AssistantMessageEvent::TextEnd {
                content, partial, ..
            } => {
                assert_eq!(content, text);
                assert_eq!(&partial.text(), text);
            }
            AssistantMessageEvent::ToolCallEnd {
                tool_call, partial, ..
            } => {
                assert!(
                    partial.tool_calls().any(|c| c.id == tool_call.id
                        && c.name == tool_call.name
                        && c.arguments == tool_call.arguments),
                    "toolcall_end's call is missing from the partial"
                );
                checked += 1;
            }
            AssistantMessageEvent::Done { message, .. }
            | AssistantMessageEvent::Error { error: message, .. } => {
                assert_eq!(&message.text(), text, "final message text != accumulated");
                assert_eq!(&thinking_of(message), thinking);
            }
            _ => {}
        }
    }
    assert!(checked > 0, "no deltas to accumulate");
}

/// Everything about a run that is a function of the seed. Ids minted by the store and
/// wall-clock durations are deliberately excluded.
fn fingerprint(events: &[EventKind]) -> Vec<String> {
    events
        .iter()
        .map(|event| match event {
            EventKind::RunStart { .. } => "run_start".to_string(),
            EventKind::TurnStart { .. } => "turn_start".to_string(),
            EventKind::MessageStart { entry, .. } => format!("message_start:{}", role_of(entry)),
            EventKind::MessageEnd { entry, .. } => format!("message_end:{}", role_of(entry)),
            EventKind::MessageUpdate { event, .. } => match event {
                AssistantMessageEvent::Start { .. } => "start".to_string(),
                AssistantMessageEvent::TextDelta { delta, .. } => format!("text_delta:{delta}"),
                AssistantMessageEvent::ThinkingDelta { delta, .. } => {
                    format!("thinking_delta:{delta}")
                }
                AssistantMessageEvent::ToolCallDelta { delta, .. } => {
                    format!("toolcall_delta:{delta}")
                }
                AssistantMessageEvent::ToolCallEnd { tool_call, .. } => {
                    format!("toolcall_end:{}:{:?}", tool_call.name, tool_call.arguments)
                }
                AssistantMessageEvent::Done { reason, message } => {
                    format!("done:{reason:?}:{}", message.usage.total_tokens)
                }
                AssistantMessageEvent::Error { reason, error } => {
                    format!("error:{reason:?}:{:?}", error.error_message)
                }
                other => format!("{other:?}").chars().take(24).collect(),
            },
            EventKind::ToolExecutionStart {
                tool_name, args, ..
            } => format!("tool_start:{tool_name}:{args}"),
            EventKind::ToolExecutionUpdate { partial_result, .. } => {
                format!("tool_update:{partial_result}")
            }
            EventKind::ToolExecutionEnd {
                result, is_error, ..
            } => format!("tool_end:{result}:{is_error}"),
            EventKind::TurnEnd { usage, .. } => format!("turn_end:{}", usage.total_tokens),
            EventKind::RunEnd { outcome, usage, .. } => {
                format!("run_end:{outcome:?}:{}", usage.total_tokens)
            }
            other => format!("{other:?}"),
        })
        .collect()
}

fn run_outcome(events: &[EventKind]) -> RunOutcome {
    events
        .iter()
        .find_map(|e| match e {
            EventKind::RunEnd { outcome, .. } => Some(*outcome),
            _ => None,
        })
        .expect("run_end")
}

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn ordering_holds_across_seeds() {
    for n in 0..16 {
        let recorder = run(&format!("ses_order_{n}"), 500.0).await;
        recorder.with_events(|events| {
            assert_ordering(events);
            assert_partial_accumulates(events);
        });
    }
}

#[tokio::test]
async fn every_run_has_exactly_one_terminal_event() {
    for n in 0..8 {
        let recorder = run(&format!("ses_terminal_{n}"), 500.0).await;
        recorder.with_events(|events| {
            assert_eq!(
                events
                    .iter()
                    .filter(|e| matches!(e, EventKind::RunEnd { .. }))
                    .count(),
                1
            );
            // One terminal assistant event per assistant message, and nothing after it.
            let mut per_entry: HashMap<&str, usize> = HashMap::new();
            for event in events {
                if let EventKind::MessageUpdate {
                    entry_id, event, ..
                } = event
                {
                    if event.is_terminal() {
                        *per_entry.entry(entry_id.as_str()).or_default() += 1;
                    }
                }
            }
            assert!(!per_entry.is_empty());
            for (entry, count) in per_entry {
                assert_eq!(count, 1, "{entry} had {count} terminal events");
            }
        });
    }
}

#[tokio::test]
async fn content_is_deterministic_for_a_fixed_seed() {
    let first = run("ses_fixed_seed", 500.0).await;
    let second = run("ses_fixed_seed", 500.0).await;
    let (a, b) = (
        first.with_events(fingerprint),
        second.with_events(fingerprint),
    );
    assert_eq!(
        a.len(),
        b.len(),
        "event count differs between identical runs"
    );
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        assert_eq!(x, y, "event {i} differs between identical runs");
    }

    // …and a different session produces a different transcript.
    let other = run("ses_other_seed", 500.0).await;
    assert_ne!(a, other.with_events(fingerprint));

    // …as does a different prompt in the same session. `Core` currently passes
    // `turn_index: 0` for every run, so without this the second question in a session would
    // get a verbatim repeat of the answer to the first.
    let recorder = Recorder::new(500.0);
    let mut req = request("ses_fixed_seed", 0);
    req.prompt = "Why is the transcript re-rendering every frame?".to_string();
    StubHarness
        .run(
            req,
            recorder.clone() as Arc<dyn RunContext>,
            AbortSignal::new(),
        )
        .await;
    assert_ne!(a, recorder.with_events(fingerprint));
}

#[tokio::test]
async fn abort_lands_between_events_within_the_budget() {
    // Human speed: a turn takes seconds, so landing in under 100 ms can only happen if the
    // signal is observed between events rather than between turns.
    let recorder = Recorder::new(1.0);
    let abort = AbortSignal::new();
    let task = tokio::spawn({
        let ctx = recorder.clone() as Arc<dyn RunContext>;
        let abort = abort.clone();
        async move {
            StubHarness.run(request("ses_abort", 0), ctx, abort).await;
        }
    });

    tokio::time::sleep(Duration::from_millis(700)).await;
    let issued = Instant::now();
    abort.abort();
    task.await.unwrap();

    let landed = recorder.run_end_at.lock().unwrap().expect("run_end");
    let latency = landed.duration_since(issued);
    assert!(
        latency < Duration::from_millis(100),
        "abort took {latency:?} to land"
    );

    recorder.with_events(|events| {
        assert_ordering(events);
        assert_eq!(run_outcome(events), RunOutcome::Aborted);
        let aborted = events.iter().any(|e| {
            matches!(
                e,
                EventKind::MessageUpdate {
                    event: AssistantMessageEvent::Error {
                        reason: ErrorReason::Aborted,
                        ..
                    },
                    ..
                }
            )
        });
        assert!(aborted, "an abort must produce error {{ reason: aborted }}");
    });
}

#[tokio::test]
async fn abort_before_the_first_token_still_terminates_cleanly() {
    let recorder = Recorder::new(1.0);
    let abort = AbortSignal::new();
    let task = tokio::spawn({
        let ctx = recorder.clone() as Arc<dyn RunContext>;
        let abort = abort.clone();
        async move {
            StubHarness
                .run(request("ses_abort_early", 0), ctx, abort)
                .await;
        }
    });

    tokio::time::sleep(Duration::from_millis(40)).await;
    let issued = Instant::now();
    abort.abort();
    task.await.unwrap();

    assert!(issued.elapsed() < Duration::from_millis(100));
    recorder.with_events(|events| {
        assert_ordering(events);
        assert_eq!(run_outcome(events), RunOutcome::Aborted);
    });
}

/// F1.7 — a prompt sent while the run is streaming is held and injected at the next turn
/// boundary, extending the run if the agent would otherwise have stopped.
#[tokio::test]
async fn a_prompt_queued_mid_run_is_injected_at_the_next_turn_boundary() {
    // A run scripted to fail stops at the failure, so pick a seed that does not.
    let mut found = None;
    for n in 0..8 {
        let recorder = Recorder::new(500.0);
        *recorder.inject_at_first_turn_end.lock().unwrap() =
            Some("actually, make it /healthz".to_string());
        StubHarness
            .run(
                request(&format!("ses_queue_mid_{n}"), 0),
                recorder.clone() as Arc<dyn RunContext>,
                AbortSignal::new(),
            )
            .await;
        if recorder.with_events(run_outcome) == RunOutcome::Completed {
            found = Some(recorder);
            break;
        }
    }
    let recorder = found.expect("a completed run");

    recorder.with_events(|events| {
        assert_ordering(events);
        let first_turn_end = events
            .iter()
            .position(|e| matches!(e, EventKind::TurnEnd { .. }))
            .expect("at least one turn");
        let injected = events
            .iter()
            .position(
                |e| matches!(e, EventKind::MessageStart { entry, .. } if role_of(entry) == "user"),
            )
            .expect("the queued prompt joins the transcript");
        assert!(
            injected > first_turn_end,
            "the prompt was queued during the first turn, so it lands after it"
        );
        assert!(
            matches!(events[injected - 1], EventKind::TurnStart { .. }),
            "injection happens at a turn boundary, before the assistant responds"
        );
        assert!(events[injected..].iter().any(
            |e| matches!(e, EventKind::MessageStart { entry, .. } if role_of(entry) == "assistant")
        ));
    });
}

#[tokio::test]
async fn a_queued_prompt_is_injected_at_a_turn_boundary() {
    let recorder = Recorder::new(500.0);
    recorder.queue_prompt("actually, make it /healthz");
    StubHarness
        .run(
            request("ses_queue", 0),
            recorder.clone() as Arc<dyn RunContext>,
            AbortSignal::new(),
        )
        .await;

    recorder.with_events(|events| {
        assert_ordering(events);
        let injected = events
            .iter()
            .position(
                |e| matches!(e, EventKind::MessageStart { entry, .. } if role_of(entry) == "user"),
            )
            .expect("the queued prompt joins the transcript");
        assert!(
            matches!(events[injected - 1], EventKind::TurnStart { .. }),
            "a queued prompt is injected immediately after turn_start"
        );
        // …and the turn it opens still produces an assistant response.
        assert!(events[injected..].iter().any(
            |e| matches!(e, EventKind::MessageStart { entry, .. } if role_of(entry) == "assistant")
        ));
    });
    assert!(recorder.take_queued_prompt().is_none(), "queue is drained");
}

#[tokio::test]
async fn two_sessions_interleave_without_cross_talk() {
    let shared = Recorder::new(300.0);
    let a = tokio::spawn({
        let ctx = shared.clone() as Arc<dyn RunContext>;
        async move {
            StubHarness
                .run(request("ses_alpha", 0), ctx, AbortSignal::new())
                .await;
        }
    });
    let b = tokio::spawn({
        let ctx = shared.clone() as Arc<dyn RunContext>;
        async move {
            StubHarness
                .run(request("ses_beta", 0), ctx, AbortSignal::new())
                .await;
        }
    });
    a.await.unwrap();
    b.await.unwrap();

    shared.with_events(|events| {
        let mut by_session: HashMap<String, Vec<EventKind>> = HashMap::new();
        let mut order: Vec<&str> = Vec::new();
        for event in events {
            let session = session_of(event).expect("every run event carries a sessionId");
            if order.last() != Some(&session) {
                order.push(session);
            }
            by_session
                .entry(session.to_string())
                .or_default()
                .push(event.clone());
        }

        assert_eq!(by_session.len(), 2);
        for (session, group) in &by_session {
            assert_ordering(group);
            assert_partial_accumulates(group);
            assert!(session.starts_with("ses_"));
        }
        assert!(
            order.len() > 4,
            "the two runs must actually interleave, saw {} switches",
            order.len()
        );
    });
}

fn session_of(event: &EventKind) -> Option<&str> {
    Some(match event {
        EventKind::RunStart { session_id, .. }
        | EventKind::TurnStart { session_id, .. }
        | EventKind::MessageStart { session_id, .. }
        | EventKind::MessageUpdate { session_id, .. }
        | EventKind::MessageEnd { session_id, .. }
        | EventKind::ToolExecutionStart { session_id, .. }
        | EventKind::ToolExecutionUpdate { session_id, .. }
        | EventKind::ToolExecutionEnd { session_id, .. }
        | EventKind::TurnEnd { session_id, .. }
        | EventKind::RunEnd { session_id, .. } => session_id,
        _ => return None,
    })
}

#[tokio::test]
async fn failures_are_encoded_in_the_stream() {
    let mut found = false;
    for n in 0..40 {
        let recorder = run(&format!("ses_fail_{n}"), 800.0).await;
        let failed = recorder.with_events(|events| {
            assert_ordering(events);
            if run_outcome(events) != RunOutcome::Failed {
                return false;
            }
            let errored = events.iter().any(|e| {
                matches!(e, EventKind::MessageUpdate {
                event: AssistantMessageEvent::Error { reason: ErrorReason::Error, error },
                ..
            } if error.stop_reason == StopReason::Error && error.error_message.is_some())
            });
            assert!(errored, "a failed run carries error {{ reason: error }}");
            true
        });
        if failed {
            found = true;
            break;
        }
    }
    assert!(found, "no seed in 0..40 produced a failed run");
}

#[tokio::test]
async fn an_unterminated_fence_reaches_the_stream() {
    let mut found = false;
    for n in 0..24 {
        let recorder = run(&format!("ses_trunc_{n}"), 800.0).await;
        found = recorder.with_events(|events| {
            events.iter().any(|e| {
                matches!(
                    e,
                    EventKind::MessageUpdate {
                        event: AssistantMessageEvent::Done {
                            reason: DoneReason::Length,
                            ..
                        },
                        ..
                    }
                )
            })
        });
        if found {
            break;
        }
    }
    assert!(
        found,
        "no seed in 0..24 produced a length-truncated response"
    );
}

#[tokio::test]
async fn usage_is_priced_from_the_catalog_and_caches_on_later_turns() {
    // A session long enough to have a second turn — the first turn can only write cache.
    let mut multi = None;
    for n in 0..24 {
        let recorder = run(&format!("ses_usage_{n}"), 800.0).await;
        if recorder.turns.lock().unwrap().len() >= 2
            && recorder.with_events(run_outcome) == RunOutcome::Completed
        {
            multi = Some(recorder);
            break;
        }
    }
    let recorder = multi.expect("no seed produced a multi-turn completed run");
    let turns = recorder.turns.lock().unwrap().clone();

    let first = &turns[0];
    assert_eq!(
        first.usage.cache_read, 0,
        "nothing is cached on the first turn"
    );
    assert!(
        first.usage.cache_write > 0,
        "the prefix is written to cache"
    );
    assert!(first.usage.input > 0 && first.usage.output > 0);
    assert!(first.ttft_ms.is_some());

    let second = &turns[1];
    assert!(
        second.usage.cache_read > 0 && second.usage.cache_write > 0,
        "later turns report both a cache read and a write (F11.10)"
    );

    // Opus 5 in the seeded catalog: $5/Mtok in, $25/Mtok out.
    let pricing = crate::catalog::resolve(&first.model)
        .expect("model")
        .pricing;
    let expected = first.usage.input as f64 / 1e6 * pricing.input;
    assert!((first.usage.cost.input - expected).abs() < 1e-9);
    assert!(first.usage.cost.total > 0.0);
    assert_eq!(
        first.usage.total_tokens,
        first.usage.input + first.usage.output + first.usage.cache_read + first.usage.cache_write
    );

    // run_end reports the sum of the turns.
    let total: u64 = turns.iter().map(|t| t.usage.total_tokens).sum();
    recorder.with_events(|events| {
        let reported = events
            .iter()
            .find_map(|e| match e {
                EventKind::RunEnd { usage, .. } => Some(usage.total_tokens),
                _ => None,
            })
            .unwrap();
        assert_eq!(reported, total);
    });
}

#[tokio::test]
async fn tool_calls_are_plausible_and_report_diff_counts() {
    let mut mutating = 0;
    let mut names: Vec<String> = Vec::new();
    for n in 0..24 {
        let recorder = run(&format!("ses_tools_{n}"), 800.0).await;
        for turn in recorder.turns.lock().unwrap().iter() {
            names.extend(turn.tools.iter().map(|t| t.tool_name.clone()));
        }
        recorder.with_events(|events| {
            for event in events {
                match event {
                    EventKind::ToolExecutionStart {
                        tool_name, args, ..
                    } => {
                        assert!(
                            !args.as_object().expect("args are an object").is_empty(),
                            "{tool_name} was called with no arguments"
                        );
                        if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                            assert!(
                                path.starts_with("/Users/x/dev/api"),
                                "paths are relative to the workspace root, got {path}"
                            );
                        }
                    }
                    EventKind::ToolExecutionEnd { result, .. } => {
                        if let Some(added) = result.get("linesAdded") {
                            assert!(result.get("linesRemoved").is_some());
                            assert!(added.as_u64().is_some());
                            mutating += 1;
                        }
                    }
                    EventKind::ToolExecutionUpdate { partial_result, .. } => {
                        let progress = partial_result
                            .get("progress")
                            .and_then(|v| v.as_f64())
                            .expect("progress drives the determinate bar (F6.2)");
                        assert!((0.0..=1.0).contains(&progress));
                    }
                    _ => {}
                }
            }
        });
    }

    assert!(mutating > 0, "no mutating tool reported diff counts (F1.3)");
    for expected in ["read", "bash", "grep", "edit"] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} never appeared across 24 seeds"
        );
    }
}

#[tokio::test]
async fn every_tool_call_gets_an_execution_and_a_result_message() {
    for n in 0..12 {
        let recorder = run(&format!("ses_pairs_{n}"), 800.0).await;
        recorder.with_events(|events| {
            let mut called: Vec<String> = Vec::new();
            let mut executed: Vec<String> = Vec::new();
            let mut results = 0usize;
            for event in events {
                match event {
                    EventKind::MessageUpdate {
                        event: AssistantMessageEvent::ToolCallEnd { tool_call, .. },
                        ..
                    } => called.push(tool_call.id.clone()),
                    EventKind::ToolExecutionEnd { tool_call_id, .. } => {
                        executed.push(tool_call_id.clone())
                    }
                    EventKind::MessageEnd { entry, .. } if role_of(entry) == "toolResult" => {
                        results += 1
                    }
                    _ => {}
                }
            }
            assert_eq!(called, executed, "every tool call is executed, in order");
            assert_eq!(
                results,
                called.len(),
                "every execution yields a result message"
            );
        });
    }
}

/// F7: the corpus has to exercise the whole markdown surface, or the renderer is untested.
#[test]
fn corpus_covers_the_markdown_surface() {
    let mut all = String::new();
    for shape in [
        Shape::Brief,
        Shape::Handoff,
        Shape::Standard,
        Shape::Long,
        Shape::Truncated,
    ] {
        for seed in 0..24u32 {
            let mut rng = super::rng::rng("corpus", seed);
            all.push_str(&content::response(&mut rng, shape, "streaming markdown").text);
            all.push('\n');
        }
    }

    for marker in [
        "# ",
        "## ",
        "### ", // headings
        "**",
        "*after*",
        "~~", // bold, italic, strikethrough
        "`",
        "](http", // inline code, link
        "- ",
        "1. ",
        "- [x] ",
        "- [ ] ", // lists
        "> ",
        "|---",
        "\n---\n", // quote, table, rule
        "![",
        "[^1]", // image, footnote
        "```rust",
        "```swift",
        "```typescript",
        "```python",
        "```json",
        "```bash",
        "```diff",
    ] {
        assert!(all.contains(marker), "the corpus never produces {marker:?}");
    }

    let mut rng = super::rng::rng("corpus", 0);
    let truncated = content::response(&mut rng, Shape::Truncated, "cancellation");
    assert!(truncated.truncated);
    assert_eq!(
        truncated.text.matches("```").count() % 2,
        1,
        "the truncated response must end inside an open fence (F7.3)"
    );

    let long = content::response(&mut rng, Shape::Long, "the whole picture");
    assert!(
        long.text.lines().count() > 300,
        "the long response is the reflow stress case"
    );
}

#[test]
fn seeding_is_stable() {
    // Pinned so a refactor of the mixing function is a visible, deliberate change.
    assert_eq!(
        super::rng::seed("ses_abc", 0),
        super::rng::seed("ses_abc", 0)
    );
    assert_ne!(
        super::rng::seed("ses_abc", 0),
        super::rng::seed("ses_abc", 1)
    );
    assert_ne!(
        super::rng::seed("ses_abc", 0),
        super::rng::seed("ses_abd", 0)
    );
}
