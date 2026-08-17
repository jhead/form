//! Shared wiremock plumbing for the adapter tests.
//!
//! No test here touches the network beyond the local mock server.

#![allow(dead_code)]

use std::sync::Arc;

use futures_util::StreamExt;
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use pi_core::api::ApiClient;
use pi_core::content::AssistantContent;
use pi_core::event::AssistantMessageEvent;
use pi_core::message::{AssistantMessage, Message, UserMessage};
use pi_core::model::{Api, Model, ModelCompat, ModelCost, ModelCostRates};
use pi_core::options::{SimpleStreamOptions, StreamOptions};
use pi_core::tool::Context;

use pi_http::client::HttpClientConfig;
use pi_http::HttpClient;
use pi_provider_anthropic::AnthropicMessagesApi;

pub fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

/// A client that never consults proxy env vars, so the mock server is reachable
/// on any developer machine.
pub fn api() -> AnthropicMessagesApi {
    let http = HttpClient::new(HttpClientConfig {
        use_proxy_env: false,
        ..Default::default()
    })
    .expect("http client");
    AnthropicMessagesApi::with_http_client(Arc::new(http))
}

pub fn model(base_url: &str) -> Model {
    let mut model = Model::new(
        "claude-opus-4-8",
        Api::AnthropicMessages,
        "anthropic",
        base_url,
    );
    model.name = "Claude Opus 4.8".into();
    model.reasoning = true;
    model.context_window = 200_000;
    model.max_tokens = 32_000;
    model.cost = ModelCost {
        rates: ModelCostRates {
            input: 5.0,
            output: 25.0,
            cache_read: 0.5,
            cache_write: 6.25,
        },
        tiers: None,
    };
    model
}

pub fn with_compat(mut model: Model, compat: ModelCompat) -> Model {
    model.compat = Some(compat);
    model
}

pub fn user_context(text: &str) -> Context {
    Context::new(vec![Message::User(UserMessage::text(text))])
}

pub fn options_with_key() -> StreamOptions {
    let mut options = StreamOptions::default();
    options.request.api_key = Some("test-key".into());
    options
}

pub fn simple_options_with_key() -> SimpleStreamOptions {
    SimpleStreamOptions {
        stream: options_with_key(),
        ..Default::default()
    }
}

/// Start a mock `/v1/messages` endpoint that replays `body` as SSE.
pub async fn sse_server(body: String) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(body.into_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;
    server
}

/// Same, but the response headers are delayed, so an abort can land mid-request.
pub async fn delayed_sse_server(body: String, delay_ms: u64) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(body.into_bytes(), "text/event-stream")
                .set_delay(std::time::Duration::from_millis(delay_ms)),
        )
        .mount(&server)
        .await;
    server
}

pub async fn status_server(status: u16, body: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(status)
                .set_body_raw(body.as_bytes().to_vec(), "application/json"),
        )
        .mount(&server)
        .await;
    server
}

pub async fn drain(
    mut stream: pi_core::event::AssistantMessageEventStream,
) -> Vec<AssistantMessageEvent> {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    events
}

/// Run a fixture through the adapter and return every emitted event.
pub async fn run_fixture(
    fixture_name: &str,
    context: &Context,
    mutate: impl FnOnce(&mut Model, &mut StreamOptions),
) -> Vec<AssistantMessageEvent> {
    let server = sse_server(fixture(fixture_name)).await;
    let mut model = model(&server.uri());
    let mut options = options_with_key();
    mutate(&mut model, &mut options);
    let stream = api()
        .stream(&model, context, &options)
        .await
        .expect("stream starts");
    drain(stream).await
}

/// Run the adapter against an empty SSE body and return the JSON request body.
pub async fn capture_request(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
) -> (Value, Vec<(String, String)>) {
    let server = sse_server(String::new()).await;
    let mut model = model.clone();
    model.base_url = server.uri();
    let stream = api()
        .stream(&model, context, options)
        .await
        .expect("stream starts");
    drain(stream).await;
    captured(&server).await
}

/// Same, but through `stream_simple`.
pub async fn capture_simple_request(
    model: &Model,
    context: &Context,
    options: &SimpleStreamOptions,
) -> (Value, Vec<(String, String)>) {
    let server = sse_server(String::new()).await;
    let mut model = model.clone();
    model.base_url = server.uri();
    let stream = api()
        .stream_simple(&model, context, options)
        .await
        .expect("stream starts");
    drain(stream).await;
    captured(&server).await
}

async fn captured(server: &MockServer) -> (Value, Vec<(String, String)>) {
    let requests = server
        .received_requests()
        .await
        .expect("mock server records requests");
    let request = requests.first().expect("a request was made");
    let body: Value = serde_json::from_slice(&request.body).expect("request body is JSON");
    let headers = request
        .headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_lowercase(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    (body, headers)
}

pub fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// One line per event: the event kind, its `contentIndex`/payload, and a summary
/// of the running `partial` snapshot. Comparing these catches ordering, index
/// and snapshot regressions in a single assertion.
pub fn trace(events: &[AssistantMessageEvent]) -> Vec<String> {
    events.iter().map(trace_one).collect()
}

fn trace_one(event: &AssistantMessageEvent) -> String {
    match event {
        AssistantMessageEvent::Start { partial } => {
            format!("start | {}", summarize(partial))
        }
        AssistantMessageEvent::TextStart {
            content_index,
            partial,
        } => format!("text_start#{content_index} | {}", summarize(partial)),
        AssistantMessageEvent::TextDelta {
            content_index,
            delta,
            partial,
        } => format!(
            "text_delta#{content_index} {delta:?} | {}",
            summarize(partial)
        ),
        AssistantMessageEvent::TextEnd {
            content_index,
            content,
            partial,
        } => format!(
            "text_end#{content_index} {content:?} | {}",
            summarize(partial)
        ),
        AssistantMessageEvent::ThinkingStart {
            content_index,
            partial,
        } => format!("thinking_start#{content_index} | {}", summarize(partial)),
        AssistantMessageEvent::ThinkingDelta {
            content_index,
            delta,
            partial,
        } => format!(
            "thinking_delta#{content_index} {delta:?} | {}",
            summarize(partial)
        ),
        AssistantMessageEvent::ThinkingEnd {
            content_index,
            content,
            partial,
        } => format!(
            "thinking_end#{content_index} {content:?} | {}",
            summarize(partial)
        ),
        AssistantMessageEvent::ToolCallStart {
            content_index,
            partial,
        } => format!("toolcall_start#{content_index} | {}", summarize(partial)),
        AssistantMessageEvent::ToolCallDelta {
            content_index,
            delta,
            partial,
        } => format!(
            "toolcall_delta#{content_index} {delta:?} | {}",
            summarize(partial)
        ),
        AssistantMessageEvent::ToolCallEnd {
            content_index,
            tool_call,
            partial,
        } => format!(
            "toolcall_end#{content_index} {}={} | {}",
            tool_call.name,
            serde_json::to_string(&tool_call.arguments).unwrap_or_default(),
            summarize(partial)
        ),
        AssistantMessageEvent::Done { reason, message } => {
            format!("done {reason:?} | {}", summarize_message(message))
        }
        AssistantMessageEvent::Error { reason, error } => format!(
            "error {reason:?} {:?} | {}",
            error.error_message.as_deref().unwrap_or(""),
            summarize_message(error)
        ),
    }
}

fn summarize(message: &AssistantMessage) -> String {
    format!("[{}]", blocks(message))
}

fn summarize_message(message: &AssistantMessage) -> String {
    format!(
        "[{}] stop={:?} usage={}/{}",
        blocks(message),
        message.stop_reason,
        message.usage.input,
        message.usage.output
    )
}

fn blocks(message: &AssistantMessage) -> String {
    message
        .content
        .iter()
        .map(|block| match block {
            AssistantContent::Text(text) => format!("text{:?}", text.text),
            AssistantContent::Thinking(thinking) => format!(
                "thinking{:?}/sig{:?}{}",
                thinking.thinking,
                thinking.thinking_signature.as_deref().unwrap_or(""),
                if thinking.redacted { "/redacted" } else { "" }
            ),
            AssistantContent::ToolCall(call) => format!(
                "toolCall:{}{}",
                call.name,
                serde_json::to_string(&call.arguments).unwrap_or_default()
            ),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn terminal(events: &[AssistantMessageEvent]) -> &AssistantMessage {
    events
        .last()
        .and_then(AssistantMessageEvent::terminal_message)
        .expect("stream ends with a terminal event")
}
