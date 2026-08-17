//! `google-generative-ai` adapter behaviour against recorded Gemini streams.

mod common;

use common::*;
use pretty_assertions::assert_eq;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use pi_core::content::AssistantContent;
use pi_core::message::{Message, StopReason, UserMessage};
use pi_core::model::ThinkingLevel;
use pi_core::options::{AbortHandle, SimpleStreamOptions, StreamOptions};
use pi_core::tool::{Context, Tool};
use pi_core::ApiClient;
use pi_provider_google::{GoogleGenerativeAiClient, GoogleStreamOptionsExt, GoogleToolChoice};

const MODEL_ID: &str = "gemini-2.5-flash";

fn sse(fixture_name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(fixture(fixture_name), "text/event-stream")
}

async fn mock_stream(fixture_name: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/v1beta/models/{MODEL_ID}:streamGenerateContent"
        )))
        .and(query_param("alt", "sse"))
        .and(header("x-goog-api-key", "test-api-key"))
        .respond_with(sse(fixture_name))
        .mount(&server)
        .await;
    server
}

fn options() -> StreamOptions {
    let mut options = StreamOptions::default();
    options.request.api_key = Some("test-api-key".into());
    options
}

fn context() -> Context {
    Context::new(vec![Message::User(UserMessage::text("hello"))])
}

fn base_url(server: &MockServer) -> String {
    format!("{}/v1beta", server.uri())
}

#[tokio::test]
async fn streams_text_with_a_full_event_sequence() {
    let server = mock_stream("text_stream.sse").await;
    let model = gemini_model(&base_url(&server), MODEL_ID);
    let client = GoogleGenerativeAiClient::with_http_client(http_client());

    let stream = client.stream(&model, &context(), &options()).await.unwrap();
    let (events, described) = collect_events(stream).await;

    assert_eq!(
        described,
        vec![
            "start",
            "text_start#0",
            "text_delta#0 \"Hello\"",
            "text_delta#0 \" world\"",
            "text_delta#0 \"!\"",
            "text_end#0 \"Hello world!\"",
            "done Stop",
        ]
    );
    // Each partial is a point-in-time snapshot, not an alias of the final message.
    assert_eq!(
        partial_texts(&events),
        vec![
            "",
            "",
            "Hello",
            "Hello world",
            "Hello world!",
            "Hello world!"
        ]
    );

    let message = terminal(&events);
    assert_eq!(message.api, "google-generative-ai");
    assert_eq!(message.provider, "google");
    assert_eq!(message.model, MODEL_ID);
    assert_eq!(message.response_id.as_deref(), Some("resp-1"));
    assert_eq!(
        message.response_model.as_deref(),
        Some("gemini-2.5-flash-002")
    );
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("STOP"));
    assert_eq!(message.text(), "Hello world!");
    assert_eq!(message.usage.input, 12);
    assert_eq!(message.usage.output, 3);
    assert_eq!(message.usage.total_tokens, 15);
    assert_eq!(message.usage.reasoning, Some(0));
    // 12 in @ $1/Mtok + 3 out @ $10/Mtok.
    assert!((message.usage.cost.total - (12.0 + 30.0) / 1_000_000.0).abs() < 1e-12);
}

#[tokio::test]
async fn maps_thinking_signatures_tool_calls_and_reasoning_usage() {
    let server = mock_stream("thinking_tool_call.sse").await;
    let model = gemini_model(&base_url(&server), MODEL_ID);
    let client = GoogleGenerativeAiClient::with_http_client(http_client());

    let stream = client.stream(&model, &context(), &options()).await.unwrap();
    let (events, described) = collect_events(stream).await;

    assert_eq!(
        described,
        vec![
            "start",
            "thinking_start#0",
            "thinking_delta#0 \"Let me think\"",
            "thinking_delta#0 \" harder\"",
            "thinking_end#0 \"Let me think harder\"",
            "text_start#1",
            "text_delta#1 \"Running it.\"",
            "text_end#1 \"Running it.\"",
            "toolcall_start#2",
            "toolcall_delta#2 {\"command\":\"ls\"}",
            "toolcall_end#2 bash",
            "done ToolUse",
        ]
    );

    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("STOP"));
    assert_eq!(message.content.len(), 3);

    let thinking = message.content[0].as_thinking().unwrap();
    assert_eq!(thinking.thinking, "Let me think harder");
    // The signature arrived only on the first delta and must be retained.
    assert_eq!(thinking.thinking_signature.as_deref(), Some("c2lnbmF0dXJl"));

    let call = message.content[2].as_tool_call().unwrap();
    assert_eq!(call.id, "call_1");
    assert_eq!(call.name, "bash");
    assert_eq!(call.arguments["command"], "ls");
    assert_eq!(call.thought_signature.as_deref(), Some("dG9vbHNpZw=="));

    // input excludes cached tokens; output folds in thinking tokens.
    assert_eq!(message.usage.input, 80);
    assert_eq!(message.usage.output, 70);
    assert_eq!(message.usage.cache_read, 20);
    assert_eq!(message.usage.reasoning, Some(40));
    assert_eq!(message.usage.total_tokens, 190);
}

#[tokio::test]
async fn regenerates_duplicate_and_missing_tool_call_ids() {
    let server = mock_stream("duplicate_tool_call_ids.sse").await;
    let model = gemini_model(&base_url(&server), MODEL_ID);
    let client = GoogleGenerativeAiClient::with_http_client(http_client());

    let stream = client.stream(&model, &context(), &options()).await.unwrap();
    let (events, described) = collect_events(stream).await;

    assert_eq!(
        described,
        vec![
            "start",
            "toolcall_start#0",
            "toolcall_delta#0 {\"command\":\"ls\"}",
            "toolcall_end#0 bash",
            "toolcall_start#1",
            "toolcall_delta#1 {\"command\":\"pwd\"}",
            "toolcall_end#1 bash",
            "toolcall_start#2",
            "toolcall_delta#2 {}",
            "toolcall_end#2 bash",
            "done ToolUse",
        ]
    );

    let calls: Vec<_> = terminal(&events).tool_calls().collect();
    assert_eq!(calls[0].id, "call_dup");
    assert_ne!(calls[1].id, "call_dup", "duplicate id must be regenerated");
    assert!(calls[1].id.starts_with("bash_"));
    assert!(calls[2].id.starts_with("bash_"));
    assert_ne!(calls[1].id, calls[2].id);
}

#[tokio::test]
async fn max_tokens_with_a_tool_call_stays_length() {
    let server = mock_stream("max_tokens_tool_call.sse").await;
    let model = gemini_model(&base_url(&server), MODEL_ID);
    let client = GoogleGenerativeAiClient::with_http_client(http_client());

    let stream = client.stream(&model, &context(), &options()).await.unwrap();
    let (events, described) = collect_events(stream).await;

    assert_eq!(described.last().unwrap(), "done Length");
    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Length);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("MAX_TOKENS"));
    assert!(message
        .content
        .iter()
        .any(|block| matches!(block, AssistantContent::ToolCall(_))));
}

#[tokio::test]
async fn safety_finish_reason_becomes_an_error_event() {
    let server = mock_stream("safety_block.sse").await;
    let model = gemini_model(&base_url(&server), MODEL_ID);
    let client = GoogleGenerativeAiClient::with_http_client(http_client());

    let stream = client.stream(&model, &context(), &options()).await.unwrap();
    let (events, described) = collect_events(stream).await;

    assert_eq!(
        described,
        vec!["start", "error Error \"Provider stopped with: SAFETY\""]
    );
    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("SAFETY"));
}

#[tokio::test]
async fn a_stream_without_a_finish_reason_is_an_error() {
    let server = mock_stream("no_finish_reason.sse").await;
    let model = gemini_model(&base_url(&server), MODEL_ID);
    let client = GoogleGenerativeAiClient::with_http_client(http_client());

    let stream = client.stream(&model, &context(), &options()).await.unwrap();
    let (events, described) = collect_events(stream).await;

    assert_eq!(
        described,
        vec![
            "start",
            "text_start#0",
            "text_delta#0 \"partial\"",
            "text_end#0 \"partial\"",
            "error Error \"Google stream ended without a finish reason\"",
        ]
    );
    // The partial content survives on the error message.
    assert_eq!(terminal(&events).text(), "partial");
}

#[tokio::test]
async fn a_mid_stream_error_envelope_terminates_the_stream() {
    let server = mock_stream("mid_stream_error.sse").await;
    let model = gemini_model(&base_url(&server), MODEL_ID);
    let client = GoogleGenerativeAiClient::with_http_client(http_client());

    let stream = client.stream(&model, &context(), &options()).await.unwrap();
    let (events, described) = collect_events(stream).await;

    // The error is raised mid-loop, so the open text block never gets a
    // `text_end` — exactly as upstream's `throw` from inside the `for await`.
    assert_eq!(
        &described[..3],
        &["start", "text_start#0", "text_delta#0 \"start\""]
    );
    assert_eq!(described.len(), 4);
    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Error);
    let error = message.error_message.as_deref().unwrap();
    assert!(
        error.starts_with("got status: RESOURCE_EXHAUSTED. "),
        "unexpected error text: {error}"
    );
    assert!(error.contains("Quota exceeded for quota metric"));
}

#[tokio::test]
async fn an_http_error_is_reported_in_the_stream_not_as_err() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
            "error": {"code": 429, "message": "Resource exhausted", "status": "RESOURCE_EXHAUSTED"}
        })))
        .mount(&server)
        .await;
    let model = gemini_model(&base_url(&server), MODEL_ID);
    let client = GoogleGenerativeAiClient::with_http_client(http_client());

    let stream = client.stream(&model, &context(), &options()).await.unwrap();
    let (events, described) = collect_events(stream).await;

    // No `start` event: the request never produced a body.
    assert_eq!(described.len(), 1);
    assert!(described[0].starts_with("error Error"));
    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Error);
    // Upstream surfaces the provider's raw JSON error body.
    assert_eq!(
        message.error_message.as_deref(),
        Some(
            r#"{"error":{"code":429,"message":"Resource exhausted","status":"RESOURCE_EXHAUSTED"}}"#
        )
    );
}

#[tokio::test]
async fn missing_api_key_is_an_error_event() {
    let server = mock_stream("text_stream.sse").await;
    let model = gemini_model(&base_url(&server), MODEL_ID);
    let client = GoogleGenerativeAiClient::with_http_client(http_client());

    let stream = client
        .stream(&model, &context(), &StreamOptions::default())
        .await
        .unwrap();
    let (events, described) = collect_events(stream).await;

    assert_eq!(described.len(), 1);
    assert!(described[0].contains("No API key for provider: google"));
    assert_eq!(terminal(&events).stop_reason, StopReason::Error);

    // `stream_simple` uses the same channel — every adapter reports a missing
    // credential in-stream rather than as `Err`.
    let stream = client
        .stream_simple(&model, &context(), &SimpleStreamOptions::default())
        .await
        .expect("stream_simple starts even without a credential");
    let (events, described) = collect_events(stream).await;
    assert_eq!(described.len(), 1);
    assert!(described[0].contains("No API key for provider: google"));
    assert_eq!(terminal(&events).stop_reason, StopReason::Error);
}

#[tokio::test]
async fn an_aborted_request_terminates_with_stop_reason_aborted() {
    let server = mock_stream("text_stream.sse").await;
    let model = gemini_model(&base_url(&server), MODEL_ID);
    let client = GoogleGenerativeAiClient::with_http_client(http_client());

    let (handle, signal) = AbortHandle::new();
    handle.abort();
    let mut options = options();
    options.request.signal = Some(signal);

    let stream = client.stream(&model, &context(), &options).await.unwrap();
    let (events, described) = collect_events(stream).await;

    assert_eq!(described, vec!["error Aborted \"Request aborted\""]);
    assert_eq!(terminal(&events).stop_reason, StopReason::Aborted);
}

#[tokio::test]
async fn sends_the_payload_shape_the_sdk_would_send() {
    let server = mock_stream("text_stream.sse").await;
    let model = gemini_model(&base_url(&server), MODEL_ID);
    let client = GoogleGenerativeAiClient::with_http_client(http_client());

    let context = Context::new(vec![Message::User(UserMessage::text("hello"))])
        .with_system_prompt("be brief")
        .with_tools(vec![Tool::new(
            "bash",
            "run a command",
            serde_json::json!({
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"]
            }),
        )]);
    let mut options = options().with_google_tool_choice(GoogleToolChoice::Auto);
    options.temperature = Some(0.25);
    options.max_tokens = Some(1024);

    let stream = client.stream(&model, &context, &options).await.unwrap();
    collect_events(stream).await;

    assert_eq!(
        recorded_body(&server).await,
        serde_json::json!({
            "contents": [{"role": "user", "parts": [{"text": "hello"}]}],
            "systemInstruction": {"role": "user", "parts": [{"text": "be brief"}]},
            "tools": [{
                "functionDeclarations": [{
                    "name": "bash",
                    "description": "run a command",
                    "parametersJsonSchema": {
                        "type": "object",
                        "properties": {"command": {"type": "string"}},
                        "required": ["command"]
                    }
                }]
            }],
            "toolConfig": {"functionCallingConfig": {"mode": "AUTO"}},
            "generationConfig": {"temperature": 0.25, "maxOutputTokens": 1024}
        })
    );
}

#[tokio::test]
async fn on_payload_can_replace_the_request_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse("text_stream.sse"))
        .mount(&server)
        .await;
    let model = gemini_model(&base_url(&server), MODEL_ID);
    let client = GoogleGenerativeAiClient::with_http_client(http_client());

    let mut options = options();
    options.request.on_payload = Some(std::sync::Arc::new(|_payload, _model| {
        Some(serde_json::json!({"contents": [], "generationConfig": {"seed": 7}}))
    }));

    let stream = client.stream(&model, &context(), &options).await.unwrap();
    collect_events(stream).await;

    assert_eq!(
        recorded_body(&server).await,
        serde_json::json!({"contents": [], "generationConfig": {"seed": 7}})
    );
}

#[tokio::test]
async fn stream_simple_disables_thinking_without_reasoning() {
    let server = mock_stream("text_stream.sse").await;
    let model = gemini_model(&base_url(&server), MODEL_ID);
    let client = GoogleGenerativeAiClient::with_http_client(http_client());

    let stream = client
        .stream_simple(
            &model,
            &context(),
            &SimpleStreamOptions {
                stream: options(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    collect_events(stream).await;

    let body = recorded_body(&server).await;
    assert_eq!(
        body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
        0
    );
    // buildBaseOptions always sends a clamped maxOutputTokens.
    assert!(body["generationConfig"]["maxOutputTokens"].is_number());
}

#[tokio::test]
async fn stream_simple_maps_reasoning_to_a_thinking_budget() {
    let server = mock_stream("text_stream.sse").await;
    let model = gemini_model(&base_url(&server), MODEL_ID);
    let client = GoogleGenerativeAiClient::with_http_client(http_client());

    let stream = client
        .stream_simple(
            &model,
            &context(),
            &SimpleStreamOptions {
                stream: options(),
                reasoning: Some(ThinkingLevel::Medium),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    collect_events(stream).await;

    let thinking = &recorded_body(&server).await["generationConfig"]["thinkingConfig"];
    assert_eq!(thinking["includeThoughts"], true);
    assert_eq!(thinking["thinkingBudget"], 8192);
}

#[tokio::test]
async fn stream_simple_maps_reasoning_to_a_thinking_level_on_gemini_3() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/v1beta/models/gemini-3-pro-preview:streamGenerateContent",
        ))
        .respond_with(sse("text_stream.sse"))
        .mount(&server)
        .await;
    let model = gemini_model(&base_url(&server), "gemini-3-pro-preview");
    let client = GoogleGenerativeAiClient::with_http_client(http_client());

    let stream = client
        .stream_simple(
            &model,
            &context(),
            &SimpleStreamOptions {
                stream: options(),
                reasoning: Some(ThinkingLevel::High),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    collect_events(stream).await;

    let thinking = &recorded_body(&server).await["generationConfig"]["thinkingConfig"];
    assert_eq!(thinking["includeThoughts"], true);
    assert_eq!(thinking["thinkingLevel"], "HIGH");
    assert!(thinking.get("thinkingBudget").is_none());
}
