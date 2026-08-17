//! Port of `.upstream/packages/ai/test/pi-messages.test.ts`, against `wiremock`
//! serving recorded fixture bodies. No test touches a real API.

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use pi_core::api::ApiClient;
use pi_core::content::AssistantContent;
use pi_core::event::{AssistantMessageEvent, DoneReason, ErrorReason};
use pi_core::message::{Message, StopReason, Usage};
use pi_core::model::{Api, CacheRetention, Model};
use pi_core::options::{ProviderResponse, SimpleStreamOptions, StreamOptions};
use pi_core::tool::Context;
use pi_core::{ThinkingLevel, UserMessage};
use pi_provider_misc::pi_messages::{option_keys, PiMessagesApi};
use pretty_assertions::assert_eq;
use serde_json::{json, Value};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn model(base_url: &str) -> Model {
    let mut model = Model::new("auto", Api::PiMessages, "radius", base_url);
    model.name = "Radius Auto".into();
    model.context_window = 128_000;
    model.max_tokens = 16_384;
    model
}

fn context() -> Context {
    Context::new(vec![Message::User(UserMessage {
        content: "Hello".into(),
        timestamp: 1,
    })])
}

fn expected_usage() -> Usage {
    Usage {
        input: 10,
        output: 5,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 15,
        cost: pi_core::Cost {
            input: 0.1,
            output: 0.2,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.3,
        },
    }
}

async fn sse_server(fixture_name: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(fixture(fixture_name)),
        )
        .mount(&server)
        .await;
    server
}

fn key_options() -> StreamOptions {
    StreamOptions {
        request: pi_core::options::RequestOptions {
            api_key: Some("test-key".into()),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test]
async fn streams_text_and_tool_calls_with_the_full_event_sequence() {
    let server = sse_server("pi_messages_text_and_tool_call.sse").await;
    let model = model(&format!("{}/v1", server.uri()));

    let mut options = key_options();
    options.max_tokens = Some(100);
    options.session_id = Some("session-1".into());
    options
        .provider_options
        .insert(option_keys::TOOL_CHOICE.into(), json!("auto"));
    options
        .request
        .headers
        .insert("x-custom".into(), Some("1".into()));

    let events = drain(
        PiMessagesApi::new()
            .stream(&model, &context(), &options)
            .await
            .unwrap(),
    )
    .await;

    assert_eq!(
        kinds(&events),
        vec![
            "start",
            "text_start",
            "text_delta",
            "text_delta",
            "text_end",
            "toolcall_start",
            "toolcall_delta",
            "toolcall_delta",
            "toolcall_end",
            "done",
        ]
    );
    assert_eq!(content_indices(&events), vec![0, 0, 0, 0, 1, 1, 1, 1]);
    assert_partials_are_pending(&events);
    assert_eq!(
        deltas(&events),
        vec!["Hel", "lo", r#"{"path":"#, r#""a.txt"}"#]
    );

    // Running snapshots accumulate text and partially-parsed tool arguments.
    let AssistantMessageEvent::TextDelta { partial, .. } = &events[2] else {
        unreachable!()
    };
    assert_eq!(partial.content[0].as_text().unwrap().text, "Hel");
    let AssistantMessageEvent::ToolCallDelta { partial, .. } = &events[6] else {
        unreachable!()
    };
    // `{"path":` alone parses to no complete key/value pair yet.
    assert!(partial.content[1]
        .as_tool_call()
        .unwrap()
        .arguments
        .is_empty());
    let AssistantMessageEvent::ToolCallDelta { partial, .. } = &events[7] else {
        unreachable!()
    };
    assert_eq!(
        partial.content[1].as_tool_call().unwrap().arguments["path"],
        "a.txt"
    );

    let AssistantMessageEvent::Done { reason, message } = events.last().unwrap() else {
        unreachable!()
    };
    assert_eq!(*reason, DoneReason::ToolUse);
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(message.usage, expected_usage());
    assert_eq!(message.response_id.as_deref(), Some("resp_1"));
    assert_eq!(message.model, "auto");
    assert_eq!(message.provider, "radius");
    assert_eq!(message.api, "pi-messages");
    assert_eq!(message.text(), "Hello");
    let call = message.tool_calls().next().unwrap();
    assert_eq!(call.id, "call_1");
    assert_eq!(call.name, "read");
    assert_eq!(call.arguments["path"], "a.txt");

    // Request shape.
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.url.path(), "/v1/messages");
    assert_eq!(request.headers["authorization"], "Bearer test-key");
    assert_eq!(request.headers["accept"], "text/event-stream");
    assert_eq!(request.headers["x-custom"], "1");
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["model"], "auto");
    assert_eq!(
        body["options"],
        json!({ "maxTokens": 100, "sessionId": "session-1", "toolChoice": "auto" })
    );
    assert_eq!(body["context"], serde_json::to_value(context()).unwrap());
}

#[tokio::test]
async fn maps_thinking_signatures_and_rewrite_diagnostics() {
    let server = sse_server("pi_messages_thinking.sse").await;
    let model = model(&format!("{}/v1", server.uri()));

    let events = drain(
        PiMessagesApi::new()
            .stream(&model, &context(), &key_options())
            .await
            .unwrap(),
    )
    .await;

    assert_eq!(
        kinds(&events),
        vec![
            "start",
            "thinking_start",
            "thinking_delta",
            "thinking_delta",
            "thinking_end",
            "text_start",
            "text_delta",
            "text_end",
            "done",
        ]
    );

    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Stop);
    let AssistantContent::Thinking(thinking) = &message.content[0] else {
        panic!("expected a thinking block")
    };
    assert_eq!(thinking.thinking, "reason");
    assert_eq!(thinking.thinking_signature.as_deref(), Some("sig-1"));
    assert!(!thinking.redacted);
    let AssistantContent::Text(text) = &message.content[1] else {
        panic!("expected a text block")
    };
    assert_eq!(text.text, "answer");
    assert_eq!(text.text_signature.as_deref(), Some("txt-sig"));

    let diagnostics = message.diagnostics.as_ref().expect("rewrite diagnostic");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code, "pi_messages_rewrite");
    assert_eq!(diagnostics[0].detail.as_ref().unwrap()["policyId"], "p1");
    assert_eq!(
        diagnostics[0].detail.as_ref().unwrap()["tokenCountChange"],
        -5
    );
}

#[tokio::test]
async fn appends_debug_and_reports_response_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(query_param("debug", "1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header("x-pi-gateway-upstream-provider", "anthropic")
                .set_body_string(fixture("pi_messages_done_only.sse")),
        )
        .mount(&server)
        .await;
    let model = model(&format!("{}/v1", server.uri()));

    let seen: Arc<Mutex<Option<ProviderResponse>>> = Arc::new(Mutex::new(None));
    let sink = seen.clone();
    let mut options = SimpleStreamOptions {
        stream: key_options(),
        reasoning: Some(ThinkingLevel::Medium),
        ..Default::default()
    };
    options
        .stream
        .provider_options
        .insert(option_keys::DEBUG.into(), json!(true));
    options.stream.request.on_response = Some(Arc::new(move |response, _model| {
        *sink.lock().unwrap() = Some(response.clone());
    }));

    let events = drain(
        PiMessagesApi::new()
            .stream_simple(&model, &context(), &options)
            .await
            .unwrap(),
    )
    .await;

    assert_eq!(terminal(&events).stop_reason, StopReason::Stop);
    let response = seen.lock().unwrap().clone().expect("onResponse fired");
    assert_eq!(response.status, 200);
    assert_eq!(
        response.headers["x-pi-gateway-upstream-provider"],
        "anthropic"
    );

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests[0].url.query(), Some("debug=1"));
    // `stream_simple` forwards the unified reasoning level.
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["options"]["reasoning"], "medium");
}

#[tokio::test]
async fn sends_cache_retention_when_requested() {
    let server = sse_server("pi_messages_done_only.sse").await;
    let model = model(&format!("{}/v1", server.uri()));

    let mut options = key_options();
    options.cache_retention = Some(CacheRetention::Long);
    options.temperature = Some(0.25);
    drain(
        PiMessagesApi::new()
            .stream(&model, &context(), &options)
            .await
            .unwrap(),
    )
    .await;

    let requests = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["options"]["cacheRetention"], "long");
    assert_eq!(body["options"]["temperature"], 0.25);
}

#[tokio::test]
async fn surfaces_backend_error_responses_with_diagnostics() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(401)
                .insert_header("content-type", "application/json")
                .set_body_string(fixture("pi_messages_unauthorized.json")),
        )
        .mount(&server)
        .await;
    let model = model(&format!("{}/v1", server.uri()));

    let mut options = key_options();
    options.request.api_key = Some("stale".into());
    let events = drain(
        PiMessagesApi::new()
            .stream(&model, &context(), &options)
            .await
            .unwrap(),
    )
    .await;

    assert_eq!(kinds(&events), vec!["error"]);
    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Error);
    let error_message = message.error_message.as_deref().unwrap();
    assert!(error_message.contains("401"), "{error_message}");
    assert!(error_message.contains("Token expired"), "{error_message}");
    assert!(error_message.contains("unauthorized"), "{error_message}");

    let diagnostics = message.diagnostics.as_ref().expect("failure diagnostic");
    assert_eq!(diagnostics[0].code, "pi_messages_response_failure");
    let detail = diagnostics[0].detail.as_ref().unwrap();
    assert_eq!(detail["status"], 401);
    assert_eq!(detail["provider"], "radius");
    assert_eq!(detail["error"]["code"], "unauthorized");
}

#[tokio::test]
async fn propagates_server_sent_error_events() {
    let server = sse_server("pi_messages_server_error.sse").await;
    let model = model(&format!("{}/v1", server.uri()));

    let events = drain(
        PiMessagesApi::new()
            .stream(&model, &context(), &key_options())
            .await
            .unwrap(),
    )
    .await;

    assert_eq!(kinds(&events), vec!["start", "error"]);
    let AssistantMessageEvent::Error { reason, error } = events.last().unwrap() else {
        unreachable!()
    };
    assert_eq!(*reason, ErrorReason::Error);
    assert_eq!(error.stop_reason, StopReason::Error);
    assert_eq!(error.error_message.as_deref(), Some("Upstream failed"));
    assert_eq!(error.usage, expected_usage());
}

#[tokio::test]
async fn errors_when_no_api_key_is_provided() {
    let model = model("http://127.0.0.1:1/v1");
    let events = drain(
        PiMessagesApi::new()
            .stream(&model, &context(), &Default::default())
            .await
            .unwrap(),
    )
    .await;

    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("No API key provided for provider \"radius\"")
    );
    assert!(message.diagnostics.is_none());
}

#[tokio::test]
async fn errors_when_the_stream_ends_without_a_terminal_event() {
    let server = sse_server("pi_messages_no_terminal.sse").await;
    let model = model(&format!("{}/v1", server.uri()));

    let events = drain(
        PiMessagesApi::new()
            .stream(&model, &context(), &key_options())
            .await
            .unwrap(),
    )
    .await;

    assert_eq!(
        kinds(&events),
        vec!["start", "text_start", "text_delta", "error"]
    );
    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Error);
    assert!(message
        .error_message
        .as_deref()
        .unwrap()
        .contains("stream ended without a terminal event"));
}

#[tokio::test]
async fn on_payload_can_replace_the_request_body() {
    let server = sse_server("pi_messages_done_only.sse").await;
    let model = model(&format!("{}/v1", server.uri()));

    let mut options = key_options();
    options.request.on_payload = Some(Arc::new(|payload, _model| {
        let mut replaced = payload.clone();
        replaced["extra"] = json!("added");
        Some(replaced)
    }));

    drain(
        PiMessagesApi::new()
            .stream(&model, &context(), &options)
            .await
            .unwrap(),
    )
    .await;

    let requests = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["extra"], "added");
}

#[tokio::test]
async fn aborting_terminates_the_stream_as_aborted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(fixture("pi_messages_done_only.sse"))
                .set_delay(std::time::Duration::from_secs(30)),
        )
        .mount(&server)
        .await;
    let model = model(&format!("{}/v1", server.uri()));

    let (handle, signal) = pi_core::options::AbortHandle::new();
    let mut options = key_options();
    options.request.signal = Some(signal);

    let stream = PiMessagesApi::new()
        .stream(&model, &context(), &options)
        .await
        .unwrap();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        handle.abort();
    });

    let events = drain(stream).await;
    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Aborted);
    assert!(message.diagnostics.is_none());
}

#[test]
fn exposes_a_registrable_provider_descriptor() {
    let registration =
        pi_provider_misc::pi_messages_provider("radius", "Radius", "https://gateway.example/v1");
    assert_eq!(registration.descriptor.id, "radius");
    assert_eq!(registration.descriptor.api, "pi-messages");
    assert_eq!(
        registration.descriptor.base_url.as_deref(),
        Some("https://gateway.example/v1")
    );
    assert_eq!(registration.client.api(), "pi-messages");
}
