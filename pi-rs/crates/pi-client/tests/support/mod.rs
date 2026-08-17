//! Port of `.upstream/packages/client/test/support.ts`.

#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_client::{
    ByteTransport, ByteTransportFactory, ByteTransportHandlers, ConnectFuture, PiClient,
    PiClientOptions, PiSessionHandle, SendFuture, TransportError,
};
use pi_protocol::{
    encode_server_message, ClientMessage, ClientMessageDecoder, CommandResult, FrameDecoderOptions,
    ModelRef, RequestEnvelope, ServerHello, ServerMessage, ServerSnapshot, SessionPhase,
    SessionResult, SessionSnapshot, ThinkingLevel, PROTOCOL_VERSION,
};

type MessageListener = Arc<dyn Fn(&MemoryByteServer, ClientMessage) + Send + Sync>;

/// An in-memory stand-in for the peer, so the client's state machine can be
/// driven byte-exactly without a socket.
#[derive(Default)]
pub struct MemoryByteServer {
    handlers: Mutex<Option<Arc<dyn ByteTransportHandlers>>>,
    decoder: Mutex<Option<ClientMessageDecoder>>,
    listeners: Mutex<Vec<MessageListener>>,
    pub sent_by_client: Mutex<Vec<Vec<u8>>>,
    pub client_close_count: AtomicUsize,
}

impl MemoryByteServer {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn on_message(
        self: &Arc<Self>,
        listener: impl Fn(&MemoryByteServer, ClientMessage) + Send + Sync + 'static,
    ) {
        self.listeners.lock().unwrap().push(Arc::new(listener));
    }

    /// Replies to the client `hello` with a stock server `hello`.
    pub fn auto_hello(self: &Arc<Self>, snapshot: ServerSnapshot) {
        self.on_message(move |server, message| {
            if matches!(message, ClientMessage::Hello(_)) {
                server.send(&ServerMessage::Hello(ServerHello {
                    version: PROTOCOL_VERSION,
                    connection_id: "connection-1".to_string(),
                    snapshot: snapshot.clone(),
                }));
            }
        });
    }

    pub fn send(&self, message: &ServerMessage) {
        let frame = encode_server_message(message, FrameDecoderOptions::default())
            .expect("server message encodes");
        self.send_raw(&frame);
    }

    pub fn send_split(&self, message: &ServerMessage, split_at: usize) {
        let frame = encode_server_message(message, FrameDecoderOptions::default())
            .expect("server message encodes");
        self.send_raw(&frame[..split_at]);
        self.send_raw(&frame[split_at..]);
    }

    pub fn send_together(&self, messages: &[ServerMessage]) {
        let mut chunk = Vec::new();
        for message in messages {
            chunk.extend_from_slice(
                &encode_server_message(message, FrameDecoderOptions::default())
                    .expect("server message encodes"),
            );
        }
        self.send_raw(&chunk);
    }

    pub fn send_raw(&self, chunk: &[u8]) {
        let handlers = self.handlers.lock().unwrap().clone();
        if let Some(handlers) = handlers {
            handlers.on_data(chunk);
        }
    }

    pub fn close(&self) {
        let handlers = self.handlers.lock().unwrap().clone();
        if let Some(handlers) = handlers {
            handlers.on_close();
        }
    }

    pub fn error(&self, message: &str) {
        let handlers = self.handlers.lock().unwrap().clone();
        if let Some(handlers) = handlers {
            handlers.on_error(TransportError::new(message));
        }
    }

    pub fn sent_count(&self) -> usize {
        self.sent_by_client.lock().unwrap().len()
    }

    pub fn close_count(&self) -> usize {
        self.client_close_count.load(Ordering::SeqCst)
    }

    fn deliver(self: &Arc<Self>, chunk: &[u8]) {
        self.sent_by_client.lock().unwrap().push(chunk.to_vec());
        let decoded = {
            let mut decoder = self.decoder.lock().unwrap();
            decoder
                .get_or_insert_with(ClientMessageDecoder::default)
                .push(chunk)
        };
        let messages = decoded.expect("client frames decode");
        let listeners = self.listeners.lock().unwrap().clone();
        for message in messages {
            for listener in &listeners {
                listener(self, message.clone());
            }
        }
    }
}

/// Collects every request the client sends, so tests can correlate responses.
pub fn collect_requests(server: &Arc<MemoryByteServer>) -> Arc<Mutex<Vec<RequestEnvelope>>> {
    let requests: Arc<Mutex<Vec<RequestEnvelope>>> = Arc::default();
    let sink = Arc::clone(&requests);
    server.on_message(move |_, message| {
        if let ClientMessage::Request(envelope) = message {
            sink.lock().unwrap().push(envelope);
        }
    });
    requests
}

pub fn last_request(requests: &Arc<Mutex<Vec<RequestEnvelope>>>) -> RequestEnvelope {
    requests
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("at least one request")
}

pub fn find_request(requests: &Arc<Mutex<Vec<RequestEnvelope>>>, command: &str) -> RequestEnvelope {
    requests
        .lock()
        .unwrap()
        .iter()
        .find(|envelope| envelope.request.name() == command)
        .cloned()
        .unwrap_or_else(|| panic!("missing {command} request"))
}

/// Adapts one (or a sequence of) `MemoryByteServer`s into a transport factory.
/// Reconnect tests hand it several servers, one per connection attempt.
pub struct MemoryFactory {
    servers: Vec<Arc<MemoryByteServer>>,
    attempts: AtomicUsize,
}

impl MemoryFactory {
    pub fn single(server: &Arc<MemoryByteServer>) -> Arc<Self> {
        Self::sequence(vec![Arc::clone(server)])
    }

    pub fn sequence(servers: Vec<Arc<MemoryByteServer>>) -> Arc<Self> {
        Arc::new(Self {
            servers,
            attempts: AtomicUsize::new(0),
        })
    }

    pub fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }
}

impl ByteTransportFactory for MemoryFactory {
    fn connect(&self, handlers: Arc<dyn ByteTransportHandlers>) -> ConnectFuture {
        let index = self.attempts.fetch_add(1, Ordering::SeqCst);
        let server = Arc::clone(
            self.servers
                .get(index)
                .or_else(|| self.servers.last())
                .expect("at least one server"),
        );
        *server.handlers.lock().unwrap() = Some(handlers);
        *server.decoder.lock().unwrap() = Some(ClientMessageDecoder::default());
        Box::pin(async move {
            Ok(Arc::new(MemoryTransport {
                server,
                closed: AtomicBool::new(false),
            }) as Arc<dyn ByteTransport>)
        })
    }
}

struct MemoryTransport {
    server: Arc<MemoryByteServer>,
    closed: AtomicBool,
}

impl ByteTransport for MemoryTransport {
    fn send(&self, chunk: Vec<u8>) -> SendFuture {
        if self.closed.load(Ordering::SeqCst) {
            return Box::pin(async { Err(TransportError::new("Transport is closed")) });
        }
        self.server.deliver(&chunk);
        Box::pin(async { Ok(()) })
    }

    fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.server
            .client_close_count
            .fetch_add(1, Ordering::SeqCst);
    }
}

pub fn base_server_snapshot() -> ServerSnapshot {
    ServerSnapshot {
        server_id: "server-1".to_string(),
        protocol_version: PROTOCOL_VERSION,
        revision: 1,
        sessions: Vec::new(),
        models: Vec::new(),
    }
}

pub fn session_snapshot(id: &str) -> SessionSnapshot {
    SessionSnapshot {
        id: id.to_string(),
        name: None,
        cwd: "/workspace".to_string(),
        created_at: 1,
        updated_at: 1,
        phase: SessionPhase::Idle,
        model: ModelRef {
            provider: "faux".to_string(),
            id: "model".to_string(),
        },
        thinking_level: ThinkingLevel::Off,
        attached: true,
        locked: true,
        revision: 1,
        transcript: Vec::new(),
        queued_steer: Vec::new(),
        queued_steer_count: 0,
    }
}

pub fn attach_result(snapshot: SessionSnapshot) -> CommandResult {
    CommandResult::Attach(SessionResult { session: snapshot })
}

pub fn make_server() -> Arc<MemoryByteServer> {
    MemoryByteServer::new()
}

pub fn create_client(server: &Arc<MemoryByteServer>) -> PiClient {
    PiClient::new(PiClientOptions::new(
        MemoryFactory::single(server) as Arc<dyn ByteTransportFactory>
    ))
    .expect("client options")
}

pub async fn connect_client(server: &Arc<MemoryByteServer>) -> PiClient {
    server.auto_hello(base_server_snapshot());
    let client = create_client(server);
    client.connect().await.expect("handshake");
    client
}

/// Attaches to `snapshot.id`, answering the attach request the client sends.
pub async fn attach_session(
    client: &PiClient,
    server: &Arc<MemoryByteServer>,
    snapshot: SessionSnapshot,
) -> PiSessionHandle {
    let expected = snapshot.clone();
    server.on_message(move |server, message| {
        if let ClientMessage::Request(envelope) = message {
            if envelope.request.name() == "attach" {
                server.send(&ServerMessage::Response(pi_protocol::ResponseEnvelope::ok(
                    envelope.id,
                    attach_result(expected.clone()),
                )));
            }
        }
    });
    client
        .attach_session(&snapshot.id)
        .await
        .expect("attach succeeds")
}
