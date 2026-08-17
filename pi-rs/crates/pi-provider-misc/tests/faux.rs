//! Port of `.upstream/packages/ai/test/faux-provider.test.ts`, extended with
//! full event-sequence and `partial`-snapshot assertions.

use pi_core::api::ApiClient;
use pi_core::content::AssistantContent;
use pi_core::event::{AssistantMessageEvent, AssistantMessageEventStream, DoneReason, ErrorReason};
use pi_core::message::{AssistantMessage, Message, StopReason};
use pi_core::model::{CacheRetention, Modality};
use pi_core::options::{AbortHandle, Deferred, SimpleStreamOptions};
use pi_core::tool::{Context, Tool};
use pi_core::{InputContent, UserContent, UserMessage};
use pi_provider_misc::faux::{
    faux_assistant_message, faux_text, faux_thinking, faux_tool_call, faux_tool_call_with_id,
    serialize_context, FauxModelDefinition, FauxOptions, FauxProvider, FauxResponse, FauxTokenSize,
};
use pretty_assertions::assert_eq;
use serde_json::json;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn drain(mut stream: AssistantMessageEventStream) -> Vec<AssistantMessageEvent> {
    let mut events = Vec::new();
    while let Some(event) = futures_util::StreamExt::next(&mut stream).await {
        events.push(event);
    }
    events
}

fn kinds(events: &[AssistantMessageEvent]) -> Vec<&'static str> {
    events.iter().map(kind).collect()
}

fn kind(event: &AssistantMessageEvent) -> &'static str {
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

fn terminal(events: &[AssistantMessageEvent]) -> AssistantMessage {
    events
        .last()
        .and_then(AssistantMessageEvent::terminal_message)
        .cloned()
        .expect("stream ended with a terminal event")
}

fn user_context(text: &str) -> Context {
    Context::new(vec![Message::User(UserMessage::text(text))])
}

async fn complete(faux: &FauxProvider, context: &Context) -> AssistantMessage {
    complete_with(faux, context, SimpleStreamOptions::default()).await
}

async fn complete_with(
    faux: &FauxProvider,
    context: &Context,
    options: SimpleStreamOptions,
) -> AssistantMessage {
    let model = faux.model();
    let stream = faux.stream_simple(&model, context, &options).await.unwrap();
    terminal(&drain(stream).await)
}

// ---------------------------------------------------------------------------
// Basics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn replays_a_scripted_response_and_estimates_usage() {
    let faux = FauxProvider::new();
    faux.set_responses(vec![faux_assistant_message("hello world").into()]);

    let context = user_context("hi there").with_system_prompt("Be concise.");
    let response = complete(&faux, &context).await;

    assert_eq!(response.content, vec![faux_text("hello world")]);
    assert!(response.usage.input > 0);
    assert!(response.usage.output > 0);
    assert_eq!(
        response.usage.total_tokens,
        response.usage.input + response.usage.output
    );
    assert_eq!(faux.state().call_count, 1);
}

#[tokio::test]
async fn supports_text_thinking_and_tool_call_blocks() {
    let faux = FauxProvider::new();
    let mut message = faux_assistant_message(vec![
        faux_thinking("think"),
        faux_tool_call("echo", json!({ "text": "hi" })),
        faux_text("done"),
    ]);
    message.stop_reason = StopReason::ToolUse;
    faux.set_responses(vec![message.into()]);

    let response = complete(&faux, &user_context("hi")).await;

    assert_eq!(response.content.len(), 3);
    assert_eq!(response.content[0].as_thinking().unwrap().thinking, "think");
    let call = response.content[1].as_tool_call().unwrap();
    assert_eq!(call.name, "echo");
    assert_eq!(call.arguments["text"], "hi");
    assert!(!call.id.is_empty());
    assert_eq!(response.content[2].as_text().unwrap().text, "done");
    assert_eq!(response.stop_reason, StopReason::ToolUse);
}

#[tokio::test]
async fn serves_multiple_models_with_model_aware_factories() {
    let faux = FauxProvider::with_options(FauxOptions::default().models(vec![
        FauxModelDefinition::new("faux-fast").named("Faux Fast"),
        FauxModelDefinition::new("faux-thinker")
            .named("Faux Thinker")
            .reasoning(true),
    ]));
    let factory = |request: pi_provider_misc::FauxRequest| {
        Ok(faux_assistant_message(format!(
            "{}:{}",
            request.model.id, request.model.reasoning
        )))
    };
    faux.set_responses(vec![
        FauxResponse::from_fn(factory),
        FauxResponse::from_fn(factory),
    ]);

    assert_eq!(
        faux.models()
            .iter()
            .map(|m| m.id.clone())
            .collect::<Vec<_>>(),
        vec!["faux-fast", "faux-thinker"]
    );
    assert_eq!(faux.model().id, "faux-fast");
    assert!(!faux.model_by_id("faux-fast").unwrap().reasoning);
    assert!(faux.model_by_id("faux-thinker").unwrap().reasoning);
    assert!(faux.model_by_id("nope").is_none());

    let context = user_context("hi");
    let options = SimpleStreamOptions::default();
    let fast = faux
        .stream_simple(&faux.model_by_id("faux-fast").unwrap(), &context, &options)
        .await
        .unwrap();
    assert_eq!(terminal(&drain(fast).await).text(), "faux-fast:false");

    let thinker = faux
        .stream_simple(
            &faux.model_by_id("faux-thinker").unwrap(),
            &context,
            &options,
        )
        .await
        .unwrap();
    assert_eq!(terminal(&drain(thinker).await).text(), "faux-thinker:true");
}

#[tokio::test]
async fn rewrites_api_provider_and_model_on_responses() {
    let faux = FauxProvider::with_options(
        FauxOptions::default()
            .api("faux:test")
            .provider("faux-provider")
            .models(vec![FauxModelDefinition::new("faux-model")]),
    );
    faux.set_responses(vec![faux_assistant_message("hello").into()]);

    let response = complete(&faux, &user_context("hi")).await;

    assert_eq!(response.api, "faux:test");
    assert_eq!(response.provider, "faux-provider");
    assert_eq!(response.model, "faux-model");
    assert_eq!(faux.api(), "faux:test");
    assert_eq!(faux.model().api.as_str(), "faux:test");
}

#[tokio::test]
async fn consumes_queued_responses_in_order_then_errors() {
    let faux = FauxProvider::new();
    faux.set_responses(vec![
        FauxResponse::text("first"),
        FauxResponse::text("second"),
    ]);
    let context = user_context("hi");

    assert_eq!(complete(&faux, &context).await.text(), "first");
    assert_eq!(complete(&faux, &context).await.text(), "second");

    let exhausted = complete(&faux, &context).await;
    assert_eq!(exhausted.stop_reason, StopReason::Error);
    assert_eq!(
        exhausted.error_message.as_deref(),
        Some("No more faux responses queued")
    );
    // Usage is still estimated for the exhaustion error, like upstream.
    assert!(exhausted.usage.input > 0);
    assert_eq!(faux.pending_response_count(), 0);
    assert_eq!(faux.state().call_count, 3);
}

#[tokio::test]
async fn can_replace_and_append_queued_responses() {
    let faux = FauxProvider::new();
    faux.set_responses(vec![FauxResponse::text("first")]);
    let context = user_context("hi");

    assert_eq!(complete(&faux, &context).await.text(), "first");
    assert_eq!(faux.pending_response_count(), 0);

    faux.set_responses(vec![FauxResponse::text("second")]);
    assert_eq!(faux.pending_response_count(), 1);
    assert_eq!(complete(&faux, &context).await.text(), "second");

    faux.append_responses(vec![
        FauxResponse::text("third"),
        FauxResponse::text("fourth"),
    ]);
    faux.push_response(faux_assistant_message("fifth"));
    assert_eq!(faux.pending_response_count(), 3);
    assert_eq!(complete(&faux, &context).await.text(), "third");
    assert_eq!(complete(&faux, &context).await.text(), "fourth");
    assert_eq!(complete(&faux, &context).await.text(), "fifth");
    assert_eq!(faux.pending_response_count(), 0);
}

#[tokio::test]
async fn supports_async_response_factories() {
    let faux = FauxProvider::new();
    faux.set_responses(vec![FauxResponse::from_async_fn(|request| async move {
        Ok(faux_assistant_message(format!(
            "{}:{}",
            request.context.messages.len(),
            request.state.call_count
        )))
    })]);

    assert_eq!(complete(&faux, &user_context("hi")).await.text(), "1:1");
}

#[tokio::test]
async fn a_failing_factory_terminates_the_stream_with_an_error() {
    let faux = FauxProvider::new();
    faux.set_responses(vec![FauxResponse::failure("boom")]);

    let model = faux.model();
    let events = drain(
        faux.stream_simple(&model, &user_context("hi"), &Default::default())
            .await
            .unwrap(),
    )
    .await;

    assert_eq!(kinds(&events), vec!["error"]);
    let AssistantMessageEvent::Error { reason, error } = &events[0] else {
        panic!("expected an error event");
    };
    assert_eq!(*reason, ErrorReason::Error);
    assert_eq!(error.stop_reason, StopReason::Error);
    assert_eq!(error.error_message.as_deref(), Some("boom"));
}

#[tokio::test]
async fn rejects_a_response_without_a_terminal_stop_reason() {
    let faux = FauxProvider::new();
    let mut message = faux_assistant_message("partial");
    message.stop_reason = StopReason::Pending;
    faux.set_responses(vec![message.into()]);

    let model = faux.model();
    let events = drain(
        faux.stream_simple(&model, &user_context("hi"), &Default::default())
            .await
            .unwrap(),
    )
    .await;

    assert!(!kinds(&events).contains(&"done"));
    let last = events.last().unwrap();
    assert_eq!(kind(last), "error");
    let AssistantMessageEvent::Error { error, .. } = last else {
        unreachable!()
    };
    assert_eq!(error.stop_reason, StopReason::Error);
    assert_eq!(
        error.error_message.as_deref(),
        Some("Faux response ended without a stop reason")
    );
}

// ---------------------------------------------------------------------------
// Usage simulation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn estimates_prompt_and_output_tokens_from_the_serialized_context() {
    let faux = FauxProvider::new();
    faux.set_responses(vec![FauxResponse::text("done")]);

    let tool = Tool::new(
        "echo",
        "Echo back text",
        json!({ "type": "object", "properties": { "text": { "type": "string" } } }),
    );
    let mut prior = faux_assistant_message("prior");
    prior.timestamp = 1;
    let context = Context {
        system_prompt: Some("sys".into()),
        messages: vec![
            Message::User(UserMessage {
                content: UserContent::Blocks(vec![
                    InputContent::text("hello"),
                    InputContent::image("abcd", "image/png"),
                ]),
                timestamp: 1,
            }),
            Message::Assistant(prior),
            Message::ToolResult(pi_core::ToolResultMessage {
                tool_call_id: "tool-1".into(),
                tool_name: "echo".into(),
                content: vec![InputContent::text("tool out")],
                details: None,
                usage: None,
                added_tool_names: None,
                is_error: false,
                timestamp: 2,
            }),
        ],
        tools: Some(vec![tool.clone()]),
    };

    let expected_prompt = [
        "system:sys".to_string(),
        "user:hello\n[image:image/png:4]".to_string(),
        "assistant:prior".to_string(),
        "toolResult:echo\ntool out".to_string(),
        format!("tools:{}", serde_json::to_string(&[tool]).unwrap()),
    ]
    .join("\n\n");
    assert_eq!(serialize_context(&context), expected_prompt);

    let expected_prompt_tokens = expected_prompt.chars().count().div_ceil(4) as i64;
    let expected_output_tokens = 1; // "done"

    let response = complete(&faux, &context).await;
    assert_eq!(response.usage.input, expected_prompt_tokens);
    assert_eq!(response.usage.output, expected_output_tokens);
    assert_eq!(response.usage.cache_read, 0);
    assert_eq!(response.usage.cache_write, 0);
    assert_eq!(
        response.usage.total_tokens,
        expected_prompt_tokens + expected_output_tokens
    );
}

#[tokio::test]
async fn simulates_prompt_caching_per_session() {
    let faux = FauxProvider::new();
    faux.set_responses(vec![
        FauxResponse::text("first"),
        FauxResponse::text("second"),
    ]);

    let mut context = user_context("hello").with_system_prompt("Be concise.");
    let options = |session: &str| SimpleStreamOptions {
        stream: pi_core::options::StreamOptions {
            session_id: Some(session.to_string()),
            cache_retention: Some(CacheRetention::Short),
            ..Default::default()
        },
        ..Default::default()
    };

    let first = complete_with(&faux, &context, options("session-1")).await;
    assert_eq!(first.usage.cache_read, 0);
    assert!(first.usage.cache_write > 0);

    context.messages.push(Message::Assistant(first));
    context
        .messages
        .push(Message::User(UserMessage::text("follow up")));

    let second = complete_with(&faux, &context, options("session-1")).await;
    assert!(second.usage.cache_read > 0);
}

#[tokio::test]
async fn does_not_share_the_cache_across_sessions_or_unkeyed_requests() {
    let faux = FauxProvider::new();
    faux.set_responses(vec![
        FauxResponse::text("first"),
        FauxResponse::text("second"),
        FauxResponse::text("third"),
    ]);

    let mut context = user_context("hello");
    let session = |session: &str| SimpleStreamOptions {
        stream: pi_core::options::StreamOptions {
            session_id: Some(session.to_string()),
            cache_retention: Some(CacheRetention::Short),
            ..Default::default()
        },
        ..Default::default()
    };

    let first = complete_with(&faux, &context, session("session-1")).await;
    assert!(first.usage.cache_write > 0);
    context.messages.push(Message::Assistant(first));
    context
        .messages
        .push(Message::User(UserMessage::text("follow up")));

    let second = complete_with(&faux, &context, session("session-2")).await;
    assert_eq!(second.usage.cache_read, 0);
    assert!(second.usage.cache_write > 0);

    let third = complete(&faux, &context).await;
    assert_eq!(third.usage.cache_read, 0);
    assert_eq!(third.usage.cache_write, 0);
}

#[tokio::test]
async fn skips_caching_when_retention_is_none() {
    let faux = FauxProvider::new();
    faux.set_responses(vec![
        FauxResponse::text("first"),
        FauxResponse::text("second"),
    ]);

    let mut context = user_context("hello");
    let options = SimpleStreamOptions {
        stream: pi_core::options::StreamOptions {
            session_id: Some("session-1".into()),
            cache_retention: Some(CacheRetention::None),
            ..Default::default()
        },
        ..Default::default()
    };

    complete_with(&faux, &context, options.clone()).await;
    context
        .messages
        .push(Message::Assistant(faux_assistant_message("first")));
    context
        .messages
        .push(Message::User(UserMessage::text("follow up")));

    let second = complete_with(&faux, &context, options).await;
    assert_eq!(second.usage.cache_read, 0);
    assert_eq!(second.usage.cache_write, 0);
}

// ---------------------------------------------------------------------------
// Event sequences
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streams_an_exact_event_sequence_with_running_partials() {
    let faux =
        FauxProvider::with_options(FauxOptions::default().token_size(FauxTokenSize::fixed(1)));
    let mut message = faux_assistant_message(vec![
        faux_thinking("go"),
        faux_text("ok"),
        faux_tool_call_with_id("tool-1", "echo", json!({})),
    ]);
    message.stop_reason = StopReason::ToolUse;
    faux.set_responses(vec![message.into()]);

    let model = faux.model();
    let events = drain(
        faux.stream_simple(&model, &user_context("hi"), &Default::default())
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
            "toolcall_end",
            "done",
        ]
    );

    // Start carries an empty pending snapshot.
    let AssistantMessageEvent::Start { partial } = &events[0] else {
        unreachable!()
    };
    assert_eq!(partial.stop_reason, StopReason::Pending);
    assert!(partial.content.is_empty());

    // Content indices track the block position.
    assert_eq!(content_indices(&events), vec![0, 0, 0, 1, 1, 1, 2, 2, 2]);

    // The thinking block accumulates and is empty before its first delta.
    let AssistantMessageEvent::ThinkingStart { partial, .. } = &events[1] else {
        unreachable!()
    };
    assert_eq!(partial.content[0].as_thinking().unwrap().thinking, "");
    let AssistantMessageEvent::ThinkingDelta { delta, partial, .. } = &events[2] else {
        unreachable!()
    };
    assert_eq!(delta, "go");
    assert_eq!(partial.content[0].as_thinking().unwrap().thinking, "go");
    let AssistantMessageEvent::ThinkingEnd { content, .. } = &events[3] else {
        unreachable!()
    };
    assert_eq!(content, "go");

    // Text follows in its own block, and the partial keeps the earlier one.
    let AssistantMessageEvent::TextDelta { delta, partial, .. } = &events[5] else {
        unreachable!()
    };
    assert_eq!(delta, "ok");
    assert_eq!(partial.content.len(), 2);
    assert_eq!(partial.content[1].as_text().unwrap().text, "ok");

    // Tool arguments only materialize on `toolcall_end`, matching upstream.
    let AssistantMessageEvent::ToolCallStart { partial, .. } = &events[7] else {
        unreachable!()
    };
    assert_eq!(partial.content[2].as_tool_call().unwrap().id, "tool-1");
    assert!(partial.content[2]
        .as_tool_call()
        .unwrap()
        .arguments
        .is_empty());
    let AssistantMessageEvent::ToolCallDelta { delta, .. } = &events[8] else {
        unreachable!()
    };
    assert_eq!(delta, "{}");
    let AssistantMessageEvent::ToolCallEnd {
        tool_call, partial, ..
    } = &events[9]
    else {
        unreachable!()
    };
    assert_eq!(tool_call.id, "tool-1");
    assert_eq!(partial.content.len(), 3);

    let AssistantMessageEvent::Done { reason, message } = &events[10] else {
        unreachable!()
    };
    assert_eq!(*reason, DoneReason::ToolUse);
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(message.content.len(), 3);
}

fn content_indices(events: &[AssistantMessageEvent]) -> Vec<usize> {
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

#[tokio::test]
async fn splits_long_content_into_multiple_deltas() {
    let faux = FauxProvider::new();
    let mut message = faux_assistant_message(vec![
        faux_thinking("thinking text"),
        faux_text("answer text"),
        faux_tool_call_with_id("tool-1", "echo", json!({ "text": "hi", "count": 12 })),
    ]);
    message.stop_reason = StopReason::ToolUse;
    faux.set_responses(vec![message.into()]);

    let model = faux.model();
    let events = drain(
        faux.stream_simple(&model, &user_context("hi"), &Default::default())
            .await
            .unwrap(),
    )
    .await;

    let tool_deltas: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            AssistantMessageEvent::ToolCallDelta { delta, .. } => Some(delta.clone()),
            _ => None,
        })
        .collect();
    assert!(tool_deltas.len() > 1, "expected chunked tool arguments");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&tool_deltas.join("")).unwrap(),
        json!({ "text": "hi", "count": 12 })
    );

    let names = kinds(&events);
    for expected in [
        "thinking_start",
        "thinking_delta",
        "text_start",
        "text_delta",
        "toolcall_start",
        "toolcall_delta",
        "toolcall_end",
    ] {
        assert!(names.contains(&expected), "missing {expected}");
    }
}

#[tokio::test]
async fn streams_multiple_tool_calls_in_one_message() {
    let faux = FauxProvider::new();
    let mut message = faux_assistant_message(vec![
        faux_tool_call_with_id("tool-1", "echo", json!({ "text": "one" })),
        faux_tool_call_with_id("tool-2", "echo", json!({ "text": "two" })),
    ]);
    message.stop_reason = StopReason::ToolUse;
    faux.set_responses(vec![message.into()]);

    let model = faux.model();
    let events = drain(
        faux.stream_simple(&model, &user_context("hi"), &Default::default())
            .await
            .unwrap(),
    )
    .await;

    assert_eq!(
        kinds(&events)
            .iter()
            .filter(|k| **k == "toolcall_start")
            .count(),
        2
    );
    assert_eq!(
        kinds(&events)
            .iter()
            .filter(|k| **k == "toolcall_end")
            .count(),
        2
    );
    let message = terminal(&events);
    let ids: Vec<&str> = message.tool_calls().map(|call| call.id.as_str()).collect();
    assert_eq!(ids, vec!["tool-1", "tool-2"]);
}

#[tokio::test]
async fn streams_an_explicit_error_message_as_a_terminal_error() {
    let faux =
        FauxProvider::with_options(FauxOptions::default().token_size(FauxTokenSize::fixed(2)));
    let mut message = faux_assistant_message("partial");
    message.stop_reason = StopReason::Error;
    message.error_message = Some("upstream failed".into());
    faux.set_responses(vec![message.into()]);

    let model = faux.model();
    let events = drain(
        faux.stream_simple(&model, &user_context("hi"), &Default::default())
            .await
            .unwrap(),
    )
    .await;

    assert_eq!(
        kinds(&events),
        vec!["start", "text_start", "text_delta", "text_end", "error"]
    );
    let AssistantMessageEvent::Error { reason, error } = events.last().unwrap() else {
        unreachable!()
    };
    assert_eq!(*reason, ErrorReason::Error);
    assert_eq!(error.stop_reason, StopReason::Error);
    assert_eq!(error.error_message.as_deref(), Some("upstream failed"));
    // The partial content produced before the failure survives.
    assert_eq!(error.text(), "partial");
}

#[tokio::test]
async fn streams_an_explicit_aborted_message_as_a_terminal_error() {
    let faux =
        FauxProvider::with_options(FauxOptions::default().token_size(FauxTokenSize::fixed(2)));
    let mut message = faux_assistant_message("partial");
    message.stop_reason = StopReason::Aborted;
    message.error_message = Some("Request was aborted".into());
    faux.set_responses(vec![message.into()]);

    let model = faux.model();
    let events = drain(
        faux.stream_simple(&model, &user_context("hi"), &Default::default())
            .await
            .unwrap(),
    )
    .await;

    assert_eq!(
        kinds(&events),
        vec!["start", "text_start", "text_delta", "text_end", "error"]
    );
    let AssistantMessageEvent::Error { reason, error } = events.last().unwrap() else {
        unreachable!()
    };
    assert_eq!(*reason, ErrorReason::Aborted);
    assert_eq!(error.stop_reason, StopReason::Aborted);
}

// ---------------------------------------------------------------------------
// Aborts
// ---------------------------------------------------------------------------

fn paced() -> FauxProvider {
    FauxProvider::with_options(
        FauxOptions::default()
            .tokens_per_second(100.0)
            .token_size(FauxTokenSize::fixed(3)),
    )
}

#[tokio::test]
async fn aborting_before_the_first_chunk_emits_only_an_error() {
    let faux = paced();
    faux.set_responses(vec![FauxResponse::text("abcdefghijklmnopqrstuvwxyz")]);

    let (handle, signal) = AbortHandle::new();
    handle.abort();
    let mut options = SimpleStreamOptions::default();
    options.stream.request.signal = Some(signal);

    let model = faux.model();
    let events = drain(
        faux.stream_simple(&model, &user_context("hi"), &options)
            .await
            .unwrap(),
    )
    .await;

    assert_eq!(kinds(&events), vec!["error"]);
    let AssistantMessageEvent::Error { reason, error } = &events[0] else {
        unreachable!()
    };
    assert_eq!(*reason, ErrorReason::Aborted);
    assert_eq!(error.stop_reason, StopReason::Aborted);
    assert_eq!(error.error_message.as_deref(), Some("Request was aborted"));
}

#[tokio::test]
async fn aborting_mid_text_stops_before_text_end() {
    let faux = paced();
    faux.set_responses(vec![FauxResponse::text("abcdefghijklmnopqrstuvwxyz")]);

    let (handle, signal) = AbortHandle::new();
    let mut options = SimpleStreamOptions::default();
    options.stream.request.signal = Some(signal);

    let model = faux.model();
    let mut stream = faux
        .stream_simple(&model, &user_context("hi"), &options)
        .await
        .unwrap();

    let mut names = Vec::new();
    let mut deltas = 0;
    while let Some(event) = futures_util::StreamExt::next(&mut stream).await {
        names.push(kind(&event));
        if matches!(event, AssistantMessageEvent::TextDelta { .. }) {
            deltas += 1;
            handle.abort();
        }
    }

    assert_eq!(deltas, 1);
    assert!(names.contains(&"text_start"));
    assert!(names.contains(&"text_delta"));
    assert!(names.contains(&"error"));
    assert!(!names.contains(&"text_end"));
}

#[tokio::test]
async fn aborting_mid_thinking_stops_before_thinking_end() {
    let faux = paced();
    faux.set_responses(vec![faux_assistant_message(faux_thinking(
        "abcdefghijklmnopqrstuvwxyz",
    ))
    .into()]);

    let (handle, signal) = AbortHandle::new();
    let mut options = SimpleStreamOptions::default();
    options.stream.request.signal = Some(signal);

    let model = faux.model();
    let mut stream = faux
        .stream_simple(&model, &user_context("hi"), &options)
        .await
        .unwrap();

    let mut names = Vec::new();
    let mut deltas = 0;
    while let Some(event) = futures_util::StreamExt::next(&mut stream).await {
        names.push(kind(&event));
        if matches!(event, AssistantMessageEvent::ThinkingDelta { .. }) {
            deltas += 1;
            handle.abort();
        }
    }

    assert_eq!(deltas, 1);
    assert!(names.contains(&"thinking_start"));
    assert!(names.contains(&"error"));
    assert!(!names.contains(&"thinking_end"));
}

#[tokio::test]
async fn aborting_mid_tool_call_stops_before_toolcall_end() {
    let faux = paced();
    let mut message = faux_assistant_message(faux_tool_call_with_id(
        "tool-1",
        "echo",
        json!({ "text": "abcdefghijklmnopqrstuvwxyz", "count": 123456789 }),
    ));
    message.stop_reason = StopReason::ToolUse;
    faux.set_responses(vec![message.into()]);

    let (handle, signal) = AbortHandle::new();
    let mut options = SimpleStreamOptions::default();
    options.stream.request.signal = Some(signal);

    let model = faux.model();
    let mut stream = faux
        .stream_simple(&model, &user_context("hi"), &options)
        .await
        .unwrap();

    let mut names = Vec::new();
    let mut deltas = 0;
    while let Some(event) = futures_util::StreamExt::next(&mut stream).await {
        names.push(kind(&event));
        if matches!(event, AssistantMessageEvent::ToolCallDelta { .. }) {
            deltas += 1;
            handle.abort();
        }
    }

    assert_eq!(deltas, 1);
    assert!(names.contains(&"toolcall_start"));
    assert!(names.contains(&"error"));
    assert!(!names.contains(&"toolcall_end"));
}

// ---------------------------------------------------------------------------
// Deferred responses
// ---------------------------------------------------------------------------

#[tokio::test]
async fn defers_a_response_and_resolves_it_on_fetch() {
    let faux = FauxProvider::with_options(FauxOptions::default().deferred(
        pi_provider_misc::FauxDeferredOptions {
            pending_fetches: 1,
            poll_after_ms: Some(250),
        },
    ));
    faux.set_responses(vec![FauxResponse::text("eventual answer")]);

    let model = faux.model();
    let options = SimpleStreamOptions {
        deferred: Some(Deferred::Enabled),
        ..Default::default()
    };
    let events = drain(
        faux.stream_simple(&model, &user_context("hi"), &options)
            .await
            .unwrap(),
    )
    .await;

    assert_eq!(kinds(&events), vec!["start", "done"]);
    let submitted = terminal(&events);
    assert_eq!(submitted.stop_reason, StopReason::Deferred);
    let handle = submitted.deferred.clone().expect("deferred handle");
    assert_eq!(handle.provider, faux.provider_id());
    assert_eq!(handle.poll_after_ms, Some(250));
    assert!(faux.supports_deferred());

    // First fetch is still pending.
    let pending = drain(
        faux.fetch_deferred(&model, &handle, &Default::default())
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(terminal(&pending).stop_reason, StopReason::Deferred);

    // Second fetch materializes the scripted response.
    let ready = drain(
        faux.fetch_deferred(&model, &handle, &Default::default())
            .await
            .unwrap(),
    )
    .await;
    let message = terminal(&ready);
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.text(), "eventual answer");
    assert_eq!(faux.state().deferred_fetch_count, 2);

    // Repeat fetches replay the same resolved message.
    let again = drain(
        faux.fetch_deferred(&model, &handle, &Default::default())
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(terminal(&again).text(), "eventual answer");
}

#[tokio::test]
async fn fetching_an_unknown_deferred_handle_errors_in_the_stream() {
    let faux = FauxProvider::new();
    let model = faux.model();
    let handle = pi_core::message::DeferredHandle {
        provider: faux.provider_id().to_string(),
        model_id: model.id.clone(),
        api: model.api.as_str().to_string(),
        id: "nope".into(),
        expires_at: None,
        poll_after_ms: None,
        data: None,
    };

    let events = drain(
        faux.fetch_deferred(&model, &handle, &Default::default())
            .await
            .unwrap(),
    )
    .await;
    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("Unknown faux deferred response: nope")
    );
}

#[tokio::test]
async fn cancelling_a_deferred_response_is_recorded_and_fails_later_fetches() {
    let faux = FauxProvider::new();
    faux.set_responses(vec![FauxResponse::text("never delivered")]);

    let model = faux.model();
    let options = SimpleStreamOptions {
        deferred: Some(Deferred::Enabled),
        ..Default::default()
    };
    let submitted = terminal(
        &drain(
            faux.stream_simple(&model, &user_context("hi"), &options)
                .await
                .unwrap(),
        )
        .await,
    );
    let handle = submitted.deferred.clone().unwrap();

    faux.cancel_deferred(&model, &handle, &Default::default())
        .await
        .unwrap();
    assert_eq!(faux.state().cancelled_deferred, vec![handle.clone()]);

    let events = drain(
        faux.fetch_deferred(&model, &handle, &Default::default())
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
        .starts_with("Faux deferred response was cancelled"));
}

// ---------------------------------------------------------------------------
// Introspection and wiring
// ---------------------------------------------------------------------------

#[tokio::test]
async fn records_every_request_for_assertions() {
    let faux = FauxProvider::new();
    faux.set_responses(vec![FauxResponse::text("one"), FauxResponse::text("two")]);

    complete(&faux, &user_context("first")).await;
    complete_with(
        &faux,
        &user_context("second"),
        SimpleStreamOptions {
            stream: pi_core::options::StreamOptions {
                session_id: Some("s".into()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;

    let requests = faux.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].context.messages[0]
            .as_user()
            .unwrap()
            .content
            .to_text(),
        "first"
    );
    assert_eq!(requests[1].options.stream.session_id.as_deref(), Some("s"));
    assert_eq!(faux.last_request().unwrap().state.call_count, 2);

    faux.reset();
    assert_eq!(faux.call_count(), 0);
    assert!(faux.requests().is_empty());
}

#[tokio::test]
async fn works_through_the_stream_fn_and_client_handles() {
    let faux = FauxProvider::new();
    faux.set_responses(vec![
        FauxResponse::text("via stream_fn"),
        FauxResponse::text("via client"),
    ]);

    let stream_fn = faux.stream_fn();
    let stream = stream_fn(faux.model(), user_context("hi"), Default::default())
        .await
        .unwrap();
    assert_eq!(terminal(&drain(stream).await).text(), "via stream_fn");

    let client = faux.client();
    let stream = client
        .stream(&faux.model(), &user_context("hi"), &Default::default())
        .await
        .unwrap();
    assert_eq!(terminal(&drain(stream).await).text(), "via client");
}

#[test]
fn exposes_a_registrable_descriptor() {
    let faux = FauxProvider::with_options(
        FauxOptions::default()
            .provider("faux-x")
            .api("faux-api")
            .models(vec![FauxModelDefinition::new("m1")
                .input(vec![Modality::Text])
                .context_window(1000)
                .max_tokens(10)]),
    );

    let registration = faux.registration();
    assert_eq!(registration.descriptor.id, "faux-x");
    assert_eq!(registration.descriptor.api, "faux-api");
    assert_eq!(registration.client.api(), "faux-api");
    let model = &registration.descriptor.models[0];
    assert_eq!(model.id, "m1");
    assert_eq!(model.api.as_str(), "faux-api");
    assert_eq!(model.provider, "faux-x");
    assert_eq!(model.context_window, 1000);
    assert_eq!(model.max_tokens, 10);
    assert_eq!(model.input, vec![Modality::Text]);

    // The descriptor is plain data a catalog can persist.
    let json = serde_json::to_value(&registration.descriptor).unwrap();
    assert_eq!(json["id"], "faux-x");
    assert_eq!(json["models"][0]["id"], "m1");
}

#[tokio::test]
async fn notifies_the_response_hook() {
    use std::sync::atomic::{AtomicU16, Ordering};
    use std::sync::Arc;

    let faux = FauxProvider::new();
    faux.set_responses(vec![FauxResponse::text("hi")]);

    let status = Arc::new(AtomicU16::new(0));
    let seen = status.clone();
    let mut options = SimpleStreamOptions::default();
    options.stream.request.on_response = Some(Arc::new(move |response, _model| {
        seen.store(response.status, Ordering::SeqCst);
    }));

    complete_with(&faux, &user_context("hi"), options).await;
    assert_eq!(status.load(Ordering::SeqCst), 200);
}

#[tokio::test]
async fn content_only_faux_helpers_round_trip() {
    // The scripted content blocks are the same types the rest of the SDK uses.
    let blocks: Vec<AssistantContent> = vec![
        faux_text("a"),
        faux_thinking("b"),
        faux_tool_call_with_id("id", "name", json!({ "k": 1 })),
    ];
    let message = faux_assistant_message(blocks.clone());
    assert_eq!(message.content, blocks);
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(
        pi_provider_misc::faux_tool_use_message("x").stop_reason,
        StopReason::ToolUse
    );
    assert_eq!(
        pi_provider_misc::faux_error_message("bad").stop_reason,
        StopReason::Error
    );
    assert_eq!(
        pi_provider_misc::faux_aborted_message().stop_reason,
        StopReason::Aborted
    );
}

#[tokio::test]
async fn convenience_scripting_helpers() {
    let faux = FauxProvider::new();
    faux.set_texts(&["one", "two"]);
    assert_eq!(faux.pending_response_count(), 2);
    assert_eq!(complete(&faux, &user_context("hi")).await.text(), "one");

    faux.set_messages(vec![faux_assistant_message("three")]);
    assert_eq!(complete(&faux, &user_context("hi")).await.text(), "three");
}
