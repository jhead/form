//! Live provider check. Ignored by default: it needs a network and a real key.
//!
//!   cargo test -p form-core --test openrouter_live -- --ignored --nocapture

use form_core::harness::pi::PiHarness;
use form_core::protocol::{ModelRef, ThinkingLevel};

/// Override with FORM_TEST_MODEL. The default is a free model that is actually serving:
/// several of the free tier return "Provider returned error" with an empty stream.
fn test_model() -> String {
    std::env::var("FORM_TEST_MODEL")
        .unwrap_or_else(|_| "nvidia/nemotron-3-super-120b-a12b:free".to_string())
}

fn model(id: &str) -> ModelRef {
    ModelRef {
        provider_id: "openrouter".into(),
        model_id: id.into(),
        thinking_level: ThinkingLevel::Off,
    }
}

#[tokio::test]
#[ignore]
async fn resolves_and_streams() {
    form_core::env::load(std::path::Path::new("."));
    form_core::env::load(std::path::Path::new("../../.."));
    assert!(
        std::env::var("OPENROUTER_API_KEY").is_ok(),
        "OPENROUTER_API_KEY not set; put OPENROUTER_KEY in .env"
    );

    let harness = PiHarness::new("You are terse.".into())
        .await
        .expect("harness builds");
    println!("catalog models: {}", harness.model_count());

    for id in [
        "z-ai/glm-5.2:free",
        "google/gemma-4-31b-it:free",
        "nvidia/nemotron-3-super-120b-a12b:free",
    ] {
        match harness.resolve(&model(id)).await {
            Ok(m) => println!(
                "  resolved {id} -> ctx {} max {}",
                m.context_window, m.max_tokens
            ),
            Err(e) => println!("  FAILED {id}: {e}"),
        }
    }
}

/// Drive a real run through the harness and print every event it emits.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn streams_a_real_completion() {
    use form_core::app::TurnRecord;
    use form_core::harness::{AbortSignal, Harness, RunContext, RunRequest};
    use form_core::protocol::{now_ms, AssistantMessageEvent, Entry, EntryKind, EventKind};
    use std::sync::{Arc, Mutex};

    let found = form_core::env::load(std::path::Path::new("."));
    println!(
        "env file: {found:?}  key set: {}",
        std::env::var("OPENROUTER_API_KEY").is_ok()
    );

    struct Probe {
        events: Mutex<Vec<String>>,
        text: Mutex<String>,
        count: Mutex<usize>,
    }
    fn tag(value: &impl serde::Serialize) -> String {
        serde_json::to_value(value)
            .ok()
            .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_string))
            .unwrap_or_default()
    }
    impl RunContext for Probe {
        fn emit(&self, kind: EventKind) {
            let label = match &kind {
                EventKind::MessageUpdate { event, .. } => {
                    if let AssistantMessageEvent::TextDelta { delta, .. } = event {
                        self.text.lock().unwrap().push_str(delta);
                    }
                    tag(event)
                }
                EventKind::Error { message, .. } => format!("ERROR: {message}"),
                EventKind::MessageEnd { entry, .. } => {
                    format!(
                        "message_end {}",
                        serde_json::to_string(&entry.kind).unwrap_or_default()
                    )
                }
                other => tag(other),
            };
            self.events.lock().unwrap().push(label);
        }
        fn append_entry(&self, session_id: &str, kind: EntryKind) -> Option<Entry> {
            let mut count = self.count.lock().unwrap();
            *count += 1;
            Some(Entry {
                id: format!("ent_{count}"),
                session_id: session_id.to_string(),
                seq: *count as u64,
                parent_id: None,
                timestamp: now_ms(),
                kind,
            })
        }
        fn replace_entry(&self, _entry: &Entry) {}
        fn speed(&self) -> f64 {
            1.0
        }
        fn record_turn(&self, _turn: TurnRecord) {}
    }

    let harness = PiHarness::new("You are terse.".into())
        .await
        .expect("harness");
    let probe = Arc::new(Probe {
        events: Mutex::new(Vec::new()),
        text: Mutex::new(String::new()),
        count: Mutex::new(0),
    });
    let ctx: Arc<dyn RunContext> = probe.clone();

    harness
        .run(
            RunRequest {
                session_id: "ses_test".into(),
                run_id: "run_test".into(),
                command_id: None,
                prompt: "Reply with exactly: hello from openrouter".into(),
                model: model(&test_model()),
                workspace_root: None,
                turn_index: 0,
            },
            ctx,
            AbortSignal::new(),
        )
        .await;

    let events = probe.events.lock().unwrap().clone();
    println!("events ({}): {:?}", events.len(), events);
    println!("text: {:?}", probe.text.lock().unwrap());
}

/// A real tool call against a real workspace: the model must read a file to answer.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn uses_tools_against_a_workspace() {
    use form_core::app::TurnRecord;
    use form_core::harness::{AbortSignal, Harness, RunContext, RunRequest};
    use form_core::protocol::{now_ms, Entry, EntryKind, EventKind};
    use std::sync::{Arc, Mutex};

    form_core::env::load(std::path::Path::new("."));

    #[derive(Default)]
    struct Probe {
        tools: Mutex<Vec<String>>,
        turns: Mutex<Vec<TurnRecord>>,
        count: Mutex<usize>,
    }
    impl RunContext for Probe {
        fn emit(&self, kind: EventKind) {
            if let EventKind::ToolExecutionStart { tool_name, .. } = &kind {
                self.tools.lock().unwrap().push(tool_name.clone());
            }
        }
        fn append_entry(&self, session_id: &str, kind: EntryKind) -> Option<Entry> {
            let mut count = self.count.lock().unwrap();
            *count += 1;
            Some(Entry {
                id: format!("ent_{count}"),
                session_id: session_id.to_string(),
                seq: *count as u64,
                parent_id: None,
                timestamp: now_ms(),
                kind,
            })
        }
        fn replace_entry(&self, _entry: &Entry) {}
        fn speed(&self) -> f64 {
            1.0
        }
        fn record_turn(&self, turn: TurnRecord) {
            self.turns.lock().unwrap().push(turn);
        }
    }

    let harness = PiHarness::new(
        "You are a coding agent. Use the read tool to inspect files before answering.".into(),
    )
    .await
    .expect("harness");
    let probe = Arc::new(Probe::default());
    let ctx: Arc<dyn RunContext> = probe.clone();

    harness
        .run(
            RunRequest {
                session_id: "ses_tools".into(),
                run_id: "run_tools".into(),
                command_id: None,
                prompt: "Read notes.txt in this directory and tell me the build number.".into(),
                model: model(&test_model()),
                workspace_root: Some("/tmp/form-ws".into()),
                turn_index: 0,
            },
            ctx,
            AbortSignal::new(),
        )
        .await;

    let tools = probe.tools.lock().unwrap().clone();
    let turns = probe.turns.lock().unwrap().clone();
    println!("tools invoked: {tools:?}");
    for turn in &turns {
        println!(
            "turn ttft={:?}ms duration={}ms tools_recorded={} tokens={}",
            turn.ttft_ms,
            turn.duration_ms,
            turn.tools.len(),
            turn.usage.total_tokens
        );
    }
}

/// Stopping a live run reports `aborted`, promptly, and keeps what streamed so far.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn aborting_a_live_run_reports_aborted() {
    use form_core::app::TurnRecord;
    use form_core::harness::{AbortSignal, Harness, RunContext, RunRequest};
    use form_core::protocol::{
        now_ms, AssistantMessageEvent, Entry, EntryKind, EventKind, RunOutcome,
    };
    use std::sync::{Arc, Mutex};

    form_core::env::load(std::path::Path::new("."));

    struct Probe {
        abort: AbortSignal,
        text: Mutex<String>,
        outcome: Mutex<Option<RunOutcome>>,
        count: Mutex<usize>,
    }
    impl RunContext for Probe {
        fn emit(&self, kind: EventKind) {
            match &kind {
                // Stop as soon as the model is genuinely producing output.
                EventKind::MessageUpdate {
                    event: AssistantMessageEvent::TextDelta { delta, .. },
                    ..
                } => {
                    let mut text = self.text.lock().unwrap();
                    text.push_str(delta);
                    if text.len() > 20 {
                        self.abort.abort();
                    }
                }
                EventKind::RunEnd { outcome, .. } => {
                    *self.outcome.lock().unwrap() = Some(*outcome);
                }
                _ => {}
            }
        }
        fn append_entry(&self, session_id: &str, kind: EntryKind) -> Option<Entry> {
            let mut count = self.count.lock().unwrap();
            *count += 1;
            Some(Entry {
                id: format!("ent_{count}"),
                session_id: session_id.to_string(),
                seq: *count as u64,
                parent_id: None,
                timestamp: now_ms(),
                kind,
            })
        }
        fn replace_entry(&self, _entry: &Entry) {}
        fn speed(&self) -> f64 {
            1.0
        }
        fn record_turn(&self, _turn: TurnRecord) {}
    }

    let harness = PiHarness::new("You are verbose.".into())
        .await
        .expect("harness");
    let abort = AbortSignal::new();
    let probe = Arc::new(Probe {
        abort: abort.clone(),
        text: Mutex::new(String::new()),
        outcome: Mutex::new(None),
        count: Mutex::new(0),
    });
    let ctx: Arc<dyn RunContext> = probe.clone();

    let started = std::time::Instant::now();
    harness
        .run(
            RunRequest {
                session_id: "ses_abort".into(),
                run_id: "run_abort".into(),
                command_id: None,
                prompt: "Count slowly from 1 to 300, one number per line.".into(),
                model: model(&test_model()),
                workspace_root: None,
                turn_index: 0,
            },
            ctx,
            abort,
        )
        .await;

    let elapsed = started.elapsed();
    let outcome = *probe.outcome.lock().unwrap();
    let kept = probe.text.lock().unwrap().len();
    println!("outcome={outcome:?} elapsed={elapsed:?} kept={kept} chars");
    assert_eq!(
        outcome,
        Some(RunOutcome::Aborted),
        "stopping must report aborted"
    );
    assert!(kept > 0, "the partial response must survive the stop");
}

/// The Providers pane must agree with what the core can actually resolve.
///
/// `#[ignore]`d with the rest of this file: it needs a credential on the machine, and a fresh
/// clone has none. It failed on exactly that — a test that only passes on the author's laptop
/// is worse than no test, because it turns a clean checkout into a red build.
#[test]
#[ignore]
fn has_key_reflects_a_key_supplied_through_the_environment() {
    form_core::env::load(std::path::Path::new("."));
    let resolvable = form_core::credentials::providers_with_keys();
    assert!(
        resolvable.iter().any(|p| p == "openrouter"),
        "the .env key should make openrouter resolvable, got {resolvable:?}"
    );
}

/// What *can* be asserted without a credential: resolution is total and never panics, and a
/// provider nobody has a key for is simply absent.
#[test]
fn credential_resolution_is_total_without_any_key() {
    let resolvable = form_core::credentials::providers_with_keys();
    for provider in &resolvable {
        assert!(
            form_core::credentials::KNOWN_PROVIDERS.contains(&provider.as_str()),
            "{provider} is not a provider the core looks up"
        );
    }
    assert_eq!(form_core::credentials::api_key("nope-not-a-provider"), None);
}

/// Print the streamed event shape, to compare delta application against the provider's own
/// `partial`. This is a diagnostic, not an assertion.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn dump_event_shape() {
    use form_core::app::TurnRecord;
    use form_core::harness::{AbortSignal, Harness, RunContext, RunRequest};
    use form_core::protocol::{now_ms, AssistantMessageEvent, Entry, EntryKind, EventKind};
    use std::sync::{Arc, Mutex};

    form_core::env::load(std::path::Path::new("."));

    struct Probe {
        lines: Mutex<Vec<String>>,
        count: Mutex<usize>,
    }
    impl RunContext for Probe {
        fn emit(&self, kind: EventKind) {
            if let EventKind::MessageUpdate { event, .. } = &kind {
                let (name, index) = match event {
                    AssistantMessageEvent::Start { .. } => ("start", None),
                    AssistantMessageEvent::TextStart { content_index, .. } => {
                        ("text_start", Some(*content_index))
                    }
                    AssistantMessageEvent::TextDelta { content_index, .. } => {
                        ("text_delta", Some(*content_index))
                    }
                    AssistantMessageEvent::TextEnd { content_index, .. } => {
                        ("text_end", Some(*content_index))
                    }
                    AssistantMessageEvent::ThinkingStart { content_index, .. } => {
                        ("thinking_start", Some(*content_index))
                    }
                    AssistantMessageEvent::ThinkingDelta { content_index, .. } => {
                        ("thinking_delta", Some(*content_index))
                    }
                    AssistantMessageEvent::ThinkingEnd { content_index, .. } => {
                        ("thinking_end", Some(*content_index))
                    }
                    AssistantMessageEvent::ToolCallStart { content_index, .. } => {
                        ("toolcall_start", Some(*content_index))
                    }
                    AssistantMessageEvent::ToolCallDelta { content_index, .. } => {
                        ("toolcall_delta", Some(*content_index))
                    }
                    AssistantMessageEvent::ToolCallEnd { content_index, .. } => {
                        ("toolcall_end", Some(*content_index))
                    }
                    AssistantMessageEvent::Done { .. } => ("done", None),
                    AssistantMessageEvent::Error { .. } => ("error", None),
                };
                let shape: Vec<&str> = event
                    .partial()
                    .map(|p| {
                        p.content
                            .iter()
                            .map(|c| match c {
                                form_core::protocol::AssistantContent::Text(_) => "text",
                                form_core::protocol::AssistantContent::Thinking(_) => "thinking",
                                form_core::protocol::AssistantContent::ToolCall(_) => "toolCall",
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let mut lines = self.lines.lock().unwrap();
                let line = format!("{name} idx={index:?} partial={shape:?}");
                if lines.last() != Some(&line) {
                    lines.push(line);
                }
            }
        }
        fn append_entry(&self, session_id: &str, kind: EntryKind) -> Option<Entry> {
            let mut count = self.count.lock().unwrap();
            *count += 1;
            Some(Entry {
                id: format!("ent_{count}"),
                session_id: session_id.to_string(),
                seq: *count as u64,
                parent_id: None,
                timestamp: now_ms(),
                kind,
            })
        }
        fn replace_entry(&self, _entry: &Entry) {}
        fn speed(&self) -> f64 {
            1.0
        }
        fn record_turn(&self, _turn: TurnRecord) {}
    }

    let harness = PiHarness::new("You are terse.".into())
        .await
        .expect("harness");
    let probe = Arc::new(Probe {
        lines: Mutex::new(Vec::new()),
        count: Mutex::new(0),
    });
    let ctx: Arc<dyn RunContext> = probe.clone();
    harness
        .run(
            RunRequest {
                session_id: "ses_shape".into(),
                run_id: "run_shape".into(),
                command_id: None,
                prompt: "Say hi in three words.".into(),
                model: model(&test_model()),
                workspace_root: None,
                turn_index: 0,
            },
            ctx,
            AbortSignal::new(),
        )
        .await;

    for line in probe.lines.lock().unwrap().iter() {
        println!("{line}");
    }
}

/// One prompt must leave exactly one user entry in the transcript.
///
/// `Core::start_run` appends the prompt so the UI can render it immediately, and pi replays it
/// as the run's first message. Appending both duplicated every message the user sent.
// A plain test on purpose: `Core::new` builds its own runtime, and `block_on` inside a
// `#[tokio::test]` panics with "cannot start a runtime from within a runtime".
#[test]
#[ignore]
fn a_prompt_appears_once_in_the_transcript() {
    use form_core::protocol::{Command, CoreConfig, EntryKind, Message, Query};

    form_core::env::load(std::path::Path::new("."));

    let dir = std::env::temp_dir().join(format!("form-dup-{}", uuid::Uuid::new_v4().simple()));
    let core = form_core::Core::new(CoreConfig {
        data_dir: dir.to_string_lossy().into_owned(),
        seed_mock_data: false,
        log_level: "warn".into(),
        harness_speed: 1.0,
        harness: form_core::protocol::HarnessKind::Pi,
    })
    .expect("core starts");

    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = done.clone();
    core.subscribe(std::sync::Arc::new(move |event: &form_core::Event| {
        if matches!(event.kind, form_core::EventKind::RunEnd { .. }) {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }));

    let created = core.dispatch_json(&serde_json::json!({ "type": "createSession" }).to_string());
    assert!(
        created.contains("\"ok\":true"),
        "createSession failed: {created}"
    );
    let list = core.query_json(
        &serde_json::to_string(&Query::ListSessions {
            include_archived: false,
        })
        .unwrap(),
    );
    let parsed: serde_json::Value = serde_json::from_str(&list).unwrap();
    let session_id = parsed["data"]["sessions"][0]["id"]
        .as_str()
        .expect("session")
        .to_string();

    let prompt = "Say OK";
    core.dispatch_json(
        &serde_json::to_string(&Command::SendPrompt {
            session_id: session_id.clone(),
            text: prompt.into(),
            attachment_ids: Vec::new(),
        })
        .unwrap(),
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    while !done.load(std::sync::atomic::Ordering::SeqCst) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let session =
        core.query_json(&serde_json::to_string(&Query::GetSession { session_id }).unwrap());
    let parsed: serde_json::Value = serde_json::from_str(&session).unwrap();
    let entries: Vec<form_core::protocol::Entry> =
        serde_json::from_value(parsed["data"]["entries"].clone()).expect("entries");

    let user_entries: Vec<_> = entries
        .iter()
        .filter(|e| {
            matches!(
                &e.kind,
                EntryKind::Message {
                    message: Message::User(_)
                }
            )
        })
        .collect();

    println!(
        "entries={} user_entries={}",
        entries.len(),
        user_entries.len()
    );
    assert_eq!(user_entries.len(), 1, "the prompt must appear exactly once");

    let _ = std::fs::remove_dir_all(&dir);
}
