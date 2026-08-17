//! Port of `.upstream/packages/client/test/unix.test.ts`.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_client::{
    ByteTransportFactory, ByteTransportHandlers, ConnectionState, PiClient, PiClientOptions,
    TransportError, UnixTransportFactory, UnixTransportOptions,
};
use pi_protocol::{
    encode_server_message, ClientMessage, ClientMessageDecoder, CommandResult, FrameDecoderOptions,
    ListResult, ResponseEnvelope, ServerHello, ServerMessage, ServerSnapshot, PROTOCOL_VERSION,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

fn temp_socket() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::Builder::new()
        .prefix("pic")
        .tempdir_in("/tmp")
        .expect("temp dir");
    let path = directory.path().join("p.sock");
    (directory, path)
}

fn server_snapshot() -> ServerSnapshot {
    ServerSnapshot {
        server_id: "unix-server".to_string(),
        protocol_version: PROTOCOL_VERSION,
        revision: 4,
        sessions: Vec::new(),
        models: Vec::new(),
    }
}

#[test]
fn rejects_invalid_unix_transport_options() {
    let empty = UnixTransportFactory::new(UnixTransportOptions::new(""))
        .expect_err("empty path is refused");
    assert!(empty.to_string().contains("must not be empty"));

    let long = UnixTransportFactory::new(UnixTransportOptions::new(format!(
        "/tmp/{}",
        "x".repeat(512)
    )))
    .expect_err("long path is refused");
    assert!(long.to_string().contains("too long"));

    let zero = UnixTransportFactory::new(UnixTransportOptions {
        path: PathBuf::from("/tmp/pi.sock"),
        max_pending_bytes: Some(0),
    })
    .expect_err("zero budget is refused");
    assert!(zero.to_string().contains("positive"));
}

/// A byte-at-a-time echo server: every server frame is written one byte per
/// write so the client must reassemble across chunk boundaries.
async fn spawn_fragmenting_server(path: PathBuf) -> tokio::task::JoinHandle<()> {
    let listener = UnixListener::bind(&path).expect("bind");
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut read_half, mut write_half) = socket.into_split();
                let mut decoder = ClientMessageDecoder::default();
                let mut buffer = vec![0u8; 8192];
                loop {
                    let Ok(count) = read_half.read(&mut buffer).await else {
                        return;
                    };
                    if count == 0 {
                        return;
                    }
                    let Ok(messages) = decoder.push(&buffer[..count]) else {
                        return;
                    };
                    for message in messages {
                        match message {
                            ClientMessage::Hello(_) => {
                                let hello = encode_server_message(
                                    &ServerMessage::Hello(ServerHello {
                                        version: PROTOCOL_VERSION,
                                        connection_id: "unix-connection".to_string(),
                                        snapshot: server_snapshot(),
                                    }),
                                    FrameDecoderOptions::default(),
                                )
                                .expect("encode");
                                for byte in hello {
                                    if write_half.write_all(&[byte]).await.is_err() {
                                        return;
                                    }
                                }
                            }
                            ClientMessage::Request(envelope) => {
                                let response = encode_server_message(
                                    &ServerMessage::Response(ResponseEnvelope::ok(
                                        envelope.id,
                                        CommandResult::List(ListResult {
                                            sessions: Vec::new(),
                                        }),
                                    )),
                                    FrameDecoderOptions::default(),
                                )
                                .expect("encode");
                                let split = response.len() / 2;
                                if write_half.write_all(&response[..split]).await.is_err() {
                                    return;
                                }
                                if write_half.write_all(&response[split..]).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                }
            });
        }
    })
}

#[tokio::test]
async fn exchanges_fragmented_framed_messages_over_a_real_unix_socket() {
    let (_directory, path) = temp_socket();
    let server = spawn_fragmenting_server(path.clone()).await;

    let factory =
        UnixTransportFactory::new(UnixTransportOptions::new(&path)).expect("transport options");
    let client = PiClient::new(PiClientOptions::new(factory)).expect("client");

    assert_eq!(
        client.connect().await.expect("handshake"),
        server_snapshot()
    );
    let (first, second) = tokio::join!(client.list_sessions(), client.list_sessions());
    assert!(first.expect("list").is_empty());
    assert!(second.expect("list").is_empty());

    client.disconnect("done");
    server.abort();
}

#[tokio::test]
async fn rejects_a_truncated_final_frame_from_a_real_unix_socket() {
    let (_directory, path) = temp_socket();
    let listener = UnixListener::bind(&path).expect("bind");
    let server = tokio::spawn(async move {
        let Ok((socket, _)) = listener.accept().await else {
            return;
        };
        let (mut read_half, mut write_half) = socket.into_split();
        let mut decoder = ClientMessageDecoder::default();
        let mut buffer = vec![0u8; 8192];
        loop {
            let Ok(count) = read_half.read(&mut buffer).await else {
                return;
            };
            if count == 0 {
                return;
            }
            let Ok(messages) = decoder.push(&buffer[..count]) else {
                return;
            };
            for message in messages {
                match message {
                    ClientMessage::Hello(_) => {
                        let hello = encode_server_message(
                            &ServerMessage::Hello(ServerHello {
                                version: PROTOCOL_VERSION,
                                connection_id: "unix-truncated".to_string(),
                                snapshot: server_snapshot(),
                            }),
                            FrameDecoderOptions::default(),
                        )
                        .expect("encode");
                        let _ = write_half.write_all(&hello).await;
                    }
                    ClientMessage::Request(_) => {
                        // A length prefix promising two bytes, then one byte
                        // and EOF.
                        let _ = write_half.write_all(&[0, 0, 0, 2, 1]).await;
                        let _ = write_half.shutdown().await;
                        return;
                    }
                }
            }
        }
    });

    let factory =
        UnixTransportFactory::new(UnixTransportOptions::new(&path)).expect("transport options");
    let client = PiClient::new(PiClientOptions::new(factory)).expect("client");
    client.connect().await.expect("handshake");

    let error = client.list_sessions().await.expect_err("truncated frame");
    assert!(
        error.to_string().to_lowercase().contains("truncated"),
        "unexpected error: {error}"
    );
    assert_eq!(client.connection_state(), ConnectionState::Disconnected);
    server.abort();
}

#[tokio::test]
async fn rejects_connection_errors() {
    let (_directory, path) = temp_socket();
    let factory = UnixTransportFactory::new(UnixTransportOptions::new(path.join("missing.sock")))
        .expect("transport options");
    let client = PiClient::new(PiClientOptions::new(factory)).expect("client");
    let error = client.connect().await.expect_err("no such socket");
    assert!(error.is_disconnected(), "unexpected error: {error}");
}

struct RecordingHandlers {
    inbound: Arc<Mutex<Vec<u8>>>,
    closes: Arc<AtomicUsize>,
    errors: Arc<Mutex<Vec<String>>>,
}

impl ByteTransportHandlers for RecordingHandlers {
    fn on_data(&self, chunk: &[u8]) {
        self.inbound.lock().unwrap().extend_from_slice(chunk);
    }

    fn on_close(&self) {
        self.closes.fetch_add(1, Ordering::SeqCst);
    }

    fn on_error(&self, error: TransportError) {
        self.errors.lock().unwrap().push(error.message);
    }
}

#[tokio::test]
async fn bounds_pending_writes_preserves_order_and_reports_the_remote_end_once() {
    let (_directory, path) = temp_socket();
    let listener = UnixListener::bind(&path).expect("bind");

    const CHUNK: usize = 2 * 1024 * 1024;
    let expected_length = CHUNK * 2;
    let resume = Arc::new(tokio::sync::Notify::new());
    let server_ready = Arc::new(tokio::sync::Notify::new());
    let ordering_ok = Arc::new(Mutex::new(true));

    let server_resume = Arc::clone(&resume);
    let ready_signal = Arc::clone(&server_ready);
    let ordering = Arc::clone(&ordering_ok);
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("accept");
        ready_signal.notify_waiters();
        // Deliberately do not read until released, so the client's writes
        // block once the kernel buffers fill.
        server_resume.notified().await;
        let (mut read_half, mut write_half) = socket.into_split();
        let mut received = 0usize;
        let mut buffer = vec![0u8; 64 * 1024];
        while received < expected_length {
            let count = read_half.read(&mut buffer).await.expect("read");
            if count == 0 {
                break;
            }
            for (index, byte) in buffer[..count].iter().enumerate() {
                let expected = if received + index < CHUNK { 1u8 } else { 2u8 };
                if *byte != expected {
                    *ordering.lock().unwrap() = false;
                }
            }
            received += count;
        }
        let _ = write_half.write_all(&[9]).await;
        let _ = write_half.shutdown().await;
        received
    });

    let inbound: Arc<Mutex<Vec<u8>>> = Arc::default();
    let closes = Arc::new(AtomicUsize::new(0));
    let errors: Arc<Mutex<Vec<String>>> = Arc::default();
    let factory = UnixTransportFactory::new(UnixTransportOptions {
        path: path.clone(),
        max_pending_bytes: Some(expected_length as u64),
    })
    .expect("transport options");
    let transport = factory
        .connect(Arc::new(RecordingHandlers {
            inbound: Arc::clone(&inbound),
            closes: Arc::clone(&closes),
            errors: Arc::clone(&errors),
        }))
        .await
        .expect("connects");

    server_ready.notified().await;
    let first = transport.send(vec![1u8; CHUNK]);
    let second = transport.send(vec![2u8; CHUNK]);
    // The budget is exactly the two queued chunks, so one more byte is refused.
    let refused = transport.send(vec![3u8]).await.expect_err("over budget");
    assert!(
        refused.to_string().contains("pending byte limit"),
        "unexpected error: {refused}"
    );

    resume.notify_waiters();
    first.await.expect("first write");
    second.await.expect("second write");
    let received = server.await.expect("join");
    assert_eq!(received, expected_length);
    assert!(*ordering_ok.lock().unwrap(), "writes were reordered");

    // The remote end arrives as exactly one terminal close.
    for _ in 0..200 {
        if closes.load(Ordering::SeqCst) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(*inbound.lock().unwrap(), vec![9]);
    assert!(errors.lock().unwrap().is_empty());
    assert_eq!(closes.load(Ordering::SeqCst), 1);

    transport.close();
    assert_eq!(closes.load(Ordering::SeqCst), 1);
}
