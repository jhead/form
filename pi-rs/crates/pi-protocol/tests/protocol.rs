//! Port of `.upstream/packages/protocol/test/protocol.test.ts`.

use pi_protocol::*;
use serde_json::{json, Value};

fn client_hello() -> ClientMessage {
    ClientMessage::Hello(ClientHello {
        version: u64::from(PROTOCOL_VERSION),
    })
}

fn empty_server_snapshot() -> ServerSnapshot {
    ServerSnapshot {
        server_id: "server-1".into(),
        protocol_version: PROTOCOL_VERSION,
        revision: 0,
        sessions: vec![],
        models: vec![],
    }
}

fn server_hello() -> ServerMessage {
    ServerMessage::Hello(ServerHello {
        version: PROTOCOL_VERSION,
        connection_id: "connection-1".into(),
        snapshot: empty_server_snapshot(),
    })
}

/// Upstream's `itemMessage` helper.
fn item_message(item: Value, progress_type: &str) -> Value {
    json!({
        "type": "event",
        "event": {
            "type": "session_progress",
            "sessionId": "session-1",
            "progress": { "type": progress_type, "item": item },
        },
    })
}

fn assistant_item(extra: Value) -> Value {
    let mut item = json!({
        "id": "assistant-1",
        "role": "assistant",
        "content": [{ "type": "text", "text": "hello" }],
        "model": { "provider": "test", "id": "model" },
        "timestamp": 1,
    });
    merge(&mut item, extra);
    item
}

fn tool_item(extra: Value) -> Value {
    let mut item = json!({
        "id": "tool-1",
        "role": "tool",
        "toolCallId": "call-1",
        "toolName": "read",
        "input": {},
        "content": [],
        "timestamp": 1,
    });
    merge(&mut item, extra);
    item
}

fn merge(target: &mut Value, extra: Value) {
    let target = target.as_object_mut().expect("object");
    for (key, value) in extra.as_object().expect("object") {
        target.insert(key.clone(), value.clone());
    }
}

// ---------------------------------------------------------------------------
// protocol validation
// ---------------------------------------------------------------------------

#[test]
fn uses_protocol_version_1() {
    assert_eq!(PROTOCOL_VERSION, 1);
    assert!(is_supported_protocol_version(1));
    assert!(!is_supported_protocol_version(2));
    // Upstream also rejects `2.5`; a `u64` parameter makes that unrepresentable.
}

#[test]
fn accepts_any_integer_client_hello_version_for_negotiation() {
    for version in [0u64, 1, 2] {
        let message = json!({ "type": "hello", "version": version });
        assert_eq!(
            parse_client_message_json(&message).expect("parses"),
            ClientMessage::Hello(ClientHello { version }),
        );
    }
}

#[test]
fn rejects_a_malformed_handshake() {
    let cases = [
        ("string version", json!({ "type": "hello", "version": "1" })),
        (
            "fractional version",
            json!({ "type": "hello", "version": 1.5 }),
        ),
        (
            "credential field",
            json!({ "type": "hello", "version": 1, "token": "secret" }),
        ),
        (
            "unknown field",
            json!({ "type": "hello", "version": 1, "extra": true }),
        ),
        (
            "negative version",
            json!({ "type": "hello", "version": -1 }),
        ),
    ];
    for (label, message) in cases {
        assert!(
            parse_client_message_json(&message).is_err(),
            "expected {label} to be rejected",
        );
    }
}

#[test]
fn does_not_parse_json_strings_as_wire_messages() {
    let encoded = serde_json::to_string(&client_hello()).expect("serializes");
    assert!(parse_client_message_json(&json!(encoded)).is_err());
    let encoded = serde_json::to_string(&server_hello()).expect("serializes");
    assert!(parse_server_message_json(&json!(encoded)).is_err());
}

#[test]
fn rejects_image_input_while_the_mvp_remains_text_only() {
    let message = json!({
        "type": "request",
        "id": "request-1",
        "request": {
            "command": "prompt",
            "sessionId": "session-1",
            "text": "inspect",
            "images": [{ "type": "image", "data": "abc", "mimeType": "image/png" }],
        },
    });
    assert!(parse_client_message_json(&message).is_err());
}

#[test]
fn parses_a_server_handshake_snapshot() {
    let value = serde_json::to_value(server_hello()).expect("serializes");
    assert_eq!(
        parse_server_message_json(&value).expect("parses"),
        server_hello()
    );
}

#[test]
fn represents_listed_sessions_as_durable_metadata() {
    let message = json!({
        "type": "response",
        "id": "request-1",
        "ok": true,
        "result": {
            "command": "list",
            "sessions": [{
                "id": "session-1",
                "createdAt": 1,
                "updatedAt": 2,
                "parentSessionId": "parent-1",
                "sessionName": "Named session",
                "cwd": "/workspace",
            }],
        },
    });
    assert!(parse_server_message_json(&message).is_ok());

    // `phase` belongs to a snapshot, not to durable metadata.
    let message = json!({
        "type": "response",
        "id": "request-1",
        "ok": true,
        "result": {
            "command": "list",
            "sessions": [{ "id": "session-1", "createdAt": 1, "phase": "idle" }],
        },
    });
    assert!(parse_server_message_json(&message).is_err());
}

#[test]
fn accepts_the_not_implemented_and_internal_error_codes() {
    for code in [
        ProtocolErrorCode::NotImplemented,
        ProtocolErrorCode::InternalError,
    ] {
        let message = ServerMessage::Response(ResponseEnvelope::failed(
            "request-1",
            ProtocolError::new(code, "safe"),
        ));
        let value = serde_json::to_value(&message).expect("serializes");
        assert_eq!(parse_server_message_json(&value).expect("parses"), message);
    }
}

#[test]
fn rejects_invalid_server_messages() {
    let cases = [
        (
            "unsupported hello version",
            json!({
                "type": "hello",
                "version": 2,
                "connectionId": "connection-1",
                "snapshot": {
                    "serverId": "server-1", "protocolVersion": 1, "revision": 0,
                    "sessions": [], "models": [],
                },
            }),
        ),
        (
            "unknown error code",
            json!({ "type": "hello_error", "error": { "code": "auth", "message": "Authentication failed" } }),
        ),
        (
            "unknown result command",
            json!({ "type": "response", "id": "request-1", "ok": true, "result": { "command": "unknown" } }),
        ),
        (
            "non-string session id",
            json!({ "type": "event", "event": { "type": "session_removed", "sessionId": 42 } }),
        ),
        (
            "empty session id",
            json!({ "type": "event", "event": { "type": "session_removed", "sessionId": "" } }),
        ),
        (
            "ok response carrying an error",
            json!({
                "type": "response", "id": "request-1", "ok": true,
                "error": { "code": "busy", "message": "busy" },
            }),
        ),
        (
            "failed response carrying a result",
            json!({
                "type": "response", "id": "request-1", "ok": false,
                "result": { "command": "detach", "sessionId": "session-1" },
            }),
        ),
    ];
    for (label, message) in cases {
        assert!(
            parse_server_message_json(&message).is_err(),
            "expected {label} to be rejected",
        );
    }
}

#[test]
fn validates_nested_json_tool_details() {
    let message = item_message(
        tool_item(json!({
            "input": { "path": "/tmp/file" },
            "content": [{ "type": "text", "text": "done" }],
            "details": { "lines": [1, 2, 3], "cached": false },
            "status": "complete",
            "isError": false,
        })),
        "item_finished",
    );
    assert!(parse_server_message_json(&message).is_ok());
}

#[test]
fn accepts_consistent_assistant_items() {
    let states = [
        (json!({ "status": "streaming" }), "item_updated"),
        (
            json!({ "status": "complete", "stopReason": "stop" }),
            "item_finished",
        ),
        (
            json!({ "status": "error", "stopReason": "error" }),
            "item_finished",
        ),
        (
            json!({ "status": "error", "stopReason": "error", "errorMessage": "failed" }),
            "item_finished",
        ),
        (
            json!({ "status": "aborted", "stopReason": "aborted" }),
            "item_finished",
        ),
    ];
    for (state, progress_type) in states {
        let message = item_message(assistant_item(state.clone()), progress_type);
        assert!(
            parse_server_message_json(&message).is_ok(),
            "expected {state} to be accepted",
        );
    }
}

#[test]
fn rejects_inconsistent_assistant_items() {
    let states = [
        json!({ "status": "streaming", "stopReason": "stop" }),
        json!({ "status": "complete" }),
        json!({ "status": "complete", "stopReason": "error" }),
        json!({ "status": "error", "stopReason": "error", "errorMessage": "" }),
        json!({ "status": "aborted", "stopReason": "stop" }),
        json!({ "status": "streaming", "errorMessage": "x" }),
    ];
    for state in states {
        let message = item_message(assistant_item(state.clone()), "item_finished");
        assert!(
            parse_server_message_json(&message).is_err(),
            "expected {state} to be rejected",
        );
    }
}

#[test]
fn accepts_consistent_tool_items() {
    let states = [
        (
            json!({ "status": "running", "isError": false }),
            "item_updated",
        ),
        (
            json!({ "status": "complete", "isError": false }),
            "item_finished",
        ),
        (
            json!({ "status": "error", "isError": true }),
            "item_finished",
        ),
    ];
    for (state, progress_type) in states {
        let message = item_message(tool_item(state.clone()), progress_type);
        assert!(
            parse_server_message_json(&message).is_ok(),
            "expected {state} to be accepted",
        );
    }
}

#[test]
fn rejects_inconsistent_tool_items() {
    let states = [
        json!({ "status": "running", "isError": true }),
        json!({ "status": "complete", "isError": true }),
        json!({ "status": "error", "isError": false }),
    ];
    for state in states {
        let message = item_message(tool_item(state.clone()), "item_finished");
        assert!(
            parse_server_message_json(&message).is_err(),
            "expected {state} to be rejected",
        );
    }
}

#[test]
fn rejects_nonterminal_items_reported_as_finished() {
    let assistant = item_message(
        assistant_item(json!({ "status": "streaming" })),
        "item_finished",
    );
    assert!(parse_server_message_json(&assistant).is_err());

    let tool = item_message(
        tool_item(json!({ "status": "running", "isError": false })),
        "item_finished",
    );
    assert!(parse_server_message_json(&tool).is_err());
}

#[test]
fn rejects_user_items_reported_as_activity() {
    // Upstream's `item_updated`/`item_finished` unions exclude user items.
    let user = json!({
        "id": "user-1",
        "role": "user",
        "content": [{ "type": "text", "text": "hi" }],
        "timestamp": 1,
    });
    assert!(parse_server_message_json(&item_message(user.clone(), "item_updated")).is_err());
    assert!(parse_server_message_json(&item_message(user.clone(), "item_finished")).is_err());
    // …but `item_started` accepts them.
    assert!(parse_server_message_json(&item_message(user, "item_started")).is_ok());
}

#[test]
fn validation_errors_do_not_retain_rejected_payloads() {
    let error = parse_client_message_json(&json!({
        "type": "hello",
        "version": "1",
        "extra": "x".repeat(2_000_000),
    }))
    .expect_err("rejects");
    assert_eq!(
        error,
        ProtocolValidationError::InvalidMessage {
            kind: MessageKind::Client
        }
    );
    assert!(error.to_string().len() < 1_000);
    assert_eq!(error.to_string(), "Invalid client protocol message");
}

// Upstream's "rejects cyclic protocol values" case has no analogue: a
// `serde_json::Value` cannot contain a cycle.

// ---------------------------------------------------------------------------
// validated framed protocol APIs
// ---------------------------------------------------------------------------

#[test]
fn encodes_complete_client_and_server_frames() {
    let frame =
        encode_client_message(&client_hello(), FrameDecoderOptions::default()).expect("encodes");
    let frames = FrameDecoder::new().push(&frame).expect("frames");
    assert_eq!(frames.len(), 1);
    let value = decode_cbor(&frames[0], CborOptions::default()).expect("decodes");
    assert_eq!(
        parse_client_message(&value).expect("parses"),
        client_hello()
    );

    let frame =
        encode_server_message(&server_hello(), FrameDecoderOptions::default()).expect("encodes");
    let frames = FrameDecoder::new().push(&frame).expect("frames");
    assert_eq!(frames.len(), 1);
    let value = decode_cbor(&frames[0], CborOptions::default()).expect("decodes");
    assert_eq!(
        parse_server_message(&value).expect("parses"),
        server_hello()
    );
}

#[test]
fn enforces_an_outbound_frame_limit_before_returning_bytes() {
    let options = FrameDecoderOptions::with_max_frame_length(8);
    assert!(matches!(
        encode_client_message(&client_hello(), options),
        Err(ProtocolValidationError::Encode { .. })
    ));
    assert!(matches!(
        encode_server_message(&server_hello(), options),
        Err(ProtocolValidationError::Encode { .. })
    ));
}

#[test]
fn validates_messages_before_encoding() {
    // Upstream's case is a fractional version, which `u64` rules out; the
    // equivalent reachable failure is a refinement the type system cannot hold.
    let message = ServerMessage::Hello(ServerHello {
        version: PROTOCOL_VERSION + 1,
        connection_id: "connection-1".into(),
        snapshot: empty_server_snapshot(),
    });
    assert_eq!(
        encode_server_message(&message, FrameDecoderOptions::default()),
        Err(ProtocolValidationError::InvalidMessage {
            kind: MessageKind::Server
        }),
    );

    let message = ClientMessage::Request(RequestEnvelope {
        id: String::new(),
        request: Command::List(ListCommand {}),
    });
    assert!(encode_client_message(&message, FrameDecoderOptions::default()).is_err());
}

#[test]
fn omits_absent_optional_properties_on_the_wire() {
    let message = ClientMessage::Request(RequestEnvelope {
        id: "request-1".into(),
        request: Command::Create(CreateCommand::default()),
    });
    let frame = encode_client_message(&message, FrameDecoderOptions::default()).expect("encodes");
    let payload = FrameDecoder::new().push(&frame).expect("frames").remove(0);
    let value = decode_cbor(&payload, CborOptions::default()).expect("decodes");
    assert_eq!(
        value.to_json().expect("json"),
        json!({ "type": "request", "id": "request-1", "request": { "command": "create" } }),
    );
}

#[test]
fn incrementally_decodes_fragmented_and_coalesced_client_messages() {
    let request = ClientMessage::Request(RequestEnvelope {
        id: "request-1".into(),
        request: Command::List(ListCommand {}),
    });
    let mut wire =
        encode_client_message(&client_hello(), FrameDecoderOptions::default()).expect("encodes");
    wire.extend(encode_client_message(&request, FrameDecoderOptions::default()).expect("encodes"));

    for split in 0..=wire.len() {
        let mut decoder = ClientMessageDecoder::default();
        let mut messages = decoder.push(&wire[..split]).expect("pushes");
        messages.extend(decoder.push(&wire[split..]).expect("pushes"));
        decoder.end().expect("ends");
        assert_eq!(
            messages,
            vec![client_hello(), request.clone()],
            "split at {split}"
        );
    }
}

#[test]
fn incrementally_decodes_server_messages() {
    let message = ServerMessage::HelloError(ServerHelloError {
        error: ProtocolError::new(ProtocolErrorCode::Version, "Unsupported protocol version"),
    });
    let mut decoder = ServerMessageDecoder::default();
    let frame = encode_server_message(&message, FrameDecoderOptions::default()).expect("encodes");
    assert_eq!(decoder.push(&frame).expect("pushes"), vec![message]);
    decoder.end().expect("ends");
}

#[test]
fn rejects_invalid_framed_client_input_and_stays_failed() {
    let schema_invalid = encode_cbor(
        &CborValue::from_json(&json!({ "type": "hello", "version": 1, "extra": true })),
        CborOptions::default(),
    )
    .expect("encodes");

    let cases = [
        ("empty CBOR payload", encode_frame(&[]).expect("frames")),
        ("malformed CBOR", encode_frame(&[0xff]).expect("frames")),
        (
            "schema-invalid CBOR",
            encode_frame(&schema_invalid).expect("frames"),
        ),
    ];

    for (label, wire) in cases {
        let mut decoder = ClientMessageDecoder::default();
        assert!(decoder.push(&wire).is_err(), "{label}");
        let valid = encode_client_message(&client_hello(), FrameDecoderOptions::default())
            .expect("encodes");
        assert_eq!(
            decoder.push(&valid),
            Err(ProtocolValidationError::DecoderFailed {
                kind: MessageKind::Client
            }),
            "{label}",
        );
    }
}

#[test]
fn rejects_cbor_byte_strings_nested_in_json_valued_fields() {
    let mut details = CborMap::new();
    details.insert("nested".into(), CborValue::Bytes(vec![1, 2, 3]));
    let mut error = CborMap::new();
    error.insert("code".into(), CborValue::Text("invalid_request".into()));
    error.insert("message".into(), CborValue::Text("invalid".into()));
    error.insert("details".into(), CborValue::Map(details));
    let mut message = CborMap::new();
    message.insert("type".into(), CborValue::Text("response".into()));
    message.insert("id".into(), CborValue::Text("request-1".into()));
    message.insert("ok".into(), CborValue::Bool(false));
    message.insert("error".into(), CborValue::Map(error));

    let payload = encode_cbor(&CborValue::Map(message), CborOptions::default()).expect("encodes");
    let wire = encode_frame(&payload).expect("frames");
    assert_eq!(
        ServerMessageDecoder::default().push(&wire),
        Err(ProtocolValidationError::InvalidMessage {
            kind: MessageKind::Server
        }),
    );
}

#[test]
fn rejects_truncated_and_oversized_framing_through_the_validated_decoder() {
    let mut truncated = ServerMessageDecoder::default();
    assert_eq!(
        truncated.push(&[0, 0, 0, 2, 1]).expect("pushes"),
        Vec::<ServerMessage>::new()
    );
    assert!(matches!(
        truncated.end(),
        Err(ProtocolValidationError::InvalidFraming { .. })
    ));
    assert_eq!(
        truncated.end(),
        Err(ProtocolValidationError::DecoderFailed {
            kind: MessageKind::Server
        }),
    );

    let mut oversized = ClientMessageDecoder::new(FrameDecoderOptions::with_max_frame_length(3));
    assert!(matches!(
        oversized.push(&[0, 0, 0, 4]),
        Err(ProtocolValidationError::InvalidFrame { .. })
    ));
}

#[test]
fn error_codes_are_stable() {
    assert_eq!(
        ProtocolValidationError::InvalidMessage {
            kind: MessageKind::Client
        }
        .code(),
        "protocol_invalid_message"
    );
    assert_eq!(FrameError::Failed.code(), "frame_decoder_failed");
    assert_eq!(CborError::Truncated.code(), "cbor_truncated");
}
