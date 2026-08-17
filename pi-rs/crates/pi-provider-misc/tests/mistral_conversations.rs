//! Ports of `.upstream/packages/ai/test/mistral-{http-transport,raw-stop-reason,
//! reasoning-mode,tool-schema}.test.ts`, against `wiremock` serving recorded
//! fixture bodies. No test touches a real API.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use pi_core::api::ApiClient;
use pi_core::event::{AssistantMessageEvent, DoneReason, ErrorReason};
use pi_core::message::{AssistantMessage, Message, StopReason, ToolResultMessage};
use pi_core::model::{Api, CacheRetention, Modality, Model, ModelThinkingLevel};
use pi_core::options::{
    AbortHandle, ProviderResponse, RequestOptions, SimpleStreamOptions, StreamOptions,
};
use pi_core::tool::{ConstrainedSampling, ConstrainedSamplingConfig, Context, StrictMode, Tool};
use pi_core::{AssistantContent, InputContent, ThinkingLevel, ToolCall, UserContent, UserMessage};
use pi_provider_misc::mistral_conversations::{option_keys, MistralConversationsApi};
use pretty_assertions::assert_eq;
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// Fixtures and helpers
// ---------------------------------------------------------------------------

fn model(base_url: &str) -> Model {
    let mut model = Model::new(
        "mistral-large-latest",
        Api::MistralConversations,
        "mistral",
        base_url,
    );
    model.name = "Mistral Large".into();
    model.input = vec![Modality::Text, Modality::Image];
    model.cost.rates = pi_core::model::ModelCostRates {
        input: 2.0,
        output: 6.0,
        cache_read: 0.2,
        cache_write: 0.0,
    };
    model
}

async fn sse_server(fixture_name: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header("x-request-id", "request-1")
                .set_body_string(fixture(fixture_name)),
        )
        .mount(&server)
        .await;
    server
}

fn key_options() -> StreamOptions {
    StreamOptions {
        request: RequestOptions {
            api_key: Some("test".into()),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn user_context(text: &str) -> Context {
    Context::new(vec![Message::User(UserMessage {
        content: text.into(),
        timestamp: 1,
    })])
}

async fn wire_payload(server: &MockServer) -> Value {
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    serde_json::from_slice(&requests[0].body).unwrap()
}

async fn request_headers(server: &MockServer) -> BTreeMap<String, String> {
    let requests = server.received_requests().await.unwrap();
    requests[0]
        .headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_lowercase(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Request building
// ---------------------------------------------------------------------------

#[tokio::test]
async fn serializes_sdk_payloads_to_the_mistral_wire_format() {
    let server = sse_server("mistral_stop.sse").await;
    let model = model(&server.uri());

    let context = Context {
        system_prompt: Some("Be precise".into()),
        messages: vec![Message::User(UserMessage {
            content: UserContent::Blocks(vec![
                InputContent::text("describe"),
                InputContent::image("aGVsbG8=", "image/png"),
            ]),
            timestamp: 1,
        })],
        tools: Some(vec![Tool::new(
            "lookup",
            "Look something up",
            json!({ "type": "object", "properties": { "query": { "type": "string" } }, "required": ["query"] }),
        )]),
    };

    let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let seen_response: Arc<Mutex<Option<ProviderResponse>>> = Arc::new(Mutex::new(None));
    let payload_sink = captured.clone();
    let response_sink = seen_response.clone();

    let mut options = key_options();
    options.max_tokens = Some(123);
    options.session_id = Some("session-1".into());
    options
        .request
        .headers
        .insert("x-custom".into(), Some("value".into()));
    options
        .provider_options
        .insert(option_keys::PROMPT_MODE.into(), json!("reasoning"));
    options
        .provider_options
        .insert(option_keys::REASONING_EFFORT.into(), json!("high"));
    options.provider_options.insert(
        option_keys::TOOL_CHOICE.into(),
        json!({ "type": "function", "function": { "name": "lookup" } }),
    );
    options.request.on_payload = Some(Arc::new(move |payload, _model| {
        *payload_sink.lock().unwrap() = Some(payload.clone());
        // Extra camelCase knobs must also be remapped onto the wire.
        let mut replaced = payload.clone();
        replaced["topP"] = json!(0.9);
        replaced["randomSeed"] = json!(42);
        replaced["presencePenalty"] = json!(0.1);
        replaced["frequencyPenalty"] = json!(0.2);
        replaced["parallelToolCalls"] = json!(true);
        replaced["safePrompt"] = json!(true);
        replaced["responseFormat"] = json!({
            "type": "json_schema",
            "jsonSchema": {
                "name": "result",
                "schemaDefinition": { "type": "object", "properties": { "maxTokens": {"type": "number"} } }
            }
        });
        Some(replaced)
    }));
    options.request.on_response = Some(Arc::new(move |response, _model| {
        *response_sink.lock().unwrap() = Some(response.clone());
    }));

    let events = drain(
        MistralConversationsApi::new()
            .stream(&model, &context, &options)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(terminal(&events).stop_reason, StopReason::Stop);

    // The SDK-shaped payload observed by `on_payload` is still camelCase.
    let observed = captured.lock().unwrap().clone().unwrap();
    assert_eq!(observed["maxTokens"], 123);
    assert_eq!(observed["promptMode"], "reasoning");
    assert_eq!(observed["promptCacheKey"], "session-1");
    assert_eq!(observed["stream"], true);

    let response = seen_response.lock().unwrap().clone().unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.headers["x-request-id"], "request-1");

    let headers = request_headers(&server).await;
    assert_eq!(headers["authorization"], "Bearer test");
    assert_eq!(headers["accept"], "text/event-stream");
    assert_eq!(headers["x-affinity"], "session-1");
    assert_eq!(headers["x-custom"], "value");

    let wire = wire_payload(&server).await;
    assert_eq!(wire["max_tokens"], 123);
    assert_eq!(wire["prompt_mode"], "reasoning");
    assert_eq!(wire["reasoning_effort"], "high");
    assert_eq!(wire["prompt_cache_key"], "session-1");
    assert_eq!(
        wire["tool_choice"],
        json!({ "type": "function", "function": { "name": "lookup" } })
    );
    assert_eq!(wire["top_p"], 0.9);
    assert_eq!(wire["random_seed"], 42);
    assert_eq!(wire["presence_penalty"], 0.1);
    assert_eq!(wire["frequency_penalty"], 0.2);
    assert_eq!(wire["parallel_tool_calls"], true);
    assert_eq!(wire["safe_prompt"], true);
    assert_eq!(
        wire["response_format"],
        json!({
            "type": "json_schema",
            "json_schema": {
                "name": "result",
                "schema": { "type": "object", "properties": { "maxTokens": { "type": "number" } } }
            }
        })
    );
    for camel in [
        "maxTokens",
        "promptMode",
        "promptCacheKey",
        "topP",
        "responseFormat",
    ] {
        assert!(wire.get(camel).is_none(), "{camel} leaked to the wire");
    }
    assert_eq!(
        wire["messages"],
        json!([
            { "role": "system", "content": "Be precise" },
            {
                "role": "user",
                "content": [
                    { "type": "text", "text": "describe" },
                    { "type": "image_url", "image_url": "data:image/png;base64,aGVsbG8=" }
                ]
            }
        ])
    );
    assert_eq!(wire["tools"][0]["type"], "function");
    assert_eq!(wire["tools"][0]["function"]["name"], "lookup");
    assert_eq!(wire["tools"][0]["function"]["strict"], false);
}

#[tokio::test]
async fn serializes_assistant_thinking_tool_calls_and_tool_results_for_replay() {
    let server = sse_server("mistral_stop.sse").await;
    let model = model(&server.uri());

    let mut assistant = AssistantMessage::pending("mistral-conversations", "mistral", &model.id);
    assistant.stop_reason = StopReason::ToolUse;
    assistant.timestamp = 1;
    assistant.content = vec![
        AssistantContent::thinking("reason"),
        AssistantContent::text("answer"),
        AssistantContent::ToolCall(ToolCall {
            id: "abc123456".into(),
            name: "lookup".into(),
            arguments: json!({ "query": "pi" }).as_object().unwrap().clone(),
            thought_signature: None,
            namespace: None,
        }),
    ];

    let context = Context::new(vec![
        Message::Assistant(assistant),
        Message::ToolResult(ToolResultMessage {
            tool_call_id: "abc123456".into(),
            tool_name: "lookup".into(),
            content: vec![
                InputContent::text("found"),
                InputContent::image("aGVsbG8=", "image/png"),
            ],
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: false,
            timestamp: 2,
        }),
    ]);

    let events = drain(
        MistralConversationsApi::new()
            .stream(&model, &context, &key_options())
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(terminal(&events).stop_reason, StopReason::Stop);

    let wire = wire_payload(&server).await;
    assert_eq!(
        wire["messages"],
        json!([
            {
                "role": "assistant",
                "prefix": false,
                "content": [
                    { "type": "thinking", "thinking": [{ "type": "text", "text": "reason" }] },
                    { "type": "text", "text": "answer" }
                ],
                "tool_calls": [{
                    "id": "abc123456",
                    "type": "function",
                    "function": { "name": "lookup", "arguments": "{\"query\":\"pi\"}" },
                    "index": 0
                }]
            },
            {
                "role": "tool",
                "tool_call_id": "abc123456",
                "name": "lookup",
                "content": [
                    { "type": "text", "text": "found" },
                    { "type": "image_url", "image_url": "data:image/png;base64,aGVsbG8=" }
                ]
            }
        ])
    );
}

#[tokio::test]
async fn normalizes_foreign_tool_call_ids_and_downgrades_images() {
    let server = sse_server("mistral_stop.sse").await;
    let mut model = model(&server.uri());
    model.input = vec![Modality::Text];

    let mut foreign = AssistantMessage::pending("openai-responses", "openai", "gpt-x");
    foreign.stop_reason = StopReason::ToolUse;
    foreign.content = vec![AssistantContent::ToolCall(ToolCall::new(
        "fc_0123456789abcdef|call",
        "lookup",
    ))];

    let context = Context::new(vec![
        Message::User(UserMessage {
            content: UserContent::Blocks(vec![InputContent::image("aGk=", "image/png")]),
            timestamp: 1,
        }),
        Message::Assistant(foreign),
        Message::ToolResult(ToolResultMessage::text(
            "fc_0123456789abcdef|call",
            "lookup",
            "found",
            false,
        )),
    ]);

    drain(
        MistralConversationsApi::new()
            .stream(&model, &context, &key_options())
            .await
            .unwrap(),
    )
    .await;

    let wire = wire_payload(&server).await;
    let messages = wire["messages"].as_array().unwrap();
    // `transform_messages` swaps the image for a placeholder text block before
    // the wire encoding runs, so the content stays an array.
    assert_eq!(
        messages[0]["content"],
        json!([{ "type": "text", "text": "(image omitted: model does not support images)" }])
    );
    let call_id = messages[1]["tool_calls"][0]["id"].as_str().unwrap();
    assert_eq!(call_id.chars().count(), 9);
    assert!(call_id.chars().all(|c| c.is_ascii_alphanumeric()));
    // The tool result is rewritten to the same normalized id.
    assert_eq!(messages[2]["tool_call_id"], call_id);
}

#[tokio::test]
async fn sends_strict_tool_schemas_when_constrained_sampling_is_requested() {
    let server = sse_server("mistral_stop.sse").await;
    let model = model(&server.uri());

    let context = Context::new(vec![Message::User(UserMessage {
        content: "Hi".into(),
        timestamp: 1,
    })])
    .with_tools(vec![Tool {
        name: "inspect_schema".into(),
        description: "Inspect the schema".into(),
        parameters: json!({
            "type": "object",
            "properties": { "nested": { "type": "object", "properties": { "value": { "type": "string" } }, "required": ["value"] } },
            "required": ["nested"]
        }),
        constrained_sampling: Some(ConstrainedSampling::Config(
            ConstrainedSamplingConfig::JsonSchema {
                strict: StrictMode::Require,
            },
        )),
    }]);

    let events = drain(
        MistralConversationsApi::new()
            .stream(&model, &context, &key_options())
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(terminal(&events).stop_reason, StopReason::Stop);

    let wire = wire_payload(&server).await;
    let function = &wire["tools"][0]["function"];
    assert_eq!(function["strict"], true);
    assert_eq!(function["parameters"]["additionalProperties"], false);
    assert_eq!(function["parameters"]["required"], json!(["nested"]));
    assert_eq!(
        function["parameters"]["properties"]["nested"]["additionalProperties"],
        false
    );
}

#[tokio::test]
async fn a_tool_requiring_unsupported_strict_sampling_errors_in_the_stream() {
    let model = model("http://127.0.0.1:1");
    let context = user_context("hi").with_tools(vec![Tool {
        name: "grammar_only".into(),
        description: "d".into(),
        // `oneOf` cannot be expressed in the strict subset.
        parameters: json!({ "type": "object", "properties": {}, "oneOf": [] }),
        constrained_sampling: Some(ConstrainedSampling::Config(
            ConstrainedSamplingConfig::JsonSchema {
                strict: StrictMode::Require,
            },
        )),
    }]);

    let events = drain(
        MistralConversationsApi::new()
            .stream(&model, &context, &key_options())
            .await
            .unwrap(),
    )
    .await;
    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Error);
    assert!(message
        .error_message
        .as_deref()
        .unwrap()
        .contains("requires JSON-schema constrained sampling"));
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn parses_thinking_text_tool_calls_and_cached_usage() {
    let server = sse_server("mistral_thinking_text_tool_call.sse").await;
    let model = model(&server.uri());

    let events = drain(
        MistralConversationsApi::new()
            .stream(&model, &user_context("hello"), &key_options())
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
            "thinking_end",
            "text_start",
            "text_delta",
            "text_end",
            "toolcall_start",
            "toolcall_delta",
            "toolcall_delta",
            "toolcall_end",
            "done",
        ]
    );
    assert_eq!(content_indices(&events), vec![0, 0, 0, 1, 1, 1, 2, 2, 2, 2]);
    // Mistral reports `finish_reason` on the same chunk as the final tool-call
    // delta, so the last snapshots already carry the terminal stop reason.
    assert_partial_presence(&events);
    assert_eq!(
        deltas(&events),
        vec!["reason", "answer", r#"{"query":"#, r#""pi"}"#]
    );

    // The thinking block is closed before the text block opens.
    let AssistantMessageEvent::ThinkingEnd {
        content, partial, ..
    } = &events[3]
    else {
        unreachable!()
    };
    assert_eq!(content, "reason");
    assert_eq!(partial.content.len(), 1);

    // Tool arguments are parsed incrementally.
    let AssistantMessageEvent::ToolCallDelta { partial, .. } = &events[8] else {
        unreachable!()
    };
    assert!(partial.content[2]
        .as_tool_call()
        .unwrap()
        .arguments
        .is_empty());
    let AssistantMessageEvent::ToolCallDelta { partial, .. } = &events[9] else {
        unreachable!()
    };
    assert_eq!(
        partial.content[2].as_tool_call().unwrap().arguments["query"],
        "pi"
    );

    let AssistantMessageEvent::Done { reason, message } = events.last().unwrap() else {
        unreachable!()
    };
    assert_eq!(*reason, DoneReason::ToolUse);
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("tool_calls"));
    assert_eq!(message.response_id.as_deref(), Some("response-1"));
    assert_eq!(
        message.content,
        vec![
            AssistantContent::thinking("reason"),
            AssistantContent::text("answer"),
            AssistantContent::ToolCall(ToolCall {
                id: "abc123456".into(),
                name: "lookup".into(),
                arguments: json!({ "query": "pi" }).as_object().unwrap().clone(),
                thought_signature: None,
                namespace: None,
            }),
        ]
    );

    // 10 prompt tokens with 3 cached => 7 billed as input.
    assert_eq!(message.usage.input, 7);
    assert_eq!(message.usage.output, 4);
    assert_eq!(message.usage.cache_read, 3);
    assert_eq!(message.usage.cache_write, 0);
    assert_eq!(message.usage.total_tokens, 14);
    // 7 * $2/Mtok + 4 * $6/Mtok + 3 * $0.2/Mtok
    assert!((message.usage.cost.input - 0.000_014).abs() < 1e-12);
    assert!((message.usage.cost.output - 0.000_024).abs() < 1e-12);
    assert!((message.usage.cost.cache_read - 0.000_000_6).abs() < 1e-12);
    assert!((message.usage.cost.total - 0.000_038_6).abs() < 1e-12);
}

#[tokio::test]
async fn parses_plain_string_content_and_multibyte_characters() {
    let server = sse_server("mistral_plain_text.sse").await;
    let model = model(&server.uri());

    let events = drain(
        MistralConversationsApi::new()
            .stream(&model, &user_context("hello"), &key_options())
            .await
            .unwrap(),
    )
    .await;

    assert_eq!(
        kinds(&events),
        vec!["start", "text_start", "text_delta", "text_end", "done"]
    );
    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.content, vec![AssistantContent::text("héllo 🌍")]);
}

#[tokio::test]
async fn preserves_raw_finish_reasons() {
    for (fixture_name, stop_reason, raw, error) in [
        ("mistral_stop.sse", StopReason::Stop, "stop", None),
        (
            "mistral_finish_length.sse",
            StopReason::Length,
            "model_length",
            None,
        ),
        (
            "mistral_finish_error.sse",
            StopReason::Error,
            "error",
            Some("Provider stopped with: error"),
        ),
        (
            "mistral_finish_unmapped.sse",
            StopReason::Error,
            "unmapped_error",
            Some("Provider stopped with: unmapped_error"),
        ),
    ] {
        let server = sse_server(fixture_name).await;
        let model = model(&server.uri());
        let events = drain(
            MistralConversationsApi::new()
                .stream(&model, &user_context("hello"), &key_options())
                .await
                .unwrap(),
        )
        .await;

        let message = terminal(&events);
        assert_eq!(message.stop_reason, stop_reason, "{fixture_name}");
        assert_eq!(
            message.raw_stop_reason.as_deref(),
            Some(raw),
            "{fixture_name}"
        );
        assert_eq!(message.error_message.as_deref(), error, "{fixture_name}");
        if error.is_some() {
            assert_eq!(kinds(&events).last(), Some(&"error"), "{fixture_name}");
        } else {
            assert_eq!(kinds(&events).last(), Some(&"done"), "{fixture_name}");
        }
    }
}

#[tokio::test]
async fn errors_when_the_stream_ends_without_a_finish_reason() {
    let server = sse_server("mistral_no_finish_reason.sse").await;
    let model = model(&server.uri());

    let events = drain(
        MistralConversationsApi::new()
            .stream(&model, &user_context("hello"), &key_options())
            .await
            .unwrap(),
    )
    .await;

    let AssistantMessageEvent::Error { reason, error } = events.last().unwrap() else {
        panic!("expected a terminal error, got {:?}", kinds(&events));
    };
    assert_eq!(*reason, ErrorReason::Error);
    assert_eq!(
        error.error_message.as_deref(),
        Some("Mistral stream ended without a finish reason")
    );
    // Partial content produced before the failure survives on the error message.
    assert_eq!(error.text(), "partial");
}

// ---------------------------------------------------------------------------
// Errors, aborts, timeouts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn preserves_http_status_and_response_bodies_in_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("content-type", "application/json")
                .set_body_string(fixture("mistral_forbidden.json")),
        )
        .mount(&server)
        .await;
    let model = model(&server.uri());

    let events = drain(
        MistralConversationsApi::new()
            .stream(&model, &user_context("hello"), &key_options())
            .await
            .unwrap(),
    )
    .await;

    assert_eq!(kinds(&events), vec!["error"]);
    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some(r#"Mistral API error (403): {"message":"blocked by gateway"}"#)
    );
}

#[tokio::test]
async fn errors_when_no_api_key_is_provided() {
    let model = model("http://127.0.0.1:1");
    let events = drain(
        MistralConversationsApi::new()
            .stream(&model, &user_context("hello"), &Default::default())
            .await
            .unwrap(),
    )
    .await;

    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("No API key for provider: mistral")
    );

    // `stream_simple` uses the same channel — every adapter reports a missing
    // credential in-stream rather than as `Err`.
    let events = drain(
        MistralConversationsApi::new()
            .stream_simple(&model, &user_context("hello"), &Default::default())
            .await
            .expect("stream_simple starts even without a credential"),
    )
    .await;
    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("No API key for provider: mistral")
    );
}

#[tokio::test]
async fn aborts_while_waiting_for_the_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(fixture("mistral_stop.sse"))
                .set_delay(Duration::from_secs(30)),
        )
        .mount(&server)
        .await;
    let model = model(&server.uri());

    let (handle, signal) = AbortHandle::new();
    let mut options = key_options();
    options.request.signal = Some(signal);

    let stream = MistralConversationsApi::new()
        .stream(&model, &user_context("hello"), &options)
        .await
        .unwrap();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        handle.abort();
    });

    let events = drain(stream).await;
    let AssistantMessageEvent::Error { reason, error } = events.last().unwrap() else {
        panic!("expected a terminal error")
    };
    assert_eq!(*reason, ErrorReason::Aborted);
    assert_eq!(error.stop_reason, StopReason::Aborted);
}

#[tokio::test]
async fn applies_the_request_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(fixture("mistral_stop.sse"))
                .set_delay(Duration::from_secs(30)),
        )
        .mount(&server)
        .await;
    let model = model(&server.uri());

    let mut options = key_options();
    options.request.timeout_ms = Some(30);

    let events = drain(
        MistralConversationsApi::new()
            .stream(&model, &user_context("hello"), &options)
            .await
            .unwrap(),
    )
    .await;

    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Error);
    assert!(
        message
            .error_message
            .as_deref()
            .unwrap()
            .to_lowercase()
            .contains("timed out"),
        "{:?}",
        message.error_message
    );
}

// ---------------------------------------------------------------------------
// Reasoning mode selection (`stream_simple`)
// ---------------------------------------------------------------------------

async fn capture_simple_payload(
    model_id: &str,
    reasoning: Option<ThinkingLevel>,
    mutate: impl FnOnce(&mut SimpleStreamOptions),
) -> Value {
    let server = sse_server("mistral_stop.sse").await;
    let mut model = model(&server.uri());
    model.id = model_id.to_string();
    model.reasoning = true;

    let mut options = SimpleStreamOptions {
        stream: key_options(),
        reasoning,
        ..Default::default()
    };
    mutate(&mut options);

    drain(
        MistralConversationsApi::new()
            .stream_simple(&model, &user_context("Hello"), &options)
            .await
            .unwrap(),
    )
    .await;

    wire_payload(&server).await
}

#[tokio::test]
async fn uses_reasoning_effort_for_models_that_take_it() {
    for model_id in [
        "mistral-small-2603",
        "mistral-small-latest",
        "mistral-medium-3.5",
    ] {
        let wire = capture_simple_payload(model_id, Some(ThinkingLevel::Medium), |_| {}).await;
        assert_eq!(wire["reasoning_effort"], "high", "{model_id}");
        assert!(wire.get("prompt_mode").is_none(), "{model_id}");
    }
}

#[tokio::test]
async fn omits_reasoning_controls_when_thinking_is_off() {
    let wire = capture_simple_payload("mistral-small-2603", None, |_| {}).await;
    assert!(wire.get("reasoning_effort").is_none());
    assert!(wire.get("prompt_mode").is_none());
}

#[tokio::test]
async fn uses_prompt_mode_for_magistral_reasoning_models() {
    let wire = capture_simple_payload(
        "magistral-medium-latest",
        Some(ThinkingLevel::Medium),
        |_| {},
    )
    .await;
    assert_eq!(wire["prompt_mode"], "reasoning");
    assert!(wire.get("reasoning_effort").is_none());
}

#[tokio::test]
async fn honours_an_explicit_thinking_level_map() {
    let server = sse_server("mistral_stop.sse").await;
    let mut model = model(&server.uri());
    model.id = "mistral-medium-3.5".into();
    model.reasoning = true;
    model.thinking_level_map = Some(
        [(ModelThinkingLevel::Low, Some("none".to_string()))]
            .into_iter()
            .collect(),
    );

    let options = SimpleStreamOptions {
        stream: key_options(),
        reasoning: Some(ThinkingLevel::Low),
        ..Default::default()
    };
    drain(
        MistralConversationsApi::new()
            .stream_simple(&model, &user_context("Hello"), &options)
            .await
            .unwrap(),
    )
    .await;

    assert_eq!(wire_payload(&server).await["reasoning_effort"], "none");
}

#[tokio::test]
async fn uses_the_session_id_as_the_prompt_cache_key() {
    let wire = capture_simple_payload("mistral-large-latest", None, |options| {
        options.stream.session_id = Some("session-123".into());
    })
    .await;
    assert_eq!(wire["prompt_cache_key"], "session-123");
}

#[tokio::test]
async fn omits_the_prompt_cache_key_when_retention_is_disabled() {
    let wire = capture_simple_payload("mistral-large-latest", None, |options| {
        options.stream.session_id = Some("session-123".into());
        options.stream.cache_retention = Some(CacheRetention::None);
    })
    .await;
    assert!(wire.get("prompt_cache_key").is_none());
}

#[tokio::test]
async fn clamps_max_tokens_to_the_context_window() {
    let server = sse_server("mistral_stop.sse").await;
    let mut model = model(&server.uri());
    model.context_window = 5000;
    model.max_tokens = 100_000;

    drain(
        MistralConversationsApi::new()
            .stream_simple(
                &model,
                &user_context("Hello"),
                &SimpleStreamOptions {
                    stream: key_options(),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
    )
    .await;

    // 5000 window - 4096 safety - 2 estimated prompt tokens ("Hello").
    assert_eq!(wire_payload(&server).await["max_tokens"], 902);
}

// ---------------------------------------------------------------------------
// Headers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn honours_case_insensitive_header_overrides_and_affinity_suppression() {
    let server = sse_server("mistral_stop.sse").await;
    let mut model = model(&server.uri());
    model.headers = Some(
        [
            ("Authorization".to_string(), "Bearer model-key".to_string()),
            ("X-Affinity".to_string(), "model-affinity".to_string()),
        ]
        .into_iter()
        .collect(),
    );

    let mut options = key_options();
    options.request.api_key = Some("request-key".into());
    options.session_id = Some("automatic-affinity".into());
    options.request.headers.insert("authorization".into(), None);
    options.request.headers.insert("x-affinity".into(), None);

    drain(
        MistralConversationsApi::new()
            .stream(&model, &user_context("hello"), &options)
            .await
            .unwrap(),
    )
    .await;

    let headers = request_headers(&server).await;
    assert!(!headers.contains_key("authorization"));
    assert!(!headers.contains_key("x-affinity"));
}

#[tokio::test]
async fn model_headers_win_over_provider_defaults() {
    let server = sse_server("mistral_stop.sse").await;
    let mut model = model(&server.uri());
    model.headers = Some(
        [("X-Affinity".to_string(), "model-affinity".to_string())]
            .into_iter()
            .collect(),
    );

    let mut options = key_options();
    options.session_id = Some("session-1".into());

    drain(
        MistralConversationsApi::new()
            .stream(&model, &user_context("hello"), &options)
            .await
            .unwrap(),
    )
    .await;

    // An explicit affinity header suppresses the session-derived one.
    assert_eq!(
        request_headers(&server).await["x-affinity"],
        "model-affinity"
    );
}

#[test]
fn exposes_a_registrable_provider_descriptor() {
    let registration = pi_provider_misc::mistral_provider();
    assert_eq!(registration.descriptor.id, "mistral");
    assert_eq!(registration.descriptor.name, "Mistral");
    assert_eq!(registration.descriptor.api, "mistral-conversations");
    assert_eq!(
        registration.descriptor.base_url.as_deref(),
        Some("https://api.mistral.ai")
    );
    assert_eq!(registration.descriptor.api_key_env, vec!["MISTRAL_API_KEY"]);
    assert_eq!(registration.client.api(), "mistral-conversations");
}
