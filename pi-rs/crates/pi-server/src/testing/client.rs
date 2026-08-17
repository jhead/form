//! Port of `.upstream/packages/server/src/testing/client.ts`.
//!
//! A raw wire client: it speaks frames, not `PiClient` semantics, so
//! conformance tests can send malformed input and observe exactly what the
//! server writes back.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use pi_protocol::{
    encode_client_message, ClientHello, ClientMessage, Command, FrameDecoderOptions,
    RequestEnvelope, ResponseEnvelope, ServerMessage, ServerMessageDecoder, PROTOCOL_VERSION,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::Notify;

use crate::testing::service::Deferred;

/// Default ceiling for the `wait_*` helpers so a hung server fails the test
/// with a clear message instead of hanging the suite.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(5);

struct Shared {
    messages: Mutex<Vec<ServerMessage>>,
    decoder: Mutex<ServerMessageDecoder>,
    notify: Notify,
    closed: Deferred<()>,
    closed_flag: std::sync::atomic::AtomicBool,
    failure: Mutex<Option<String>>,
}

impl Shared {
    fn mark_closed(&self) {
        self.closed_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.closed.resolve(());
        self.notify.notify_waiters();
    }
}

pub struct ProtocolTestClient {
    shared: Arc<Shared>,
    writer: Mutex<Option<tokio::net::unix::OwnedWriteHalf>>,
    reader: Mutex<Option<tokio::task::JoinHandle<()>>>,
    request_sequence: Mutex<u64>,
}

impl ProtocolTestClient {
    pub async fn connect(path: impl AsRef<Path>) -> std::io::Result<Arc<Self>> {
        let stream = UnixStream::connect(path.as_ref()).await?;
        let (mut read_half, write_half) = stream.into_split();
        let shared = Arc::new(Shared {
            messages: Mutex::new(Vec::new()),
            decoder: Mutex::new(ServerMessageDecoder::default()),
            notify: Notify::new(),
            closed: Deferred::new(),
            closed_flag: std::sync::atomic::AtomicBool::new(false),
            failure: Mutex::new(None),
        });
        let reader_shared = Arc::clone(&shared);
        let reader = tokio::spawn(async move {
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                match read_half.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(count) => {
                        let decoded = reader_shared.decoder.lock().push(&buffer[..count]);
                        match decoded {
                            Ok(messages) => {
                                reader_shared.messages.lock().extend(messages);
                            }
                            Err(error) => {
                                *reader_shared.failure.lock() = Some(error.to_string());
                                break;
                            }
                        }
                        reader_shared.notify.notify_waiters();
                    }
                    Err(error) => {
                        *reader_shared.failure.lock() = Some(error.to_string());
                        break;
                    }
                }
            }
            reader_shared.mark_closed();
        });
        Ok(Arc::new(Self {
            shared,
            writer: Mutex::new(Some(write_half)),
            reader: Mutex::new(Some(reader)),
            request_sequence: Mutex::new(0),
        }))
    }

    pub fn messages(&self) -> Vec<ServerMessage> {
        self.shared.messages.lock().clone()
    }

    pub fn message_count(&self) -> usize {
        self.shared.messages.lock().len()
    }

    pub fn closed(&self) -> bool {
        self.shared
            .closed_flag
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub async fn send_bytes(&self, chunk: &[u8]) -> std::io::Result<()> {
        let mut writer = self.writer.lock().take();
        let result = match writer.as_mut() {
            Some(writer) => writer.write_all(chunk).await,
            None => Err(std::io::Error::other("Wire client is closed")),
        };
        *self.writer.lock() = writer;
        result
    }

    pub async fn send_message(&self, message: &ClientMessage) -> std::io::Result<()> {
        let frame = encode_client_message(message, FrameDecoderOptions::default())
            .map_err(std::io::Error::other)?;
        self.send_bytes(&frame).await
    }

    pub async fn send_fragmented_message(
        &self,
        message: &ClientMessage,
        split_at: usize,
    ) -> std::io::Result<()> {
        let frame = encode_client_message(message, FrameDecoderOptions::default())
            .map_err(std::io::Error::other)?;
        self.send_bytes(&frame[..split_at]).await?;
        self.send_bytes(&frame[split_at..]).await
    }

    /// Sends `hello` and waits for the server's `hello` or `hello_error`.
    pub async fn hello(&self) -> ServerMessage {
        self.hello_with_version(u64::from(PROTOCOL_VERSION)).await
    }

    pub async fn hello_with_version(&self, version: u64) -> ServerMessage {
        self.send_message(&ClientMessage::Hello(ClientHello { version }))
            .await
            .expect("hello write");
        self.next(|message| {
            matches!(
                message,
                ServerMessage::Hello(_) | ServerMessage::HelloError(_)
            )
        })
        .await
    }

    pub async fn request(&self, command: Command) -> ResponseEnvelope {
        let id = {
            let mut sequence = self.request_sequence.lock();
            *sequence += 1;
            format!("request-{sequence}")
        };
        self.request_with_id(command, &id).await
    }

    pub async fn request_with_id(&self, command: Command, id: &str) -> ResponseEnvelope {
        self.send_message(&ClientMessage::Request(RequestEnvelope {
            id: id.to_string(),
            request: command,
        }))
        .await
        .expect("request write");
        let owned = id.to_string();
        let message = self
            .next(move |message| {
                matches!(message, ServerMessage::Response(envelope) if envelope.id == owned)
            })
            .await;
        match message {
            ServerMessage::Response(envelope) => envelope,
            other => panic!("expected a response, got {other:?}"),
        }
    }

    pub async fn next(&self, predicate: impl Fn(&ServerMessage) -> bool) -> ServerMessage {
        self.next_from(0, predicate).await
    }

    pub async fn next_from(
        &self,
        index: usize,
        predicate: impl Fn(&ServerMessage) -> bool,
    ) -> ServerMessage {
        let deadline = tokio::time::Instant::now() + DEFAULT_WAIT;
        loop {
            let notified = self.shared.notify.notified();
            if let Some(found) = self
                .shared
                .messages
                .lock()
                .iter()
                .skip(index)
                .find(|message| predicate(message))
                .cloned()
            {
                return found;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                panic!(
                    "timed out waiting for a server message; saw {:?}",
                    self.messages()
                );
            }
        }
    }

    pub async fn wait_for_close(&self) {
        let deadline = tokio::time::Instant::now() + DEFAULT_WAIT;
        if tokio::time::timeout_at(deadline, self.shared.closed.wait())
            .await
            .is_err()
        {
            panic!("timed out waiting for the wire connection to close");
        }
    }

    pub async fn close(&self) {
        if let Some(mut writer) = self.writer.lock().take() {
            tokio::spawn(async move {
                let _ = writer.shutdown().await;
            });
        }
        if let Some(reader) = self.reader.lock().take() {
            reader.abort();
        }
        self.shared.mark_closed();
    }
}

pub async fn connect_unix_test_client(
    path: impl AsRef<Path>,
) -> std::io::Result<Arc<ProtocolTestClient>> {
    ProtocolTestClient::connect(path).await
}
