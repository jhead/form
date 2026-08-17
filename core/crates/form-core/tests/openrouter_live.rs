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
