//! Shared helpers for the wiremock-backed adapter tests.
//!
//! Compiled separately into each test binary, so not every helper is used by
//! every one of them.
#![allow(dead_code)]

use pi_core::event::{AssistantMessageEvent, AssistantMessageEventStream};
use pi_core::message::AssistantMessage;

/// Load a recorded response body from `tests/fixtures/`.
pub fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

pub async fn drain(mut stream: AssistantMessageEventStream) -> Vec<AssistantMessageEvent> {
    let mut events = Vec::new();
    while let Some(event) = futures_util::StreamExt::next(&mut stream).await {
        events.push(event);
    }
    events
}

pub fn kind(event: &AssistantMessageEvent) -> &'static str {
    match event {
        AssistantMessageEvent::Start { .. } => "start",
        AssistantMessageEvent::TextStart { .. } => "text_start",
        AssistantMessageEvent::TextDelta { .. } => "text_delta",
        AssistantMessageEvent::TextEnd { .. } => "text_end",
        AssistantMessageEvent::ThinkingStart { .. } => "thinking_start",
        AssistantMessageEvent::ThinkingDelta { .. } => "thinking_delta",
        AssistantMessageEvent::ThinkingEnd { .. } => "thinking_end",
        AssistantMessageEvent::ToolCallStart { .. } => "toolcall_start",
        AssistantMessageEvent::ToolCallDelta { .. } => "toolcall_delta",
        AssistantMessageEvent::ToolCallEnd { .. } => "toolcall_end",
        AssistantMessageEvent::Done { .. } => "done",
        AssistantMessageEvent::Error { .. } => "error",
    }
}

pub fn kinds(events: &[AssistantMessageEvent]) -> Vec<&'static str> {
    events.iter().map(kind).collect()
}

pub fn content_indices(events: &[AssistantMessageEvent]) -> Vec<usize> {
    events
        .iter()
        .filter_map(|event| match event {
            AssistantMessageEvent::TextStart { content_index, .. }
            | AssistantMessageEvent::TextDelta { content_index, .. }
            | AssistantMessageEvent::TextEnd { content_index, .. }
            | AssistantMessageEvent::ThinkingStart { content_index, .. }
            | AssistantMessageEvent::ThinkingDelta { content_index, .. }
            | AssistantMessageEvent::ThinkingEnd { content_index, .. }
            | AssistantMessageEvent::ToolCallStart { content_index, .. }
            | AssistantMessageEvent::ToolCallDelta { content_index, .. }
            | AssistantMessageEvent::ToolCallEnd { content_index, .. } => Some(*content_index),
            _ => None,
        })
        .collect()
}

pub fn deltas(events: &[AssistantMessageEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AssistantMessageEvent::TextDelta { delta, .. }
            | AssistantMessageEvent::ThinkingDelta { delta, .. }
            | AssistantMessageEvent::ToolCallDelta { delta, .. } => Some(delta.clone()),
            _ => None,
        })
        .collect()
}

pub fn terminal(events: &[AssistantMessageEvent]) -> AssistantMessage {
    events
        .last()
        .and_then(AssistantMessageEvent::terminal_message)
        .cloned()
        .expect("stream ended with a terminal event")
}

/// Every non-terminal event carries a snapshot and every terminal event does not.
pub fn assert_partial_presence(events: &[AssistantMessageEvent]) {
    for event in events {
        assert_eq!(
            event.partial().is_some(),
            !event.is_terminal(),
            "{} has the wrong partial/terminal shape",
            kind(event)
        );
    }
    assert!(
        events
            .last()
            .is_some_and(AssistantMessageEvent::is_terminal),
        "stream did not end with a terminal event"
    );
}

/// As [`assert_partial_presence`], and additionally that no snapshot has
/// already adopted a terminal stop reason.
///
/// Adapters that learn the stop reason from the same wire chunk that carries
/// the last delta (Mistral does) legitimately fail this; use the weaker check
/// there.
pub fn assert_partials_are_pending(events: &[AssistantMessageEvent]) {
    assert_partial_presence(events);
    for event in events {
        if let Some(partial) = event.partial() {
            assert_eq!(
                partial.stop_reason,
                pi_core::StopReason::Pending,
                "{} carried a terminal partial",
                kind(event)
            );
        }
    }
}
