//! Shared harness for the Google adapter integration tests.
//!
//! Every test drives a real adapter over `wiremock`; nothing here talks to a
//! Google endpoint.

// Each integration-test binary compiles this module in full but uses only part
// of it.
#![allow(dead_code)]

use std::sync::Arc;

use futures_util::StreamExt;
use serde_json::Value;

use pi_core::event::AssistantMessageEvent;
use pi_core::message::AssistantMessage;
use pi_core::model::{Api, Modality, Model, ModelCost, ModelCostRates};
use pi_core::AssistantMessageEventStream;
use pi_http::client::{HttpClient, HttpClientConfig};

pub fn http_client() -> Arc<HttpClient> {
    Arc::new(
        HttpClient::new(HttpClientConfig {
            // Never route a localhost mock through an ambient proxy.
            use_proxy_env: false,
            ..Default::default()
        })
        .expect("test http client"),
    )
}

pub fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

pub fn gemini_model(base_url: &str, id: &str) -> Model {
    let mut model = Model::new(id, Api::GoogleGenerativeAi, "google", base_url);
    model.reasoning = true;
    model.input = vec![Modality::Text, Modality::Image];
    model.cost = ModelCost {
        rates: ModelCostRates {
            input: 1.0,
            output: 10.0,
            cache_read: 0.1,
            cache_write: 0.0,
        },
        tiers: None,
    };
    model
}

pub fn vertex_model(base_url: &str, id: &str) -> Model {
    let mut model = Model::new(id, Api::GoogleVertex, "google-vertex", base_url);
    model.reasoning = true;
    model.input = vec![Modality::Text, Modality::Image];
    model
}

/// Drain a stream, returning the events plus a one-line description of each.
pub async fn collect_events(
    stream: AssistantMessageEventStream,
) -> (Vec<AssistantMessageEvent>, Vec<String>) {
    // `AssistantMessageEventStream::collect` is the inherent "drain to terminal
    // message" helper, so drive the `Stream` impl directly to keep every event.
    let mut stream = stream;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    let described = events.iter().map(describe).collect();
    (events, described)
}

/// Stable, readable rendering of an event, so tests can assert on the whole
/// sequence rather than only the terminal message.
pub fn describe(event: &AssistantMessageEvent) -> String {
    use AssistantMessageEvent::*;
    match event {
        Start { .. } => "start".to_string(),
        TextStart { content_index, .. } => format!("text_start#{content_index}"),
        TextDelta {
            content_index,
            delta,
            ..
        } => format!("text_delta#{content_index} {delta:?}"),
        TextEnd {
            content_index,
            content,
            ..
        } => format!("text_end#{content_index} {content:?}"),
        ThinkingStart { content_index, .. } => format!("thinking_start#{content_index}"),
        ThinkingDelta {
            content_index,
            delta,
            ..
        } => format!("thinking_delta#{content_index} {delta:?}"),
        ThinkingEnd {
            content_index,
            content,
            ..
        } => format!("thinking_end#{content_index} {content:?}"),
        ToolCallStart { content_index, .. } => format!("toolcall_start#{content_index}"),
        ToolCallDelta {
            content_index,
            delta,
            ..
        } => format!("toolcall_delta#{content_index} {delta}"),
        ToolCallEnd {
            content_index,
            tool_call,
            ..
        } => format!("toolcall_end#{content_index} {}", tool_call.name),
        Done { reason, .. } => format!("done {reason:?}"),
        Error { reason, error } => format!(
            "error {reason:?} {:?}",
            error.error_message.as_deref().unwrap_or_default()
        ),
    }
}

/// The `partial` snapshot carried by each non-terminal event, rendered as the
/// running text of the message. Proves snapshots advance rather than aliasing
/// one mutable object.
pub fn partial_texts(events: &[AssistantMessageEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| event.partial().map(AssistantMessage::text))
        .collect()
}

pub fn terminal(events: &[AssistantMessageEvent]) -> &AssistantMessage {
    events
        .last()
        .and_then(AssistantMessageEvent::terminal_message)
        .expect("stream ended without a terminal event")
}

/// The single request wiremock recorded, as JSON.
pub async fn recorded_body(server: &wiremock::MockServer) -> Value {
    let requests = server
        .received_requests()
        .await
        .expect("wiremock request recording enabled");
    assert_eq!(requests.len(), 1, "expected exactly one request");
    serde_json::from_slice(&requests[0].body).expect("request body is JSON")
}
