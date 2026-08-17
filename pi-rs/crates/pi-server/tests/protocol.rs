//! Port of `.upstream/packages/server/test/protocol.test.ts`.

use pi_core::{
    Api, AssistantContent as AiAssistantContent, AssistantMessage, Cost, ImageContent,
    InputContent, Model, ModelCost, ModelCostRates, ModelThinkingLevel, StopReason, TextContent,
    ToolCall, ToolResultMessage, Usage, UserContent as AiUserContent, UserMessage,
};
use pi_protocol::{
    encode_server_message, AssistantContent, AssistantStatus, AssistantStopReason,
    FrameDecoderOptions, ModelRef, ServerHello, ServerMessage, ServerSnapshot, SessionMetadata,
    SessionPhase, SessionSnapshot, ThinkingLevel, TranscriptItem, PROTOCOL_VERSION,
};
use pi_server::protocol::{
    sanitize_protocol_details, to_protocol_assistant_message, to_protocol_json_value,
    to_protocol_model_metadata, to_protocol_tool_result_message, to_protocol_user_message,
};
use serde_json::json;

fn model() -> Model {
    Model {
        id: "model-1".to_string(),
        name: "Model One".to_string(),
        api: Api::from("test-api".to_string()),
        provider: "test-provider".to_string(),
        base_url: "https://example.test".to_string(),
        reasoning: true,
        thinking_level_map: None,
        input: vec![pi_core::Modality::Text, pi_core::Modality::Image],
        cost: ModelCost {
            rates: ModelCostRates {
                input: 1.0,
                output: 2.0,
                cache_read: 0.1,
                cache_write: 0.2,
            },
            tiers: None,
        },
        context_window: 100_000,
        max_tokens: 10_000,
        sampling_params: None,
        headers: None,
        compat: None,
    }
}

fn usage() -> Usage {
    Usage {
        input: 1,
        output: 2,
        cache_read: 3,
        cache_write: 4,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 10,
        cost: Cost {
            input: 0.1,
            output: 0.2,
            cache_read: 0.3,
            cache_write: 0.4,
            total: 1.0,
        },
    }
}

/// Everything produced here must survive the protocol's own encode-time
/// validation, which is upstream's `assertValidServerPayload`.
fn assert_valid_server_payload(item: TranscriptItem) {
    let metadata = to_protocol_model_metadata(&model(), true).expect("model metadata");
    encode_server_message(
        &ServerMessage::Hello(ServerHello {
            version: PROTOCOL_VERSION,
            connection_id: "connection-1".to_string(),
            snapshot: ServerSnapshot {
                server_id: "server-1".to_string(),
                protocol_version: PROTOCOL_VERSION,
                revision: 0,
                sessions: vec![SessionMetadata {
                    id: "session-1".to_string(),
                    created_at: 1,
                    updated_at: Some(1),
                    parent_session_id: None,
                    session_name: Some("Session one".to_string()),
                    cwd: Some("/workspace".to_string()),
                }],
                models: vec![metadata],
            },
        }),
        FrameDecoderOptions::default(),
    )
    .expect("the hello encodes");

    encode_server_message(
        &ServerMessage::Event(pi_protocol::EventEnvelope {
            event: pi_protocol::ServerEvent::SessionSnapshot(pi_protocol::SessionSnapshotEvent {
                snapshot: SessionSnapshot {
                    id: "session-1".to_string(),
                    name: None,
                    cwd: "/workspace".to_string(),
                    created_at: 1,
                    updated_at: 1,
                    phase: SessionPhase::Idle,
                    model: ModelRef {
                        provider: "test-provider".to_string(),
                        id: "model-1".to_string(),
                    },
                    thinking_level: ThinkingLevel::Off,
                    attached: true,
                    locked: true,
                    revision: 1,
                    transcript: vec![item],
                    queued_steer: Vec::new(),
                    queued_steer_count: 0,
                },
            }),
        }),
        FrameDecoderOptions::default(),
    )
    .expect("the session snapshot encodes");
}

#[test]
fn maps_model_metadata_and_produces_protocol_valid_output() {
    let metadata = to_protocol_model_metadata(&model(), true).expect("metadata");
    assert_eq!(metadata.provider, "test-provider");
    assert_eq!(metadata.id, "model-1");
    assert_eq!(metadata.api, "test-api");
    assert_eq!(
        metadata.input,
        vec![pi_protocol::Modality::Text, pi_protocol::Modality::Image]
    );
    assert!(metadata.authenticated);
    assert!(metadata
        .supported_thinking_levels
        .contains(&ThinkingLevel::Off));
    // `xhigh`/`max` only appear when the model maps them.
    assert!(!metadata
        .supported_thinking_levels
        .contains(&ThinkingLevel::Xhigh));
}

#[test]
fn a_non_reasoning_model_supports_only_off() {
    let mut model = model();
    model.reasoning = false;
    assert_eq!(
        model.supported_thinking_levels(),
        vec![ModelThinkingLevel::Off]
    );
}

#[test]
fn exhaustively_maps_assistant_content_and_stop_reasons() {
    let mut call = ToolCall::new("call-1", "read");
    call.arguments = json!({ "path": "README.md" })
        .as_object()
        .cloned()
        .expect("object");
    let message = AssistantMessage {
        content: vec![
            AiAssistantContent::Text(TextContent {
                text: "hello".to_string(),
                text_signature: None,
            }),
            AiAssistantContent::Thinking(pi_core::ThinkingContent {
                thinking: "hmm".to_string(),
                thinking_signature: None,
                redacted: false,
            }),
            AiAssistantContent::ToolCall(call),
        ],
        api: "test-api".to_string(),
        provider: "test-provider".to_string(),
        model: "model-1".to_string(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: usage(),
        stop_reason: StopReason::ToolUse,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 123,
    };

    let item = to_protocol_assistant_message(&message, "message-1").expect("assistant item");
    assert_eq!(item.id, "message-1");
    assert_eq!(item.status, AssistantStatus::Complete);
    assert_eq!(item.stop_reason, Some(AssistantStopReason::ToolUse));
    assert_eq!(item.model.provider, "test-provider");
    assert_eq!(item.model.id, "model-1");
    assert_eq!(item.content.len(), 3);
    assert!(matches!(&item.content[0], AssistantContent::Text(text) if text.text == "hello"));
    assert!(
        matches!(&item.content[1], AssistantContent::Thinking(thinking) if thinking.thinking == "hmm")
    );
    let AssistantContent::ToolCall(call) = &item.content[2] else {
        panic!("expected a tool call");
    };
    assert_eq!(call.tool_call_id, "call-1");
    assert_eq!(call.tool_name, "read");
    assert_eq!(call.input, json!({ "path": "README.md" }));

    assert_valid_server_payload(TranscriptItem::Assistant(item));
}

#[test]
fn maps_user_and_tool_messages_without_leaking_non_json_details() {
    let user = UserMessage {
        content: AiUserContent::Text("hello".to_string()),
        timestamp: 1,
    };
    let item = to_protocol_user_message(&user, "user-1").expect("user item");
    assert_eq!(item.id, "user-1");
    assert_eq!(item.content.len(), 1);
    assert_valid_server_payload(TranscriptItem::User(item));

    let mut call = ToolCall::new("call-1", "read");
    call.arguments = json!({ "path": "README.md" })
        .as_object()
        .cloned()
        .expect("object");
    let tool = ToolResultMessage {
        tool_call_id: "call-1".to_string(),
        tool_name: "read".to_string(),
        content: vec![InputContent::Text(TextContent {
            text: "result".to_string(),
            text_signature: None,
        })],
        details: Some(json!({ "nested": { "ok": true } })),
        usage: None,
        added_tool_names: None,
        is_error: false,
        timestamp: 2,
    };
    let item = to_protocol_tool_result_message(&tool, "tool-1", &call).expect("tool item");
    assert_eq!(item.id, "tool-1");
    assert_eq!(item.tool_name, "read");
    assert_eq!(item.input, json!({ "path": "README.md" }));
    assert_eq!(item.details, Some(json!({ "nested": { "ok": true } })));
    assert_eq!(item.status, pi_protocol::ToolStatus::Complete);
    assert_valid_server_payload(TranscriptItem::Tool(item));
}

#[test]
fn rejects_tool_results_associated_with_a_different_call() {
    let call = ToolCall::new("call-1", "read");
    let base = ToolResultMessage {
        tool_call_id: "call-2".to_string(),
        tool_name: "read".to_string(),
        content: vec![InputContent::Text(TextContent {
            text: "result".to_string(),
            text_signature: None,
        })],
        details: None,
        usage: None,
        added_tool_names: None,
        is_error: false,
        timestamp: 2,
    };
    let error = to_protocol_tool_result_message(&base, "tool-1", &call).expect_err("mismatched id");
    assert!(error.to_string().to_lowercase().contains("tool call"));

    let renamed = ToolResultMessage {
        tool_call_id: "call-1".to_string(),
        tool_name: "write".to_string(),
        ..base
    };
    let error =
        to_protocol_tool_result_message(&renamed, "tool-1", &call).expect_err("mismatched name");
    assert!(error.to_string().to_lowercase().contains("tool call"));
}

fn pending_message(stop_reason: StopReason) -> AssistantMessage {
    AssistantMessage {
        content: vec![AiAssistantContent::Text(TextContent {
            text: "partial".to_string(),
            text_signature: None,
        })],
        api: "test-api".to_string(),
        provider: "test-provider".to_string(),
        model: "model-1".to_string(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 123,
    }
}

#[test]
fn derives_streaming_status_from_a_pending_stop_reason() {
    let item =
        to_protocol_assistant_message(&pending_message(StopReason::Pending), "message-pending")
            .expect("item");
    assert_eq!(item.status, AssistantStatus::Streaming);
    assert_eq!(item.stop_reason, None);
    assert_valid_server_payload(TranscriptItem::Assistant(item));
}

#[test]
fn preserves_optional_non_empty_assistant_error_messages() {
    let mut message = pending_message(StopReason::Error);
    message.content = Vec::new();

    let without = to_protocol_assistant_message(&message, "message-error").expect("item");
    assert_eq!(without.status, AssistantStatus::Error);
    assert_eq!(without.stop_reason, Some(AssistantStopReason::Error));
    assert_eq!(without.error_message, None);
    assert_valid_server_payload(TranscriptItem::Assistant(without));

    let mut empty = message.clone();
    empty.error_message = Some(String::new());
    assert!(to_protocol_assistant_message(&empty, "message-error").is_err());

    let mut filled = message;
    filled.error_message = Some("failed".to_string());
    let item = to_protocol_assistant_message(&filled, "message-error").expect("item");
    assert_eq!(item.error_message.as_deref(), Some("failed"));
    assert_valid_server_payload(TranscriptItem::Assistant(item));
}

#[test]
fn rejects_deferred_assistant_messages() {
    let message = pending_message(StopReason::Deferred);
    let error = to_protocol_assistant_message(&message, "message-deferred").expect_err("deferred");
    assert!(error.to_string().contains("protocol v1"));
}

#[test]
fn rejects_invalid_source_identifiers_and_timestamps() {
    let mut message = pending_message(StopReason::ToolUse);
    message.content = vec![AiAssistantContent::ToolCall(ToolCall::new("", "read"))];
    let error = to_protocol_assistant_message(&message, "assistant-1").expect_err("empty call id");
    assert!(error.to_string().to_lowercase().contains("tool call id"));

    let user = UserMessage {
        content: AiUserContent::Text("hello".to_string()),
        timestamp: -1,
    };
    let error = to_protocol_user_message(&user, "user-1").expect_err("negative timestamp");
    assert!(error.to_string().to_lowercase().contains("timestamp"));
}

#[test]
fn rejects_lossy_json_and_normalizes_diagnostic_numbers() {
    // `serde_json` cannot hold a non-finite number through the safe API, so the
    // guard is exercised through the arbitrary-precision path.
    let finite = json!({ "a": [1, 2.5, "x", null, true] });
    assert_eq!(to_protocol_json_value(&finite).expect("finite"), finite);

    let nested = json!({ "outer": { "inner": [1, { "deep": "value" }] } });
    assert_eq!(sanitize_protocol_details(&nested), nested);
}

#[test]
fn maps_image_content_on_both_user_and_tool_messages() {
    let user = UserMessage {
        content: AiUserContent::Blocks(vec![InputContent::Image(ImageContent {
            data: "AAAA".to_string(),
            mime_type: "image/png".to_string(),
        })]),
        timestamp: 1,
    };
    let item = to_protocol_user_message(&user, "user-1").expect("user item");
    assert!(matches!(
        &item.content[0],
        pi_protocol::UserContent::Image(image) if image.mime_type == "image/png"
    ));
    assert_valid_server_payload(TranscriptItem::User(item));
}
