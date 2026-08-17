//! Byte-level wire fixtures.
//!
//! Every hex string here was produced by running **upstream's own**
//! `encodeClientMessage` / `encodeServerMessage`
//! (`.upstream/packages/protocol/src/codec.ts`) under Bun against the
//! equivalent TypeScript object literal, with the properties written in the
//! order `schemas.ts` declares them — which is also the order
//! `.upstream/packages/server/src/protocol.ts` builds them in, and therefore
//! the order they reach the wire, since these CBOR maps are insertion-ordered.
//!
//! Asserting the Rust encoder reproduces these byte for byte is the only real
//! proof that a Rust client and the TypeScript server speak the same protocol;
//! a round-trip test would pass just as happily on a private dialect.

mod common;

use common::{from_hex, to_hex};
use pi_protocol::*;

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

const CLIENT_HELLO: &str = "00000015a264747970656568656c6c6f6776657273696f6e01";

const CLIENT_REQUEST_LIST: &str = "00000031a36474797065677265717565737462696469726571756573742d316772657175657374a167636f6d6d616e64646c697374";

const CLIENT_REQUEST_CREATE_EMPTY: &str = "00000033a36474797065677265717565737462696469726571756573742d316772657175657374a167636f6d6d616e6466637265617465";

const CLIENT_REQUEST_CREATE_FULL: &str = "0000008ca36474797065677265717565737462696469726571756573742d326772657175657374a567636f6d6d616e6466637265617465636377646a2f776f726b7370616365646e616d656444656d6f656d6f64656ca26870726f766964657269616e7468726f7069636269646f636c617564652d736f6e6e65742d346d7468696e6b696e674c6576656c6468696768";

const CLIENT_REQUEST_PROMPT: &str = "00000054a36474797065677265717565737462696469726571756573742d336772657175657374a367636f6d6d616e646670726f6d70746973657373696f6e49646973657373696f6e2d31647465787467696e7370656374";

const CLIENT_REQUEST_SET_THINKING: &str = "0000005fa36474797065677265717565737462696469726571756573742d346772657175657374a367636f6d6d616e646c7365745f7468696e6b696e676973657373696f6e49646973657373696f6e2d316d7468696e6b696e674c6576656c636f6666";

const SERVER_HELLO: &str = "00000078a464747970656568656c6c6f6776657273696f6e016c636f6e6e656374696f6e49646c636f6e6e656374696f6e2d3168736e617073686f74a5687365727665724964687365727665722d316f70726f746f636f6c56657273696f6e01687265766973696f6e006873657373696f6e7380666d6f64656c7380";

const SERVER_HELLO_ERROR: &str = "0000004ca264747970656b68656c6c6f5f6572726f72656572726f72a264636f64656776657273696f6e676d657373616765781c556e737570706f727465642070726f746f636f6c2076657273696f6e";

const SERVER_RESPONSE_LIST: &str = "000000a5a4647479706568726573706f6e736562696469726571756573742d31626f6bf566726573756c74a267636f6d6d616e64646c6973746873657373696f6e7381a66269646973657373696f6e2d31696372656174656441740169757064617465644174026f706172656e7453657373696f6e496468706172656e742d316b73657373696f6e4e616d656d4e616d65642073657373696f6e636377646a2f776f726b7370616365";

const SERVER_RESPONSE_ERROR: &str = "0000004ea4647479706568726573706f6e736562696469726571756573742d31626f6bf4656572726f72a264636f6465696e6f745f666f756e64676d6573736167656f6e6f20737563682073657373696f6e";

const SERVER_RESPONSE_ATTACH: &str = "00000221a4647479706568726573706f6e736562696469726571756573742d32626f6bf566726573756c74a267636f6d6d616e64666174746163686773657373696f6eae6269646973657373696f6e2d31646e616d656444656d6f636377646a2f776f726b7370616365696372656174656441740169757064617465644174026570686173656469646c65656d6f64656ca26870726f766964657269616e7468726f7069636269646f636c617564652d736f6e6e65742d346d7468696e6b696e674c6576656c666d656469756d686174746163686564f5666c6f636b6564f4687265766973696f6e036a7472616e73637269707482a462696466757365722d3164726f6c65647573657267636f6e74656e7481a26474797065647465787464746578746268696974696d657374616d700aa76269646b617373697374616e742d3164726f6c6569617373697374616e7467636f6e74656e7481a26474797065647465787464746578746568656c6c6f656d6f64656ca26870726f766964657269616e7468726f7069636269646f636c617564652d736f6e6e65742d346974696d657374616d700b6673746174757368636f6d706c6574656a73746f70526561736f6e6473746f706b717565756564537465657281a46269646773746565722d3164726f6c65647573657267636f6e74656e7481a264747970656474657874647465787464776169746974696d657374616d700c707175657565645374656572436f756e7401";

const SERVER_EVENT_TOOL_FINISHED: &str = "00000107a26474797065656576656e74656576656e74a364747970657073657373696f6e5f70726f67726573736973657373696f6e49646973657373696f6e2d316870726f6772657373a264747970656d6974656d5f66696e6973686564646974656daa62696466746f6f6c2d3164726f6c6564746f6f6c6a746f6f6c43616c6c49646663616c6c2d3168746f6f6c4e616d65647265616465696e707574a16470617468692f746d702f66696c6567636f6e74656e7481a264747970656474657874647465787464646f6e656764657461696c73a2656c696e65738301020366636163686564f46974696d657374616d70016673746174757368636f6d706c6574656769734572726f72f4";

const SERVER_EVENT_ASSISTANT_DELTA: &str = "00000094a26474797065656576656e74656576656e74a364747970657073657373696f6e5f70726f67726573736973657373696f6e49646973657373696f6e2d316870726f6772657373a564747970656f617373697374616e745f64656c7461696d65737361676549646b617373697374616e742d316c636f6e74656e74496e64657800646b696e6464746578746564656c74616368656c";

const SERVER_EVENT_ASSISTANT_FINISHED: &str = "00000210a26474797065656576656e74656576656e74a364747970657073657373696f6e5f70726f67726573736973657373696f6e49646973657373696f6e2d316870726f6772657373a264747970656d6974656d5f66696e6973686564646974656daa6269646b617373697374616e742d3264726f6c6569617373697374616e7467636f6e74656e7482a36474797065687468696e6b696e67687468696e6b696e6763686d6d687265646163746564f4a4647479706568746f6f6c43616c6c6a746f6f6c43616c6c49646663616c6c2d3168746f6f6c4e616d65647265616465696e707574a16470617468662f746d702f66656d6f64656ca26870726f766964657269616e7468726f7069636269646f636c617564652d736f6e6e65742d346d726573706f6e73654d6f64656c7818636c617564652d736f6e6e65742d342d3230323530353134657573616765a765696e7075740a666f75747075741469636163686552656164006a636163686557726974650069726561736f6e696e67056b746f74616c546f6b656e73182364636f7374a565696e707574fb3f9eb851eb851eb8666f7574707574fb3fd333333333333369636163686552656164006a636163686557726974650065746f74616cfb3fd51eb851eb851f6974696d657374616d700d66737461747573656572726f726a73746f70526561736f6e656572726f726c6572726f724d65737361676564626f6f6d";

const SERVER_EVENT_SESSION_REMOVED: &str = "0000003ca26474797065656576656e74656576656e74a264747970656f73657373696f6e5f72656d6f7665646973657373696f6e49646973657373696f6e2d31";

const SERVER_EVENT_SERVER_SNAPSHOT: &str = "00000192a26474797065656576656e74656576656e74a264747970656f7365727665725f736e617073686f7468736e617073686f74a5687365727665724964687365727665722d316f70726f746f636f6c56657273696f6e01687265766973696f6e076873657373696f6e7381a26269646973657373696f6e2d316963726561746564417401666d6f64656c7381ab6870726f766964657269616e7468726f7069636269646f636c617564652d736f6e6e65742d34646e616d656f436c6175646520536f6e6e657420346361706972616e7468726f7069632d6d6573736167657369726561736f6e696e67f565696e70757482647465787465696d6167656d636f6e7465787457696e646f771a00030d40696d6178546f6b656e7319fa0064636f7374a465696e70757403666f75747075740f69636163686552656164fb3fd33333333333336a63616368655772697465fb400e00000000000077737570706f727465645468696e6b696e674c6576656c7384636f6666636c6f77666d656469756d64686967686d61757468656e74696361746564f5";

// ---------------------------------------------------------------------------
// builders
// ---------------------------------------------------------------------------

fn model() -> ModelRef {
    ModelRef {
        provider: "anthropic".into(),
        id: "claude-sonnet-4".into(),
    }
}

fn text_content(text: &str) -> TextContent {
    TextContent { text: text.into() }
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

fn session_snapshot() -> SessionSnapshot {
    SessionSnapshot {
        id: "session-1".into(),
        name: Some("Demo".into()),
        cwd: "/workspace".into(),
        created_at: 1,
        updated_at: 2,
        phase: SessionPhase::Idle,
        model: model(),
        thinking_level: ThinkingLevel::Medium,
        attached: true,
        locked: false,
        revision: 3,
        transcript: vec![
            TranscriptItem::User(UserTranscriptItem::new(
                "user-1",
                vec![UserContent::Text(text_content("hi"))],
                10,
            )),
            TranscriptItem::Assistant(AssistantTranscriptItem {
                id: "assistant-1".into(),
                role: AssistantRole::Assistant,
                content: vec![AssistantContent::Text(text_content("hello"))],
                model: model(),
                response_model: None,
                usage: None,
                timestamp: 11,
                status: AssistantStatus::Complete,
                stop_reason: Some(AssistantStopReason::Stop),
                error_message: None,
            }),
        ],
        queued_steer: vec![UserTranscriptItem::new(
            "steer-1",
            vec![UserContent::Text(text_content("wait"))],
            12,
        )],
        queued_steer_count: 1,
    }
}

fn tool_item() -> ToolTranscriptItem {
    ToolTranscriptItem {
        id: "tool-1".into(),
        role: ToolRole::Tool,
        tool_call_id: "call-1".into(),
        tool_name: "read".into(),
        input: serde_json::json!({ "path": "/tmp/file" }),
        content: vec![ToolContent::Text(text_content("done"))],
        details: Some(serde_json::json!({ "lines": [1, 2, 3], "cached": false })),
        usage: None,
        timestamp: 1,
        status: ToolStatus::Complete,
        is_error: false,
    }
}

fn rich_assistant_item() -> AssistantTranscriptItem {
    AssistantTranscriptItem {
        id: "assistant-2".into(),
        role: AssistantRole::Assistant,
        content: vec![
            AssistantContent::Thinking(ThinkingContent {
                thinking: "hmm".into(),
                redacted: Some(false),
            }),
            AssistantContent::ToolCall(ToolCallContent {
                tool_call_id: "call-1".into(),
                tool_name: "read".into(),
                input: serde_json::json!({ "path": "/tmp/f" }),
            }),
        ],
        model: model(),
        response_model: Some("claude-sonnet-4-20250514".into()),
        usage: Some(Usage {
            input: 10,
            output: 20,
            cache_read: 0,
            cache_write: 0,
            reasoning: Some(5),
            total_tokens: 35,
            cost: UsageCost {
                input: 0.03,
                output: 0.3,
                cache_read: 0.0,
                cache_write: 0.0,
                total: 0.33,
            },
        }),
        timestamp: 13,
        status: AssistantStatus::Error,
        stop_reason: Some(AssistantStopReason::Error),
        error_message: Some("boom".into()),
    }
}

fn progress_event(progress: TranscriptProgress) -> ServerMessage {
    ServerMessage::Event(EventEnvelope {
        event: ServerEvent::SessionProgress(SessionProgressEvent {
            session_id: "session-1".into(),
            progress,
        }),
    })
}

fn model_metadata() -> ModelMetadata {
    ModelMetadata {
        provider: "anthropic".into(),
        id: "claude-sonnet-4".into(),
        name: "Claude Sonnet 4".into(),
        api: "anthropic-messages".into(),
        reasoning: true,
        input: vec![Modality::Text, Modality::Image],
        context_window: 200_000,
        max_tokens: 64_000,
        cost: ModelCost {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write: 3.75,
        },
        supported_thinking_levels: vec![
            ThinkingLevel::Off,
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
        ],
        authenticated: true,
    }
}

fn client_cases() -> Vec<(&'static str, ClientMessage)> {
    let request = |id: &str, command: Command| {
        ClientMessage::Request(RequestEnvelope {
            id: id.into(),
            request: command,
        })
    };
    vec![
        (
            CLIENT_HELLO,
            ClientMessage::Hello(ClientHello { version: 1 }),
        ),
        (
            CLIENT_REQUEST_LIST,
            request("request-1", Command::List(ListCommand {})),
        ),
        (
            CLIENT_REQUEST_CREATE_EMPTY,
            request("request-1", Command::Create(CreateCommand::default())),
        ),
        (
            CLIENT_REQUEST_CREATE_FULL,
            request(
                "request-2",
                Command::Create(CreateCommand {
                    cwd: Some("/workspace".into()),
                    name: Some("Demo".into()),
                    model: Some(model()),
                    thinking_level: Some(ThinkingLevel::High),
                }),
            ),
        ),
        (
            CLIENT_REQUEST_PROMPT,
            request(
                "request-3",
                Command::Prompt(PromptCommand {
                    session_id: "session-1".into(),
                    text: "inspect".into(),
                }),
            ),
        ),
        (
            CLIENT_REQUEST_SET_THINKING,
            request(
                "request-4",
                Command::SetThinking(SetThinkingCommand {
                    session_id: "session-1".into(),
                    thinking_level: ThinkingLevel::Off,
                }),
            ),
        ),
    ]
}

fn server_cases() -> Vec<(&'static str, ServerMessage)> {
    vec![
        (
            SERVER_HELLO,
            ServerMessage::Hello(ServerHello {
                version: PROTOCOL_VERSION,
                connection_id: "connection-1".into(),
                snapshot: empty_server_snapshot(),
            }),
        ),
        (
            SERVER_HELLO_ERROR,
            ServerMessage::HelloError(ServerHelloError {
                error: ProtocolError::new(
                    ProtocolErrorCode::Version,
                    "Unsupported protocol version",
                ),
            }),
        ),
        (
            SERVER_RESPONSE_LIST,
            ServerMessage::Response(ResponseEnvelope::ok(
                "request-1",
                CommandResult::List(ListResult {
                    sessions: vec![SessionMetadata {
                        id: "session-1".into(),
                        created_at: 1,
                        updated_at: Some(2),
                        parent_session_id: Some("parent-1".into()),
                        session_name: Some("Named session".into()),
                        cwd: Some("/workspace".into()),
                    }],
                }),
            )),
        ),
        (
            SERVER_RESPONSE_ERROR,
            ServerMessage::Response(ResponseEnvelope::failed(
                "request-1",
                ProtocolError::new(ProtocolErrorCode::NotFound, "no such session"),
            )),
        ),
        (
            SERVER_RESPONSE_ATTACH,
            ServerMessage::Response(ResponseEnvelope::ok(
                "request-2",
                CommandResult::Attach(SessionResult {
                    session: session_snapshot(),
                }),
            )),
        ),
        (
            SERVER_EVENT_TOOL_FINISHED,
            progress_event(TranscriptProgress::ItemFinished(ItemFinished {
                item: TranscriptItem::Tool(tool_item()),
            })),
        ),
        (
            SERVER_EVENT_ASSISTANT_DELTA,
            progress_event(TranscriptProgress::AssistantDelta(AssistantDelta {
                message_id: "assistant-1".into(),
                content_index: 0,
                kind: AssistantDeltaKind::Text,
                delta: "hel".into(),
            })),
        ),
        (
            SERVER_EVENT_ASSISTANT_FINISHED,
            progress_event(TranscriptProgress::ItemFinished(ItemFinished {
                item: TranscriptItem::Assistant(rich_assistant_item()),
            })),
        ),
        (
            SERVER_EVENT_SESSION_REMOVED,
            ServerMessage::Event(EventEnvelope {
                event: ServerEvent::SessionRemoved(SessionRemovedEvent {
                    session_id: "session-1".into(),
                }),
            }),
        ),
        (
            SERVER_EVENT_SERVER_SNAPSHOT,
            ServerMessage::Event(EventEnvelope {
                event: ServerEvent::ServerSnapshot(ServerSnapshotEvent {
                    snapshot: ServerSnapshot {
                        server_id: "server-1".into(),
                        protocol_version: PROTOCOL_VERSION,
                        revision: 7,
                        sessions: vec![SessionMetadata {
                            id: "session-1".into(),
                            created_at: 1,
                            updated_at: None,
                            parent_session_id: None,
                            session_name: None,
                            cwd: None,
                        }],
                        models: vec![model_metadata()],
                    },
                }),
            }),
        ),
    ]
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[test]
fn client_messages_encode_to_upstream_bytes() {
    for (wire, message) in client_cases() {
        let encoded = encode_client_message(&message, FrameDecoderOptions::default())
            .unwrap_or_else(|error| panic!("encoding {message:?}: {error}"));
        assert_eq!(to_hex(&encoded), wire, "encoding {message:?}");
    }
}

#[test]
fn server_messages_encode_to_upstream_bytes() {
    for (wire, message) in server_cases() {
        let encoded = encode_server_message(&message, FrameDecoderOptions::default())
            .unwrap_or_else(|error| panic!("encoding {message:?}: {error}"));
        assert_eq!(to_hex(&encoded), wire, "encoding {message:?}");
    }
}

#[test]
fn upstream_client_bytes_decode_to_the_expected_messages() {
    for (wire, message) in client_cases() {
        let mut decoder = ClientMessageDecoder::default();
        let decoded = decoder.push(&from_hex(wire)).expect("decodes");
        decoder.end().expect("ends");
        assert_eq!(decoded, vec![message], "decoding {wire}");
    }
}

#[test]
fn upstream_server_bytes_decode_to_the_expected_messages() {
    for (wire, message) in server_cases() {
        let mut decoder = ServerMessageDecoder::default();
        let decoded = decoder.push(&from_hex(wire)).expect("decodes");
        decoder.end().expect("ends");
        assert_eq!(decoded, vec![message], "decoding {wire}");
    }
}

#[test]
fn every_upstream_fixture_survives_a_full_round_trip() {
    for (wire, message) in client_cases() {
        let mut decoder = ClientMessageDecoder::default();
        let decoded = decoder.push(&from_hex(wire)).expect("decodes").remove(0);
        assert_eq!(
            to_hex(
                &encode_client_message(&decoded, FrameDecoderOptions::default())
                    .expect("re-encodes")
            ),
            wire,
        );
        assert_eq!(decoded, message);
    }
    for (wire, message) in server_cases() {
        let mut decoder = ServerMessageDecoder::default();
        let decoded = decoder.push(&from_hex(wire)).expect("decodes").remove(0);
        assert_eq!(
            to_hex(
                &encode_server_message(&decoded, FrameDecoderOptions::default())
                    .expect("re-encodes")
            ),
            wire,
        );
        assert_eq!(decoded, message);
    }
}

#[test]
fn a_stream_of_every_client_fixture_decodes_at_every_split_point() {
    let wire: Vec<u8> = client_cases()
        .into_iter()
        .flat_map(|(wire, _)| from_hex(wire))
        .collect();
    let expected: Vec<ClientMessage> = client_cases()
        .into_iter()
        .map(|(_, message)| message)
        .collect();

    for split in 0..=wire.len() {
        let mut decoder = ClientMessageDecoder::default();
        let mut messages = decoder.push(&wire[..split]).expect("pushes");
        messages.extend(decoder.push(&wire[split..]).expect("pushes"));
        decoder.end().expect("ends");
        assert_eq!(messages, expected, "split at {split}");
    }
}

#[test]
fn decoded_messages_are_order_insensitive() {
    // The decoder must accept a peer that built the same message with its keys
    // in a different order — key order is a property of the producer, not of
    // the format.
    let reordered = serde_json::json!({
        "version": 1,
        "type": "hello",
    });
    assert_eq!(
        parse_client_message_json(&reordered).expect("parses"),
        ClientMessage::Hello(ClientHello { version: 1 }),
    );
}
