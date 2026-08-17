//! Wire compatibility with `pi-core`.
//!
//! PRD §4.3 claims form's transcript types are *structurally identical* to `pi-core`'s, and
//! the whole swap plan rests on it: when `pi-rs` is wired in, `form_core::protocol::wire` is
//! deleted and `pi_core`'s types are re-exported in its place. That claim was prose until
//! now. This test makes it a build failure.
//!
//! The direction that matters is **form → pi**: every payload form writes today must be
//! readable by the real harness. The reverse direction is checked where form is the reader.
//!
//! `pi-core` is a dev-dependency only. Nothing in the shipping library links it.

use form_core::protocol::wire as fw;
use serde_json::Value;

/// Serialize a form value, read it back as its `pi-core` counterpart, and require every
/// field form emitted to survive with the same value.
///
/// Not an equality check on the whole document: `pi-core` carries fields form does not model
/// yet (`responseModel`, `deferred`, `endTurn`, …), all optional and skipped when absent, so
/// pi's re-encoding is a superset. A *missing or altered* key is the failure we care about —
/// that is a rename, and a rename is the thing that breaks the swap.
fn survives<F, P>(label: &str, value: &F)
where
    F: serde::Serialize,
    P: serde::de::DeserializeOwned + serde::Serialize,
{
    let from_form = serde_json::to_value(value).expect("form value serializes");

    let as_pi: P = serde_json::from_value(from_form.clone()).unwrap_or_else(|e| {
        panic!("{label}: pi-core cannot read form's payload: {e}\n{from_form:#}")
    });

    let round_tripped = serde_json::to_value(&as_pi).expect("pi value serializes");
    assert_subset(label, &from_form, &round_tripped, "");
}

fn assert_subset(label: &str, expected: &Value, actual: &Value, path: &str) {
    match (expected, actual) {
        (Value::Object(want), Value::Object(got)) => {
            for (key, want_value) in want {
                let got_value = got.get(key).unwrap_or_else(|| {
                    panic!(
                        "{label}: pi-core dropped `{path}{key}` — the field was renamed or removed"
                    )
                });
                assert_subset(label, want_value, got_value, &format!("{path}{key}."));
            }
        }
        (Value::Array(want), Value::Array(got)) => {
            assert_eq!(
                want.len(),
                got.len(),
                "{label}: array length changed at `{path}`"
            );
            for (i, (w, g)) in want.iter().zip(got).enumerate() {
                assert_subset(label, w, g, &format!("{path}[{i}]."));
            }
        }
        (want, got) => assert_eq!(want, got, "{label}: value changed at `{path}`"),
    }
}

fn sample_usage() -> fw::Usage {
    fw::Usage {
        input: 1_200,
        output: 340,
        cache_read: 800,
        cache_write: 64,
        cache_write_1h: Some(16),
        reasoning: Some(120),
        total_tokens: 1_540,
        cost: fw::Cost {
            input: 0.006,
            output: 0.0085,
            cache_read: 0.0004,
            cache_write: 0.0004,
            total: 0.0153,
        },
    }
}

fn sample_assistant() -> fw::AssistantMessage {
    let mut tool_call = fw::ToolCall::new("toolu_01", "read");
    tool_call
        .arguments
        .insert("path".into(), Value::String("src/main.rs".into()));
    tool_call.thought_signature = Some("sig".into());
    tool_call.namespace = Some("fs".into());

    fw::AssistantMessage {
        content: vec![
            fw::AssistantContent::text("Here is the change."),
            fw::AssistantContent::Thinking(fw::ThinkingContent {
                thinking: "considering the options".into(),
                thinking_signature: Some("think-sig".into()),
                redacted: false,
            }),
            fw::AssistantContent::ToolCall(tool_call),
        ],
        api: "anthropic-messages".into(),
        provider: "anthropic".into(),
        model: "claude-opus-5".into(),
        response_id: Some("msg_01".into()),
        diagnostics: Some(vec![fw::AssistantMessageDiagnostic {
            code: "stream_retry".into(),
            message: "retried once".into(),
            detail: Some(serde_json::json!({ "attempt": 1 })),
            timestamp: Some(1_755_000_000_000),
        }]),
        usage: sample_usage(),
        stop_reason: fw::StopReason::ToolUse,
        error_message: None,
        timestamp: 1_755_000_000_000,
    }
}

#[test]
fn usage_and_cost_are_wire_identical() {
    survives::<_, pi_core::Usage>("Usage", &sample_usage());
}

#[test]
fn messages_are_wire_identical() {
    survives::<_, pi_core::AssistantMessage>("AssistantMessage", &sample_assistant());

    survives::<_, pi_core::UserMessage>("UserMessage", &fw::UserMessage::text("hello"));

    survives::<_, pi_core::UserMessage>(
        "UserMessage(blocks)",
        &fw::UserMessage {
            content: fw::UserContent::Blocks(vec![
                fw::InputContent::text("look at this"),
                fw::InputContent::Image(fw::ImageContent {
                    data: "aGVsbG8=".into(),
                    mime_type: "image/png".into(),
                }),
            ]),
            timestamp: 1_755_000_000_000,
        },
    );

    survives::<_, pi_core::ToolResultMessage>(
        "ToolResultMessage",
        &fw::ToolResultMessage {
            tool_call_id: "toolu_01".into(),
            tool_name: "read".into(),
            content: vec![fw::InputContent::text("268 lines")],
            details: Some(serde_json::json!({ "linesAdded": 268, "linesRemoved": 0 })),
            is_error: false,
            timestamp: 1_755_000_000_000,
        },
    );
}

/// The one most likely to rot, because the tags are irregular: `snake_case` throughout, but
/// the tool-call variants are `toolcall_*` (not `tool_call_*`) and `contentIndex` is
/// `camelCase` inside an otherwise snake_case union. All three inherited from the TypeScript.
#[test]
fn every_streaming_event_variant_is_wire_identical() {
    let partial = sample_assistant();
    let tool_call = fw::ToolCall::new("toolu_01", "read");

    let events = vec![
        fw::AssistantMessageEvent::Start {
            partial: partial.clone(),
        },
        fw::AssistantMessageEvent::TextStart {
            content_index: 0,
            partial: partial.clone(),
        },
        fw::AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "chunk".into(),
            partial: partial.clone(),
        },
        fw::AssistantMessageEvent::TextEnd {
            content_index: 0,
            content: "whole".into(),
            partial: partial.clone(),
        },
        fw::AssistantMessageEvent::ThinkingStart {
            content_index: 1,
            partial: partial.clone(),
        },
        fw::AssistantMessageEvent::ThinkingDelta {
            content_index: 1,
            delta: "hmm".into(),
            partial: partial.clone(),
        },
        fw::AssistantMessageEvent::ThinkingEnd {
            content_index: 1,
            content: "hmm".into(),
            partial: partial.clone(),
        },
        fw::AssistantMessageEvent::ToolCallStart {
            content_index: 2,
            partial: partial.clone(),
        },
        fw::AssistantMessageEvent::ToolCallDelta {
            content_index: 2,
            delta: "{\"path\":".into(),
            partial: partial.clone(),
        },
        fw::AssistantMessageEvent::ToolCallEnd {
            content_index: 2,
            tool_call,
            partial: partial.clone(),
        },
        fw::AssistantMessageEvent::Done {
            reason: fw::DoneReason::ToolUse,
            message: partial.clone(),
        },
        fw::AssistantMessageEvent::Error {
            reason: fw::ErrorReason::Aborted,
            error: partial,
        },
    ];

    for event in &events {
        let tag = serde_json::to_value(event).unwrap()["type"]
            .as_str()
            .expect("every event is tagged")
            .to_string();
        survives::<_, pi_core::AssistantMessageEvent>(&format!("event/{tag}"), event);
    }

    // Guard the irregular spellings by name, so normalizing them fails loudly here rather
    // than silently at the provider boundary.
    let tags: Vec<String> = events
        .iter()
        .map(|e| {
            serde_json::to_value(e).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    for expected in [
        "start",
        "text_start",
        "text_delta",
        "text_end",
        "thinking_start",
        "thinking_delta",
        "thinking_end",
        "toolcall_start",
        "toolcall_delta",
        "toolcall_end",
        "done",
        "error",
    ] {
        assert!(
            tags.contains(&expected.to_string()),
            "missing event tag `{expected}`"
        );
    }
}

#[test]
fn stop_reasons_agree() {
    for (ours, theirs) in [
        (fw::StopReason::Pending, pi_core::StopReason::Pending),
        (fw::StopReason::Stop, pi_core::StopReason::Stop),
        (fw::StopReason::Length, pi_core::StopReason::Length),
        (fw::StopReason::ToolUse, pi_core::StopReason::ToolUse),
        (fw::StopReason::Error, pi_core::StopReason::Error),
        (fw::StopReason::Aborted, pi_core::StopReason::Aborted),
        (fw::StopReason::Deferred, pi_core::StopReason::Deferred),
    ] {
        assert_eq!(
            serde_json::to_value(ours).unwrap(),
            serde_json::to_value(theirs).unwrap(),
            "stop reason spelling diverged"
        );
    }
}

/// pi writes sessions too. A transcript the real harness produces must load here, or an
/// existing session becomes unreadable the day the swap lands.
#[test]
fn pi_payloads_are_readable_by_form() {
    let pi_message =
        pi_core::AssistantMessage::pending("anthropic-messages", "anthropic", "claude-opus-5");
    let json = serde_json::to_string(&pi_message).expect("pi message serializes");

    let ours: fw::AssistantMessage =
        serde_json::from_str(&json).expect("form must read what pi writes");

    assert_eq!(ours.api, "anthropic-messages");
    assert_eq!(ours.provider, "anthropic");
    assert_eq!(ours.model, "claude-opus-5");
    assert_eq!(ours.stop_reason, fw::StopReason::Pending);
}
