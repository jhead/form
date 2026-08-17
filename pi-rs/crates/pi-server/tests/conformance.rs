//! Port of `.upstream/packages/server/test/conformance.test.ts`.

mod common;

use std::sync::Arc;

use common::{start_harness, start_harness_with, Harness};
use pi_protocol::{
    encode_client_message, encode_frame, ClientHello, ClientMessage, Command, FrameDecoderOptions,
    ListCommand, ProtocolErrorCode, ServerEvent, ServerMessage, SessionCommand, PROTOCOL_VERSION,
};
use pi_server::testing::{
    connect_unix_test_client, Deferred, ProtocolTestClient, TestServerService,
};
use pi_server::{PiServerError, ServerErrorReport};

async fn connect(harness: &Harness) -> Arc<ProtocolTestClient> {
    connect_unix_test_client(harness.socket_path())
        .await
        .expect("wire client connects")
}

fn error_code(message: &ServerMessage) -> Option<ProtocolErrorCode> {
    match message {
        ServerMessage::HelloError(error) => Some(error.error.code),
        _ => None,
    }
}

#[tokio::test]
async fn accepts_a_transport_fragmented_framed_cbor_hello() {
    let harness = start_harness().await;
    let client = connect(&harness).await;
    client
        .send_fragmented_message(
            &ClientMessage::Hello(ClientHello {
                version: u64::from(PROTOCOL_VERSION),
            }),
            2,
        )
        .await
        .expect("write");
    let hello = client
        .next(|message| matches!(message, ServerMessage::Hello(_)))
        .await;
    assert!(matches!(hello, ServerMessage::Hello(hello) if hello.version == PROTOCOL_VERSION));
    harness.server.close().await;
}

#[tokio::test]
async fn enforces_version_and_exactly_one_first_message_hello() {
    let harness = start_harness().await;

    let bad_version = connect(&harness).await;
    let response = bad_version
        .hello_with_version(u64::from(PROTOCOL_VERSION) + 1)
        .await;
    assert_eq!(error_code(&response), Some(ProtocolErrorCode::Version));
    bad_version.wait_for_close().await;

    let request_first = connect(&harness).await;
    request_first
        .send_message(&ClientMessage::Request(pi_protocol::RequestEnvelope {
            id: "too-early".to_string(),
            request: Command::List(ListCommand {}),
        }))
        .await
        .expect("write");
    let response = request_first
        .next(|message| matches!(message, ServerMessage::HelloError(_)))
        .await;
    assert_eq!(
        error_code(&response),
        Some(ProtocolErrorCode::InvalidRequest)
    );
    request_first.wait_for_close().await;

    let duplicate = connect(&harness).await;
    assert!(matches!(duplicate.hello().await, ServerMessage::Hello(_)));
    duplicate
        .send_message(&ClientMessage::Hello(ClientHello {
            version: u64::from(PROTOCOL_VERSION),
        }))
        .await
        .expect("write");
    let response = duplicate
        .next(|message| matches!(message, ServerMessage::HelloError(_)))
        .await;
    assert_eq!(
        error_code(&response),
        Some(ProtocolErrorCode::InvalidRequest)
    );
    duplicate.wait_for_close().await;

    harness.server.close().await;
}

#[tokio::test]
async fn closes_connections_that_do_not_complete_hello_before_the_timeout() {
    let harness = start_harness_with(TestServerService::new(), |options| {
        options.with_handshake_timeout_ms(20)
    })
    .await;
    let client = connect(&harness).await;
    client.wait_for_close().await;
    assert!(client
        .messages()
        .iter()
        .any(|message| error_code(message) == Some(ProtocolErrorCode::InvalidRequest)));
    harness.server.close().await;
}

#[tokio::test]
async fn keeps_the_handshake_timeout_active_until_the_server_hello_is_sent() {
    let service = TestServerService::new();
    let (entered, release) = service.delay_next_list();
    let harness =
        start_harness_with(service, |options| options.with_handshake_timeout_ms(20)).await;
    let client = connect(&harness).await;
    client
        .send_message(&ClientMessage::Hello(ClientHello {
            version: u64::from(PROTOCOL_VERSION),
        }))
        .await
        .expect("write");
    entered.wait().await;
    client.wait_for_close().await;
    release.resolve(());
    assert!(client
        .messages()
        .iter()
        .any(|message| error_code(message) == Some(ProtocolErrorCode::InvalidRequest)));
    harness.server.close().await;
}

#[tokio::test]
async fn bounds_and_closes_malformed_or_oversized_frames() {
    let malformed_harness = start_harness().await;
    let malformed = connect(&malformed_harness).await;
    malformed
        .send_bytes(&encode_frame(&[0xff]).expect("frame"))
        .await
        .expect("write");
    let response = malformed
        .next(|message| matches!(message, ServerMessage::HelloError(_)))
        .await;
    assert_eq!(
        error_code(&response),
        Some(ProtocolErrorCode::InvalidRequest)
    );
    malformed.wait_for_close().await;
    malformed_harness.server.close().await;

    let bounded_harness = start_harness_with(TestServerService::new(), |options| {
        options
            .with_max_frame_length(128)
            .with_max_pending_bytes(512)
    })
    .await;
    let oversized = connect(&bounded_harness).await;
    let mut frame = vec![0u8; 4 + 129];
    frame[3] = 129;
    oversized.send_bytes(&frame).await.expect("write");
    oversized.wait_for_close().await;
    assert!(!oversized
        .messages()
        .iter()
        .any(|message| matches!(message, ServerMessage::Hello(_))));
    bounded_harness.server.close().await;

    // A server hello that will not fit the configured limit is dropped, and the
    // connection closes without ever completing the handshake.
    let outbound_harness = start_harness_with(TestServerService::new(), |options| {
        options
            .with_max_frame_length(16)
            .with_max_pending_bytes(512)
    })
    .await;
    let outbound = connect(&outbound_harness).await;
    outbound
        .send_message(&ClientMessage::Hello(ClientHello {
            version: u64::from(PROTOCOL_VERSION),
        }))
        .await
        .expect("write");
    outbound.wait_for_close().await;
    assert!(outbound.messages().is_empty());
    outbound_harness.server.close().await;
}

#[tokio::test]
async fn catches_up_a_handshaking_client_after_a_concurrent_server_change() {
    let service = TestServerService::new();
    service.seed("shared");
    let entered = Arc::new(Deferred::<()>::new());
    let release = Arc::new(Deferred::<()>::new());
    let racing = Arc::new(parking_lot::Mutex::new(false));
    {
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        let racing = Arc::clone(&racing);
        service.set_list_sessions_hook(Arc::new(move |metadata| {
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            let race = *racing.lock();
            Box::pin(async move {
                if race {
                    entered.resolve(());
                    release.wait().await;
                }
                Ok(metadata)
            })
        }));
    }
    let harness = start_harness_with(Arc::clone(&service), |options| options).await;

    let controller = connect(&harness).await;
    controller.hello().await;
    *racing.lock() = true;

    let joining = connect(&harness).await;
    let joining_handshake = {
        let joining = Arc::clone(&joining);
        tokio::spawn(async move { joining.hello().await })
    };
    entered.wait().await;
    controller
        .request(Command::Attach(SessionCommand::new("shared")))
        .await;
    release.resolve(());

    let handshake = joining_handshake.await.expect("join");
    let ServerMessage::Hello(hello) = handshake else {
        panic!("expected a server hello");
    };
    let catchup = joining
        .next(|message| match message {
            ServerMessage::Event(envelope) => match &envelope.event {
                ServerEvent::ServerSnapshot(event) => {
                    event.snapshot.revision > hello.snapshot.revision
                }
                _ => false,
            },
            _ => false,
        })
        .await;
    let ServerMessage::Event(envelope) = catchup else {
        panic!("expected an event");
    };
    let ServerEvent::ServerSnapshot(event) = envelope.event else {
        panic!("expected a server snapshot");
    };
    assert_eq!(event.snapshot.sessions[0].id, "shared");
    assert_eq!(
        event.snapshot.sessions[0].session_name.as_deref(),
        Some("Session shared")
    );

    harness.server.close().await;
}

#[tokio::test]
async fn does_not_expose_unexpected_service_errors_to_clients() {
    let service = TestServerService::new();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    service.set_list_sessions_hook(Arc::new(move |metadata| {
        let failing = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0;
        Box::pin(async move {
            if failing {
                Err(PiServerError::internal("private service detail"))
            } else {
                Ok(metadata)
            }
        })
    }));
    let harness = start_harness_with(service, |options| options).await;
    let client = connect(&harness).await;
    client.hello().await;

    let response = client.request(Command::List(ListCommand {})).await;
    assert!(!response.ok);
    let error = response.error.expect("error");
    assert_eq!(error.code, ProtocolErrorCode::InternalError);
    assert_eq!(error.message, "Internal server error");
    assert!(
        harness
            .errors
            .lock()
            .iter()
            .any(|report| report.to_string() == "private service detail"),
        "the private cause is reported to the observer, never to the client"
    );

    harness.server.close().await;
}

#[tokio::test]
async fn keeps_not_implemented_stable() {
    let service = TestServerService::new();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    service.set_list_sessions_hook(Arc::new(move |metadata| {
        let failing = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0;
        Box::pin(async move {
            if failing {
                Err(PiServerError::NotImplemented)
            } else {
                Ok(metadata)
            }
        })
    }));
    let harness = start_harness_with(service, |options| options).await;
    let client = connect(&harness).await;
    client.hello().await;

    let response = client.request(Command::List(ListCommand {})).await;
    let error = response.error.expect("error");
    assert_eq!(error.code, ProtocolErrorCode::NotImplemented);
    assert_eq!(error.message, "Operation is not implemented");
    harness.server.close().await;
}

#[tokio::test]
async fn shares_request_event_attachment_and_disconnect_behavior() {
    let service = TestServerService::new();
    service.seed("first");
    service.seed("second");
    let harness = start_harness_with(Arc::clone(&service), |options| options).await;
    let client = connect(&harness).await;

    let ServerMessage::Hello(hello) = client.hello().await else {
        panic!("expected a server hello");
    };
    assert_eq!(
        hello
            .snapshot
            .sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );

    let listed = client.request(Command::List(ListCommand {})).await;
    let Some(pi_protocol::CommandResult::List(list)) = listed.result else {
        panic!("expected a list result");
    };
    assert_eq!(list.sessions.len(), 2);

    for id in ["first", "second"] {
        let attached = client
            .request(Command::Attach(SessionCommand::new(id)))
            .await;
        let Some(pi_protocol::CommandResult::Attach(result)) = attached.result else {
            panic!("expected an attach result");
        };
        assert_eq!(result.session.id, id);
        assert!(result.session.attached);
    }

    let progress = pi_protocol::TranscriptProgress::AssistantDelta(pi_protocol::AssistantDelta {
        message_id: "assistant-1".to_string(),
        content_index: 0,
        kind: pi_protocol::AssistantDeltaKind::Text,
        delta: "hello".to_string(),
    });
    service
        .latest_runtime("first")
        .emit_progress(progress.clone());
    let event = client
        .next(|message| {
            matches!(
                message,
                ServerMessage::Event(envelope)
                    if matches!(envelope.event, ServerEvent::SessionProgress(_))
            )
        })
        .await;
    let ServerMessage::Event(envelope) = event else {
        panic!("expected an event");
    };
    let ServerEvent::SessionProgress(event) = envelope.event else {
        panic!("expected session progress");
    };
    assert_eq!(event.session_id, "first");
    assert_eq!(event.progress, progress);

    let detached = client
        .request(Command::Detach(SessionCommand::new("first")))
        .await;
    assert!(detached.ok);
    assert_eq!(service.latest_runtime("first").dispose_count(), 1);

    let thinking = client
        .request(Command::SetThinking(pi_protocol::SetThinkingCommand {
            session_id: "second".to_string(),
            thinking_level: pi_protocol::ThinkingLevel::High,
        }))
        .await;
    let Some(pi_protocol::CommandResult::SetThinking(result)) = thinking.result else {
        panic!("expected a set_thinking result");
    };
    assert_eq!(
        result.session.thinking_level,
        pi_protocol::ThinkingLevel::High
    );

    let second_runtime = service.latest_runtime("second");
    client.close().await;
    second_runtime.disposed.wait().await;
    assert_eq!(second_runtime.dispose_count(), 1);

    harness.server.close().await;
}

#[tokio::test]
async fn disconnects_attached_clients_when_a_runtime_reports_a_terminal_error() {
    let service = TestServerService::new();
    service.seed("terminal");
    let harness = start_harness_with(Arc::clone(&service), |options| options).await;
    let client = connect(&harness).await;
    client.hello().await;
    client
        .request(Command::Attach(SessionCommand::new("terminal")))
        .await;
    let runtime = service.latest_runtime("terminal");

    runtime.set_phase(pi_protocol::SessionPhase::Turn);
    runtime.emit_error(PiServerError::session_locked("lock ownership lost"));

    client.wait_for_close().await;
    runtime.disposed.wait().await;
    assert_eq!(runtime.dispose_count(), 1);
    assert!(!service.is_locked("terminal"));
    assert!(harness.errors.lock().iter().any(|report| matches!(
        report,
        ServerErrorReport::Service(PiServerError::SessionLocked { .. })
    )));

    let next_client = connect(&harness).await;
    next_client.hello().await;
    let attached = next_client
        .request(Command::Attach(SessionCommand::new("terminal")))
        .await;
    assert!(attached.ok);
    assert_eq!(service.runtime_count("terminal"), 2);

    harness.server.close().await;
}

#[tokio::test]
async fn can_respond_out_of_request_order_after_the_handshake() {
    let service = TestServerService::new();
    service.seed("first");
    let harness = start_harness_with(Arc::clone(&service), |options| options).await;
    let client = connect(&harness).await;
    client.hello().await;

    let (entered, release) = service.delay_next_list();
    let slow = {
        let client = Arc::clone(&client);
        tokio::spawn(async move {
            client
                .request_with_id(Command::List(ListCommand {}), "slow")
                .await
        })
    };
    entered.wait().await;
    let fast = client
        .request_with_id(Command::Attach(SessionCommand::new("first")), "fast")
        .await;
    assert!(fast.ok);
    assert!(!client.messages().iter().any(|message| matches!(
        message,
        ServerMessage::Response(envelope) if envelope.id == "slow"
    )));

    release.resolve(());
    let slow = slow.await.expect("join");
    assert!(slow.ok);
    let order: Vec<String> = client
        .messages()
        .iter()
        .filter_map(|message| match message {
            ServerMessage::Response(envelope) if envelope.id == "slow" || envelope.id == "fast" => {
                Some(envelope.id.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(order, vec!["fast".to_string(), "slow".to_string()]);

    harness.server.close().await;
}

#[tokio::test]
async fn gracefully_closes_connections_sessions_and_listener_resources() {
    let service = TestServerService::new();
    service.seed("first");
    let harness = start_harness_with(Arc::clone(&service), |options| options).await;
    let socket_path = harness.socket_path();
    let client = connect(&harness).await;
    client.hello().await;
    client
        .request(Command::Attach(SessionCommand::new("first")))
        .await;
    let runtime = service.latest_runtime("first");

    harness.server.close().await;
    client.wait_for_close().await;
    assert_eq!(runtime.dispose_count(), 1);
    assert!(harness.server.addresses().is_empty());
    assert!(!std::path::Path::new(&socket_path).exists());
    // Closing twice is a no-op.
    harness.server.close().await;
}

#[tokio::test]
async fn decodes_multiple_framed_requests_from_one_raw_chunk() {
    let harness = start_harness().await;
    let client = connect(&harness).await;
    client.hello().await;

    let mut combined = Vec::new();
    for id in ["first", "second"] {
        combined.extend_from_slice(
            &encode_client_message(
                &ClientMessage::Request(pi_protocol::RequestEnvelope {
                    id: id.to_string(),
                    request: Command::List(ListCommand {}),
                }),
                FrameDecoderOptions::default(),
            )
            .expect("encode"),
        );
    }
    client.send_bytes(&combined).await.expect("write");

    for id in ["first", "second"] {
        let response = client
            .next(
                |message| matches!(message, ServerMessage::Response(envelope) if envelope.id == id),
            )
            .await;
        assert!(matches!(response, ServerMessage::Response(envelope) if envelope.ok));
    }
    harness.server.close().await;
}
