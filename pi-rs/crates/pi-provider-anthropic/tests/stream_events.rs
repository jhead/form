//! SSE → `AssistantMessageEvent` mapping, usage/cost accounting and error paths.
//!
//! Ports `test/anthropic-sse-parsing.test.ts` and
//! `test/anthropic-cache-write-1h-cost.test.ts`, extended to assert the full
//! event sequence (upstream only inspects the final message).

mod common;

use common::*;
use pretty_assertions::assert_eq;

use pi_core::api::ApiClient;
use pi_core::content::AssistantContent;
use pi_core::event::{AssistantMessageEvent, DoneReason, ErrorReason};
use pi_core::message::StopReason;
use pi_core::options::AbortHandle;

#[tokio::test]
async fn text_stream_emits_the_full_event_sequence() {
    let events = run_fixture("text.sse", &user_context("Say hello."), |_, _| {}).await;

    assert_eq!(
        trace(&events),
        vec![
            "start | []",
            "text_start#0 | [text\"\"]",
            "text_delta#0 \"Hello\" | [text\"Hello\"]",
            "text_delta#0 \" world\" | [text\"Hello world\"]",
            "text_end#0 \"Hello world\" | [text\"Hello world\"]",
            "done Stop | [text\"Hello world\"] stop=Stop usage=12/5",
        ]
    );

    let message = terminal(&events);
    assert_eq!(message.response_id.as_deref(), Some("msg_test"));
    assert_eq!(message.api, "anthropic-messages");
    assert_eq!(message.provider, "anthropic");
    assert_eq!(message.model, "claude-opus-4-8");
    assert_eq!(message.raw_stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(message.usage.total_tokens, 17);
    // 12 input @ $5/Mtok + 5 output @ $25/Mtok.
    assert!((message.usage.cost.total - (12.0 * 5.0 + 5.0 * 25.0) / 1e6).abs() < 1e-12);
}

#[tokio::test]
async fn thinking_redacted_thinking_and_text_interleave() {
    let events = run_fixture("thinking.sse", &user_context("Think."), |_, _| {}).await;

    assert_eq!(
        trace(&events),
        vec![
            "start | []",
            "thinking_start#0 | [thinking\"Initial thinking\"/sig\"initial signature\"]",
            "thinking_delta#0 \" plus delta\" | [thinking\"Initial thinking plus delta\"/sig\"initial signature\"]",
            // signature_delta updates the snapshot without emitting an event
            "thinking_end#0 \"Initial thinking plus delta\" | [thinking\"Initial thinking plus delta\"/sig\"initial signature plus delta\"]",
            "thinking_start#1 | [thinking\"Initial thinking plus delta\"/sig\"initial signature plus delta\", thinking\"[Reasoning redacted]\"/sig\"encrypted-payload\"/redacted]",
            "thinking_end#1 \"[Reasoning redacted]\" | [thinking\"Initial thinking plus delta\"/sig\"initial signature plus delta\", thinking\"[Reasoning redacted]\"/sig\"encrypted-payload\"/redacted]",
            "text_start#2 | [thinking\"Initial thinking plus delta\"/sig\"initial signature plus delta\", thinking\"[Reasoning redacted]\"/sig\"encrypted-payload\"/redacted, text\"Initial text\"]",
            "text_delta#2 \" plus delta\" | [thinking\"Initial thinking plus delta\"/sig\"initial signature plus delta\", thinking\"[Reasoning redacted]\"/sig\"encrypted-payload\"/redacted, text\"Initial text plus delta\"]",
            "text_end#2 \"Initial text plus delta\" | [thinking\"Initial thinking plus delta\"/sig\"initial signature plus delta\", thinking\"[Reasoning redacted]\"/sig\"encrypted-payload\"/redacted, text\"Initial text plus delta\"]",
            "done Stop | [thinking\"Initial thinking plus delta\"/sig\"initial signature plus delta\", thinking\"[Reasoning redacted]\"/sig\"encrypted-payload\"/redacted, text\"Initial text plus delta\"] stop=Stop usage=12/5",
        ]
    );

    let message = terminal(&events);
    // Reasoning tokens ride along on the final message_delta usage.
    assert_eq!(message.usage.reasoning, Some(3));
}

#[tokio::test]
async fn repairs_malformed_streamed_tool_json() {
    let events = run_fixture(
        "tool_use.sse",
        &user_context("Use the edit tool."),
        |_, _| {},
    )
    .await;

    assert_eq!(
        trace(&events)
            .iter()
            .map(|line| line.split(" | ").next().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec![
            "start",
            "toolcall_start#0",
            "toolcall_delta#0 \"{\\\"path\\\":\\\"A\\\\H\\\",\\\"text\\\":\\\"col1\\tcol2\\\"}\"",
            "toolcall_end#0 edit={\"path\":\"A\\\\H\",\"text\":\"col1\\tcol2\"}",
            "done ToolUse",
        ]
    );

    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(message.error_message, None);
    let tool_call = message
        .content
        .iter()
        .find_map(AssistantContent::as_tool_call)
        .expect("tool call");
    assert_eq!(tool_call.id, "toolu_test");
    assert_eq!(tool_call.arguments["path"], "A\\H");
    assert_eq!(tool_call.arguments["text"], "col1\tcol2");
}

#[tokio::test]
async fn tool_arguments_accumulate_across_fragments() {
    let events = run_fixture("tool_use_streaming.sse", &user_context("Read."), |_, _| {}).await;

    assert_eq!(
        trace(&events),
        vec![
            "start | []",
            "toolcall_start#0 | [toolCall:Read{}]",
            // The partial snapshot carries a best-effort parse of the fragment.
            "toolcall_delta#0 \"{\\\"path\\\":\\\"src/\" | [toolCall:Read{\"path\":\"src/\"}]",
            "toolcall_delta#0 \"main.rs\\\"}\" | [toolCall:Read{\"path\":\"src/main.rs\"}]",
            "toolcall_end#0 Read={\"path\":\"src/main.rs\"} | [toolCall:Read{\"path\":\"src/main.rs\"}]",
            "done ToolUse | [toolCall:Read{\"path\":\"src/main.rs\"}] stop=ToolUse usage=12/5",
        ]
    );
}

#[tokio::test]
async fn oauth_tool_calls_come_back_with_the_callers_tool_name() {
    let context = user_context("Read a file")
        .with_tools(vec![pi_core::tool::Tool::no_params("read", "Read a file")]);
    let events = run_fixture("tool_use_streaming.sse", &context, |_, options| {
        options.request.api_key = Some("sk-ant-oat-fake".into());
    })
    .await;

    let tool_call = terminal(&events)
        .content
        .iter()
        .find_map(AssistantContent::as_tool_call)
        .expect("tool call");
    assert_eq!(tool_call.name, "read");
}

#[tokio::test]
async fn preserves_refusal_stop_details() {
    let events = run_fixture("refusal.sse", &user_context("blocked request"), |_, _| {}).await;

    let last = events.last().expect("terminal event");
    assert!(matches!(
        last,
        AssistantMessageEvent::Error {
            reason: ErrorReason::Error,
            ..
        }
    ));
    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("refusal"));
    assert_eq!(
        message.error_message.as_deref(),
        Some("This request was blocked under Anthropic's Usage Policy.")
    );
    // Input usage captured at message_start survives the failure.
    assert_eq!(message.usage.input, 412);
}

#[tokio::test]
async fn preserves_sensitive_stop_reason() {
    let events = run_fixture("sensitive.sse", &user_context("blocked"), |_, _| {}).await;
    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("sensitive"));
    assert_eq!(
        message.error_message.as_deref(),
        Some("Provider stopped with: sensitive")
    );
}

#[tokio::test]
async fn unknown_stop_reasons_fail_the_stream() {
    let events = run_fixture("unknown_stop_reason.sse", &user_context("hi"), |_, _| {}).await;
    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("Unhandled stop reason: time_travel")
    );
}

#[tokio::test]
async fn provider_error_events_terminate_the_stream() {
    let events = run_fixture("error_event.sse", &user_context("hi"), |_, _| {}).await;
    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Error);
    assert!(message
        .error_message
        .as_deref()
        .unwrap_or_default()
        .contains("overloaded_error"));
}

#[tokio::test]
async fn missing_stop_reason_is_an_error() {
    let events = run_fixture("no_stop_reason.sse", &user_context("hi"), |_, _| {}).await;
    assert_eq!(
        terminal(&events).error_message.as_deref(),
        Some("Anthropic stream ended without a stop reason")
    );
}

#[tokio::test]
async fn truncated_stream_is_an_error() {
    let events = run_fixture("truncated.sse", &user_context("hi"), |_, _| {}).await;
    assert_eq!(
        terminal(&events).error_message.as_deref(),
        Some("Anthropic stream ended before message_stop")
    );
}

#[tokio::test]
async fn message_delta_without_usage_keeps_message_start_counts() {
    let events = run_fixture("no_delta_usage.sse", &user_context("hi"), |_, _| {}).await;

    assert_eq!(
        trace(&events),
        vec![
            "start | []",
            "text_start#0 | [text\"\"]",
            "text_delta#0 \"Hello\" | [text\"Hello\"]",
            "text_end#0 \"Hello\" | [text\"Hello\"]",
            "done Stop | [text\"Hello\"] stop=Stop usage=12/0",
        ]
    );
    let message = terminal(&events);
    assert_eq!(message.usage.input, 12);
    assert_eq!(message.usage.total_tokens, 12);
}

#[tokio::test]
async fn prices_one_hour_cache_writes_at_twice_input() {
    let events = run_fixture("cache_write_1h.sse", &user_context("hi"), |_, _| {}).await;
    let usage = &terminal(&events).usage;
    assert_eq!(usage.cache_write, 1_000_000);
    assert_eq!(usage.cache_write_1h, Some(400_000));
    // 600k * 6.25/Mtok + 400k * 10/Mtok = 3.75 + 4.0
    assert!((usage.cost.cache_write - 7.75).abs() < 1e-10);
}

#[tokio::test]
async fn falls_back_to_the_short_cache_rate_without_a_breakdown() {
    let events = run_fixture("cache_write_no_split.sse", &user_context("hi"), |_, _| {}).await;
    let usage = &terminal(&events).usage;
    assert_eq!(usage.cache_write, 1_000_000);
    assert_eq!(usage.cache_write_1h, Some(0));
    assert!((usage.cost.cache_write - 6.25).abs() < 1e-10);
}

#[tokio::test]
async fn applies_tiered_pricing_from_the_model() {
    let events = run_fixture(
        "cache_write_no_split.sse",
        &user_context("hi"),
        |model, _| {
            model.cost.tiers = Some(vec![pi_core::model::ModelCostTier {
                rates: pi_core::model::ModelCostRates {
                    input: 10.0,
                    output: 50.0,
                    cache_read: 1.0,
                    cache_write: 12.5,
                },
                input_tokens_above: 200_000,
            }]);
        },
    )
    .await;
    // 1_000_100 input-side tokens exceeds the tier threshold.
    let usage = &terminal(&events).usage;
    assert!((usage.cost.cache_write - 12.5).abs() < 1e-10);
    assert!((usage.cost.output - 5.0 * 50.0 / 1e6).abs() < 1e-12);
}

#[tokio::test]
async fn http_errors_are_encoded_in_the_stream() {
    let server = status_server(500, r#"{"error":{"message":"upstream exploded"}}"#).await;
    let model = model(&server.uri());
    let stream = api()
        .stream(&model, &user_context("hi"), &options_with_key())
        .await
        .expect("stream starts");
    let events = drain(stream).await;

    // The request never reached `start`, so the error event is the whole stream.
    assert_eq!(events.len(), 1);
    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Error);
    assert!(message
        .error_message
        .as_deref()
        .unwrap_or_default()
        .contains("upstream exploded"));
}

#[tokio::test]
async fn auth_errors_are_encoded_in_the_stream() {
    let server = status_server(401, r#"{"error":{"message":"invalid x-api-key"}}"#).await;
    let model = model(&server.uri());
    let stream = api()
        .stream(&model, &user_context("hi"), &options_with_key())
        .await
        .expect("stream starts");
    let events = drain(stream).await;
    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Error);
    assert!(message
        .error_message
        .as_deref()
        .unwrap_or_default()
        .contains("invalid x-api-key"));
}

/// A missing credential is an in-stream `Error` event from **both** entry
/// points.
///
/// Upstream's `streamSimple` throws synchronously here, and this port used to
/// mirror that. It no longer does: the `ApiClient` contract in `pi-core`
/// reserves `Err` for programmer errors, every other adapter already encoded
/// this condition in the stream, and a Swift caller should not have to handle
/// one failure two ways depending on which entry point it used.
#[tokio::test]
async fn missing_credentials_are_an_in_stream_error_from_both_entry_points() {
    let server = sse_server(fixture("text.sse")).await;
    let model = model(&server.uri());

    let stream = api()
        .stream(&model, &user_context("hi"), &Default::default())
        .await
        .expect("stream starts");
    let events = drain(stream).await;
    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("authentication error: No API key for provider: anthropic")
    );

    let stream = api()
        .stream_simple(&model, &user_context("hi"), &Default::default())
        .await
        .expect("stream_simple starts even without a credential");
    let events = drain(stream).await;
    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("authentication error: No API key for provider: anthropic")
    );
}

#[tokio::test]
async fn aborted_requests_report_the_aborted_stop_reason() {
    let server = sse_server(fixture("text.sse")).await;
    let model = model(&server.uri());
    let (handle, signal) = AbortHandle::new();
    handle.abort();

    let mut options = options_with_key();
    options.request.signal = Some(signal);

    let stream = api()
        .stream(&model, &user_context("hi"), &options)
        .await
        .expect("stream starts");
    let events = drain(stream).await;

    assert!(matches!(
        events.last(),
        Some(AssistantMessageEvent::Error {
            reason: ErrorReason::Aborted,
            ..
        })
    ));
    assert_eq!(terminal(&events).stop_reason, StopReason::Aborted);
}

#[tokio::test]
async fn abort_during_the_request_stops_the_stream() {
    let server = delayed_sse_server(fixture("text.sse"), 2_000).await;
    let model = model(&server.uri());
    let (handle, signal) = AbortHandle::new();
    let mut options = options_with_key();
    options.request.signal = Some(signal);

    let stream = api()
        .stream(&model, &user_context("hi"), &options)
        .await
        .expect("stream starts");
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        handle.abort();
    });
    let events = drain(stream).await;
    assert_eq!(terminal(&events).stop_reason, StopReason::Aborted);
}

#[tokio::test]
async fn done_reason_tracks_the_stop_reason() {
    let events = run_fixture("tool_use_streaming.sse", &user_context("hi"), |_, _| {}).await;
    assert!(matches!(
        events.last(),
        Some(AssistantMessageEvent::Done {
            reason: DoneReason::ToolUse,
            ..
        })
    ));
}
