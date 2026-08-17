//! Port of `.upstream/packages/client/test/connection.test.ts`.

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_client::{
    ByteTransport, ByteTransportFactory, ByteTransportHandlers, ConnectFuture, ConnectionState,
    ListenerError, PiClient, PiClientError, PiClientOptions, SendFuture, TransportError,
};
use pi_protocol::{
    encode_cbor, encode_frame, encode_server_message, CborValue, ClientMessage,
    FrameDecoderOptions, ProtocolError, ProtocolErrorCode, ServerHello, ServerHelloError,
    ServerMessage, PROTOCOL_VERSION,
};
use support::{
    attach_session, base_server_snapshot, connect_client, create_client, find_request, make_server,
    session_snapshot, MemoryFactory,
};

/// Keeps a deliberately panicking listener from spamming the test output.
fn quiet_panics() {
    std::panic::set_hook(Box::new(|_| {}));
}

#[tokio::test]
async fn sends_a_framed_version_before_accepting_a_fragmented_server_hello() {
    let server = make_server();
    let received: Arc<Mutex<Vec<ClientMessage>>> = Arc::default();
    let sink = Arc::clone(&received);
    server.on_message(move |server, message| {
        sink.lock().unwrap().push(message.clone());
        if matches!(message, ClientMessage::Hello(_)) {
            server.send_split(
                &ServerMessage::Hello(ServerHello {
                    version: PROTOCOL_VERSION,
                    connection_id: "connection-1".to_string(),
                    snapshot: base_server_snapshot(),
                }),
                3,
            );
        }
    });
    let client = create_client(&server);

    assert_eq!(
        client.connect().await.expect("handshake"),
        base_server_snapshot()
    );
    assert_eq!(
        received.lock().unwrap()[0],
        ClientMessage::Hello(pi_protocol::ClientHello {
            version: u64::from(PROTOCOL_VERSION)
        })
    );
    assert_eq!(client.connection_state(), ConnectionState::Connected);
}

/// A factory that delivers server bytes before it returns the transport.
struct EagerFactory {
    sends: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
}

struct CountingTransport {
    sends: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
}

impl ByteTransport for CountingTransport {
    fn send(&self, _chunk: Vec<u8>) -> SendFuture {
        self.sends.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }

    fn close(&self) {
        self.closes.fetch_add(1, Ordering::SeqCst);
    }
}

impl ByteTransportFactory for EagerFactory {
    fn connect(&self, handlers: Arc<dyn ByteTransportHandlers>) -> ConnectFuture {
        let frame = encode_server_message(
            &ServerMessage::Hello(ServerHello {
                version: PROTOCOL_VERSION,
                connection_id: "connection-1".to_string(),
                snapshot: base_server_snapshot(),
            }),
            FrameDecoderOptions::default(),
        )
        .expect("encodes");
        handlers.on_data(&frame);
        let sends = Arc::clone(&self.sends);
        let closes = Arc::clone(&self.closes);
        Box::pin(async move {
            Ok(Arc::new(CountingTransport { sends, closes }) as Arc<dyn ByteTransport>)
        })
    }
}

#[tokio::test]
async fn rejects_server_data_delivered_before_sending_the_client_hello() {
    let sends = Arc::new(AtomicUsize::new(0));
    let closes = Arc::new(AtomicUsize::new(0));
    let factory = Arc::new(EagerFactory {
        sends: Arc::clone(&sends),
        closes: Arc::clone(&closes),
    });
    let client = PiClient::new(PiClientOptions::new(factory)).expect("client");

    let error = client.connect().await.expect_err("rejects early data");
    assert_eq!(
        error,
        PiClientError::ProtocolViolation(
            "Received server data before the client hello was sent".to_string()
        )
    );
    assert_eq!(client.connection_state(), ConnectionState::Disconnected);
    assert_eq!(sends.load(Ordering::SeqCst), 0);
    assert_eq!(closes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn isolates_subscriber_failures_from_handshake_and_transport_state() {
    quiet_panics();
    let server = make_server();
    server.auto_hello(base_server_snapshot());
    let client = create_client(&server);
    client
        .subscribe(Arc::new(|_| panic!("consumer failure")))
        .expect("subscribe");

    assert_eq!(
        client.connect().await.expect("handshake"),
        base_server_snapshot()
    );
    assert_eq!(client.connection_state(), ConnectionState::Connected);
    let _ = std::panic::take_hook();
}

#[tokio::test]
async fn reports_subscriber_failures_without_changing_connection_state() {
    quiet_panics();
    let server = make_server();
    server.auto_hello(base_server_snapshot());
    let errors: Arc<Mutex<Vec<ListenerError>>> = Arc::default();
    let sink = Arc::clone(&errors);
    let client = PiClient::new(
        PiClientOptions::new(MemoryFactory::single(&server) as Arc<dyn ByteTransportFactory>)
            .with_listener_error_handler(Arc::new(move |error| sink.lock().unwrap().push(error))),
    )
    .expect("client");
    client
        .subscribe(Arc::new(|_| panic!("consumer failure")))
        .expect("subscribe");

    client.connect().await.expect("handshake");
    assert_eq!(
        errors
            .lock()
            .unwrap()
            .iter()
            .map(|error| error.message.clone())
            .collect::<Vec<_>>(),
        vec!["consumer failure".to_string()]
    );
    assert_eq!(client.connection_state(), ConnectionState::Connected);
    let _ = std::panic::take_hook();
}

#[tokio::test]
async fn does_not_restore_a_connection_after_a_snapshot_listener_disconnects() {
    let server = make_server();
    server.auto_hello(base_server_snapshot());
    let client = create_client(&server);
    let disconnecting = client.clone();
    client
        .subscribe(Arc::new(move |_| disconnecting.disconnect("listener")))
        .expect("subscribe");

    let error = client.connect().await.expect_err("handshake is abandoned");
    assert!(matches!(error, PiClientError::Disconnected(_)));
    assert_eq!(client.connection_state(), ConnectionState::Disconnected);
    assert_eq!(server.close_count(), 1);
}

#[tokio::test]
async fn rejects_a_typed_handshake_version_error() {
    let server = make_server();
    server.on_message(|server, _| {
        server.send(&ServerMessage::HelloError(ServerHelloError {
            error: ProtocolError::new(ProtocolErrorCode::Version, "Unsupported protocol version"),
        }));
    });
    let client = create_client(&server);

    let error = client.connect().await.expect_err("version mismatch");
    assert_eq!(error.server_code(), Some(ProtocolErrorCode::Version));
    assert_eq!(error.to_string(), "Unsupported protocol version");
    assert_eq!(client.connection_state(), ConnectionState::Disconnected);
    assert_eq!(server.close_count(), 1);
}

#[tokio::test]
async fn rejects_pending_requests_on_close_and_reconnects_through_a_fresh_transport() {
    let first = make_server();
    let second = make_server();
    for (index, server) in [&first, &second].into_iter().enumerate() {
        let mut snapshot = base_server_snapshot();
        snapshot.revision = index as u64 + 1;
        server.auto_hello(snapshot);
    }
    let factory = MemoryFactory::sequence(vec![Arc::clone(&first), Arc::clone(&second)]);
    let client = PiClient::new(PiClientOptions::new(
        factory as Arc<dyn ByteTransportFactory>,
    ))
    .expect("client");
    let states: Arc<Mutex<Vec<ConnectionState>>> = Arc::default();
    let sink = Arc::clone(&states);
    client
        .on_connection_state_change(Arc::new(move |change| {
            sink.lock().unwrap().push(change.state)
        }))
        .expect("subscribe");
    client.connect().await.expect("handshake");

    let pending = {
        let client = client.clone();
        tokio::spawn(async move { client.list_sessions().await })
    };
    tokio::task::yield_now().await;
    first.close();
    let error = pending.await.expect("join").expect_err("disconnected");
    assert!(matches!(error, PiClientError::Disconnected(_)));
    assert_eq!(client.connection_state(), ConnectionState::Disconnected);

    let snapshot = client.reconnect().await.expect("reconnects");
    assert_eq!(snapshot.revision, 2);
    assert_eq!(client.connection_state(), ConnectionState::Connected);
    assert_eq!(
        *states.lock().unwrap(),
        vec![
            ConnectionState::Connecting,
            ConnectionState::Connected,
            ConnectionState::Disconnected,
            ConnectionState::Connecting,
            ConnectionState::Connected,
        ]
    );
}

#[tokio::test]
async fn rejects_pending_requests_on_transport_errors() {
    let server = make_server();
    let client = connect_client(&server).await;
    let pending = {
        let client = client.clone();
        tokio::spawn(async move { client.list_sessions().await })
    };
    tokio::task::yield_now().await;
    server.error("read failed");

    let error = pending.await.expect("join").expect_err("transport failed");
    assert_eq!(
        error,
        PiClientError::Disconnected("read failed".to_string())
    );
    assert_eq!(client.connection_state(), ConnectionState::Disconnected);
}

#[tokio::test]
async fn enforces_the_configured_frame_limit_in_both_directions() {
    let server = make_server();
    server.auto_hello(base_server_snapshot());
    let client = PiClient::new(
        PiClientOptions::new(MemoryFactory::single(&server) as Arc<dyn ByteTransportFactory>)
            .with_max_frame_length(512),
    )
    .expect("client");
    client.connect().await.expect("handshake");
    let handle = attach_session(&client, &server, session_snapshot("session-1")).await;

    let sent_before = server.sent_count();
    let error = handle
        .prompt("x".repeat(1_000))
        .await
        .expect_err("outbound frame is too large");
    assert!(matches!(error, PiClientError::Protocol(_)));
    assert_eq!(server.sent_count(), sent_before, "nothing was written");

    // An inbound length prefix over the limit is a terminal framing failure.
    server.send_raw(&[0, 0, 2, 1]);
    assert_eq!(client.connection_state(), ConnectionState::Disconnected);
}

#[tokio::test]
async fn disconnects_on_invalid_protocol_data() {
    let server = make_server();
    let client = connect_client(&server).await;

    let mut event = pi_protocol::CborMap::new();
    event.insert(
        "type".to_string(),
        CborValue::Text("session_removed".into()),
    );
    event.insert("sessionId".to_string(), CborValue::Integer(1));
    let mut envelope = pi_protocol::CborMap::new();
    envelope.insert("type".to_string(), CborValue::Text("event".into()));
    envelope.insert("event".to_string(), CborValue::Map(event));
    let payload = encode_cbor(&CborValue::Map(envelope), Default::default()).expect("cbor");
    server.send_raw(&encode_frame(&payload).expect("frame"));

    assert_eq!(client.connection_state(), ConnectionState::Disconnected);
}

#[tokio::test]
async fn reports_truncated_framing_when_the_transport_closes() {
    let server = make_server();
    let client = connect_client(&server).await;
    let pending = {
        let client = client.clone();
        tokio::spawn(async move { client.list_sessions().await })
    };
    tokio::task::yield_now().await;
    server.send_raw(&[0, 0, 0, 2, 1]);
    server.close();

    let error = pending.await.expect("join").expect_err("truncated");
    assert!(
        error.to_string().to_lowercase().contains("truncated"),
        "unexpected error: {error}"
    );
    assert_eq!(client.connection_state(), ConnectionState::Disconnected);
}

#[tokio::test]
async fn rejects_a_zero_frame_limit() {
    let server = make_server();
    let error = PiClient::new(
        PiClientOptions::new(MemoryFactory::single(&server) as Arc<dyn ByteTransportFactory>)
            .with_max_frame_length(0),
    )
    .expect_err("rejects the option");
    assert!(error.to_string().contains("maxFrameLength"));
}

#[tokio::test]
async fn refuses_to_connect_twice() {
    let server = make_server();
    let client = connect_client(&server).await;
    let error = client.connect().await.expect_err("already connected");
    assert_eq!(
        error,
        PiClientError::Disconnected("PiClient is already connected".to_string())
    );
}

/// The one upstream case Rust cannot express verbatim: a synchronous listener
/// cannot await, so the reconnect is spawned instead.
#[tokio::test]
async fn supports_reconnecting_from_a_disconnection_listener() {
    let first = make_server();
    let second = make_server();
    for (index, server) in [&first, &second].into_iter().enumerate() {
        let mut snapshot = base_server_snapshot();
        snapshot.revision = index as u64 + 1;
        server.auto_hello(snapshot);
    }
    let factory = MemoryFactory::sequence(vec![Arc::clone(&first), Arc::clone(&second)]);
    let client = PiClient::new(PiClientOptions::new(
        factory as Arc<dyn ByteTransportFactory>,
    ))
    .expect("client");
    client.connect().await.expect("handshake");

    let reconnecting = client.clone();
    let done: Arc<Mutex<Option<tokio::task::JoinHandle<Result<_, PiClientError>>>>> =
        Arc::default();
    let slot = Arc::clone(&done);
    client
        .on_connection_state_change(Arc::new(move |change| {
            if change.state == ConnectionState::Disconnected {
                let client = reconnecting.clone();
                *slot.lock().unwrap() = Some(tokio::spawn(async move { client.reconnect().await }));
            }
        }))
        .expect("subscribe");

    first.close();
    let handle = done.lock().unwrap().take().expect("reconnect scheduled");
    let snapshot = handle.await.expect("join").expect("reconnects");
    assert_eq!(snapshot.revision, 2);
    assert_eq!(client.connection_state(), ConnectionState::Connected);
}

#[tokio::test]
async fn surfaces_a_typed_request_error() {
    let server = make_server();
    let client = connect_client(&server).await;
    let requests = support::collect_requests(&server);
    let attaching = {
        let client = client.clone();
        tokio::spawn(async move { client.attach_session("locked").await })
    };
    tokio::task::yield_now().await;
    let request = find_request(&requests, "attach");
    server.send(&ServerMessage::Response(
        pi_protocol::ResponseEnvelope::failed(
            request.id,
            ProtocolError::new(ProtocolErrorCode::SessionLocked, "Already attached"),
        ),
    ));

    let error = attaching.await.expect("join").expect_err("locked");
    assert_eq!(error.server_code(), Some(ProtocolErrorCode::SessionLocked));
}

#[tokio::test]
async fn rejects_a_mismatched_response_instead_of_leaving_its_request_pending() {
    let server = make_server();
    let client = connect_client(&server).await;
    let requests = support::collect_requests(&server);
    let listing = {
        let client = client.clone();
        tokio::spawn(async move { client.list_sessions().await })
    };
    tokio::task::yield_now().await;
    let request = find_request(&requests, "list");
    server.send(&ServerMessage::Response(pi_protocol::ResponseEnvelope::ok(
        request.id,
        support::attach_result(session_snapshot("session-1")),
    )));

    let error = listing.await.expect("join").expect_err("mismatched");
    assert_eq!(
        error,
        PiClientError::ProtocolViolation("Response command attach does not match list".to_string())
    );
    assert_eq!(client.connection_state(), ConnectionState::Disconnected);
}

#[tokio::test]
async fn correlates_coalesced_out_of_order_responses() {
    let server = make_server();
    let client = connect_client(&server).await;
    let requests = support::collect_requests(&server);

    let listing = {
        let client = client.clone();
        tokio::spawn(async move { client.list_sessions().await })
    };
    let attaching = {
        let client = client.clone();
        tokio::spawn(async move { client.attach_session("session-1").await })
    };
    while requests.lock().unwrap().len() < 2 {
        tokio::task::yield_now().await;
    }

    let attach = find_request(&requests, "attach");
    let list = find_request(&requests, "list");
    server.send_together(&[
        ServerMessage::Response(pi_protocol::ResponseEnvelope::ok(
            attach.id,
            support::attach_result(session_snapshot("session-1")),
        )),
        ServerMessage::Response(pi_protocol::ResponseEnvelope::ok(
            list.id,
            pi_protocol::CommandResult::List(pi_protocol::ListResult {
                sessions: Vec::new(),
            }),
        )),
    ]);

    assert!(listing.await.expect("join").expect("list").is_empty());
    let handle = attaching.await.expect("join").expect("attach");
    assert_eq!(handle.id(), "session-1");
    assert!(handle.attached());
}

#[tokio::test]
async fn a_disposed_client_refuses_everything() {
    let server = make_server();
    let client = connect_client(&server).await;
    let handle = attach_session(&client, &server, session_snapshot("session-1")).await;

    client.dispose().await;
    client.dispose().await;

    assert!(client.disposed());
    assert!(!client.connected());
    assert!(!handle.attached());
    assert_eq!(
        client.list_sessions().await.expect_err("disposed"),
        PiClientError::Disposed
    );
    assert_eq!(
        handle.prompt("after disposal").await.expect_err("disposed"),
        PiClientError::Disposed
    );
}

#[tokio::test]
async fn connect_new_disposes_a_client_whose_handshake_fails() {
    let server = make_server();
    server.on_message(|server, _| {
        server.send(&ServerMessage::HelloError(ServerHelloError {
            error: ProtocolError::new(ProtocolErrorCode::Version, "nope"),
        }));
    });
    let error = PiClient::connect_new(PiClientOptions::new(
        MemoryFactory::single(&server) as Arc<dyn ByteTransportFactory>
    ))
    .await
    .expect_err("handshake fails");
    assert_eq!(error.server_code(), Some(ProtocolErrorCode::Version));
}

#[tokio::test]
async fn transport_errors_are_reported_as_disconnections() {
    let error = PiClientError::from(TransportError::new("boom"));
    assert_eq!(error, PiClientError::Disconnected("boom".to_string()));
    assert_eq!(error.code(), "disconnected");
}
