//! Shared harness for the adapter integration tests.
//!
//! Every test serves a recorded SSE fixture from `wiremock`; nothing here ever
//! reaches a real provider.

#![allow(dead_code)]

use std::sync::Arc;

use futures_util::StreamExt;
use pi_core::model::{ModelCompat, ModelThinkingLevel, ThinkingLevelMap};
use pi_core::options::{RequestOptions, StreamOptions};
use pi_core::{Api, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, Model};
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

pub fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("missing fixture {path:?}: {e}"))
}

/// Captures the request body so tests can assert on the payload, and replays a
/// fixture as the SSE response.
pub struct CapturingResponder {
    body: String,
    status: u16,
    captured: Arc<std::sync::Mutex<Option<Value>>>,
    captured_headers: Arc<std::sync::Mutex<Vec<(String, String)>>>,
}

impl Respond for CapturingResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        if let Ok(value) = serde_json::from_slice::<Value>(&request.body) {
            *self.captured.lock().unwrap() = Some(value);
        }
        *self.captured_headers.lock().unwrap() = request
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_lowercase(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        ResponseTemplate::new(self.status).set_body_raw(self.body.clone(), "text/event-stream")
    }
}

/// A mock provider endpoint plus handles to whatever it captured.
pub struct MockProvider {
    pub server: MockServer,
    captured: Arc<std::sync::Mutex<Option<Value>>>,
    captured_headers: Arc<std::sync::Mutex<Vec<(String, String)>>>,
}

impl MockProvider {
    pub async fn sse(endpoint: &str, fixture_name: &str) -> Self {
        Self::with_status(endpoint, &fixture(fixture_name), 200).await
    }

    pub async fn raw(endpoint: &str, body: &str, status: u16) -> Self {
        Self::with_status(endpoint, body, status).await
    }

    async fn with_status(endpoint: &str, body: &str, status: u16) -> Self {
        let server = MockServer::start().await;
        let captured = Arc::new(std::sync::Mutex::new(None));
        let captured_headers = Arc::new(std::sync::Mutex::new(Vec::new()));
        Mock::given(method("POST"))
            .and(path(endpoint.to_string()))
            .respond_with(CapturingResponder {
                body: body.to_string(),
                status,
                captured: captured.clone(),
                captured_headers: captured_headers.clone(),
            })
            .mount(&server)
            .await;
        Self {
            server,
            captured,
            captured_headers,
        }
    }

    pub fn base_url(&self) -> String {
        self.server.uri()
    }

    pub fn request_body(&self) -> Value {
        self.captured
            .lock()
            .unwrap()
            .clone()
            .expect("no request was captured")
    }

    pub fn header(&self, name: &str) -> Option<String> {
        self.captured_headers
            .lock()
            .unwrap()
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }
}

/// One event reduced to a comparable shape: kind, contentIndex and payload.
///
/// Full `AssistantMessage` snapshots are asserted separately; comparing the
/// sequence at this granularity is what catches ordering and index bugs without
/// making every test a giant literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ev {
    Start,
    TextStart(usize),
    TextDelta(usize, String),
    TextEnd(usize, String),
    ThinkingStart(usize),
    ThinkingDelta(usize, String),
    ThinkingEnd(usize, String),
    ToolCallStart(usize),
    ToolCallDelta(usize, String),
    ToolCallEnd(usize, String, String),
    Done(String),
    Error(String, String),
}

pub fn summarize(event: &AssistantMessageEvent) -> Ev {
    match event {
        AssistantMessageEvent::Start { .. } => Ev::Start,
        AssistantMessageEvent::TextStart { content_index, .. } => Ev::TextStart(*content_index),
        AssistantMessageEvent::TextDelta {
            content_index,
            delta,
            ..
        } => Ev::TextDelta(*content_index, delta.clone()),
        AssistantMessageEvent::TextEnd {
            content_index,
            content,
            ..
        } => Ev::TextEnd(*content_index, content.clone()),
        AssistantMessageEvent::ThinkingStart { content_index, .. } => {
            Ev::ThinkingStart(*content_index)
        }
        AssistantMessageEvent::ThinkingDelta {
            content_index,
            delta,
            ..
        } => Ev::ThinkingDelta(*content_index, delta.clone()),
        AssistantMessageEvent::ThinkingEnd {
            content_index,
            content,
            ..
        } => Ev::ThinkingEnd(*content_index, content.clone()),
        AssistantMessageEvent::ToolCallStart { content_index, .. } => {
            Ev::ToolCallStart(*content_index)
        }
        AssistantMessageEvent::ToolCallDelta {
            content_index,
            delta,
            ..
        } => Ev::ToolCallDelta(*content_index, delta.clone()),
        AssistantMessageEvent::ToolCallEnd {
            content_index,
            tool_call,
            ..
        } => Ev::ToolCallEnd(
            *content_index,
            tool_call.name.clone(),
            serde_json::to_string(&tool_call.arguments).unwrap(),
        ),
        AssistantMessageEvent::Done { reason, .. } => Ev::Done(
            serde_json::to_value(reason)
                .unwrap()
                .as_str()
                .unwrap()
                .into(),
        ),
        AssistantMessageEvent::Error { reason, error } => Ev::Error(
            serde_json::to_value(reason)
                .unwrap()
                .as_str()
                .unwrap()
                .into(),
            error.error_message.clone().unwrap_or_default(),
        ),
    }
}

pub struct Collected {
    pub events: Vec<AssistantMessageEvent>,
    pub sequence: Vec<Ev>,
    pub terminal: AssistantMessage,
}

impl Collected {
    /// The `partial` snapshot carried by the event at `index`.
    pub fn partial_at(&self, index: usize) -> &AssistantMessage {
        self.events[index]
            .partial()
            .expect("event carries no partial snapshot")
    }
}

pub async fn collect(stream: AssistantMessageEventStream) -> Collected {
    // Disambiguate from the inherent `AssistantMessageEventStream::collect`,
    // which drops the intermediate events we are asserting on.
    let events: Vec<AssistantMessageEvent> = StreamExt::collect(stream).await;
    let sequence = events.iter().map(summarize).collect();
    let terminal = events
        .last()
        .and_then(|e| e.terminal_message())
        .cloned()
        .expect("stream ended without a terminal event");
    Collected {
        events,
        sequence,
        terminal,
    }
}

// ---------------------------------------------------------------------------
// Model builders
// ---------------------------------------------------------------------------

pub fn model(id: &str, api: Api, provider: &str, base_url: &str) -> Model {
    let mut model = Model::new(id, api, provider, base_url);
    // Non-zero rates so cost accounting is observable in assertions.
    model.cost.rates.input = 1.0;
    model.cost.rates.output = 2.0;
    model.cost.rates.cache_read = 0.5;
    model.cost.rates.cache_write = 1.5;
    model
}

pub fn completions_model(base_url: &str) -> Model {
    model("gpt-4o-mini", Api::OpenAiCompletions, "openai", base_url)
}

pub fn responses_model(base_url: &str) -> Model {
    let mut m = model("gpt-5", Api::OpenAiResponses, "openai", base_url);
    m.reasoning = true;
    m
}

pub fn thinking_map(entries: &[(ModelThinkingLevel, Option<&str>)]) -> ThinkingLevelMap {
    entries
        .iter()
        .map(|(level, value)| (*level, value.map(str::to_string)))
        .collect()
}

pub fn compat(mutate: impl FnOnce(&mut ModelCompat)) -> ModelCompat {
    let mut compat = ModelCompat::default();
    mutate(&mut compat);
    compat
}

pub fn options_with_key(api_key: &str) -> StreamOptions {
    StreamOptions {
        request: RequestOptions {
            api_key: Some(api_key.to_string()),
            ..Default::default()
        },
        ..Default::default()
    }
}
