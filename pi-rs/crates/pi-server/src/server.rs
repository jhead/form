//! Port of `.upstream/packages/server/src/server.ts`.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::Mutex;
use pi_protocol::{
    encode_server_message, is_supported_protocol_version, ClientHello, ClientMessage,
    ClientMessageDecoder, EventEnvelope, FrameDecoderOptions, ProtocolError, ProtocolErrorCode,
    RequestEnvelope, ResponseEnvelope, ServerHello, ServerHelloError, ServerMessage,
    DEFAULT_MAX_FRAME_LENGTH, PROTOCOL_VERSION,
};

use crate::connection::{
    ByteConnection, ByteConnectionAcceptor, ByteConnectionHandler, ConnectionMutable,
    ConnectionStage, ConnectionState,
};
use crate::errors::{PiServerError, TransportError};
use crate::listener::PiServerListener;
use crate::sessions::{LiveSession, LiveSessionManager};
use crate::snapshots::ServerSnapshotPublisher;
use crate::types::{
    PiServerOptions, ServerErrorHandler, ServerErrorReport, SessionRuntimeEvent, SessionService,
};

const DEFAULT_HANDSHAKE_TIMEOUT_MS: u64 = 5_000;

pub(crate) struct ServerInner {
    id: String,
    listeners: Vec<Arc<dyn PiServerListener>>,
    max_frame_length: u32,
    handshake_timeout_ms: u64,
    on_error: Option<ServerErrorHandler>,
    connections: Mutex<Vec<Arc<ConnectionState>>>,
    sessions: LiveSessionManager,
    snapshots: Arc<ServerSnapshotPublisher>,
    self_weak: Weak<ServerInner>,
    closing: AtomicBool,
    started: AtomicBool,
    starting: AtomicBool,
    close_guard: tokio::sync::Mutex<()>,
    closed: AtomicBool,
}

impl ServerInner {
    pub(crate) fn is_closing(&self) -> bool {
        self.closing.load(Ordering::SeqCst)
    }

    pub(crate) fn sessions(&self) -> &LiveSessionManager {
        &self.sessions
    }

    pub(crate) fn connections(&self) -> Vec<Arc<ConnectionState>> {
        self.connections.lock().clone()
    }

    fn frame_options(&self) -> FrameDecoderOptions {
        FrameDecoderOptions::with_max_frame_length(self.max_frame_length)
    }

    fn me(&self) -> Option<Arc<ServerInner>> {
        self.self_weak.upgrade()
    }

    // -- error reporting ---------------------------------------------------

    pub(crate) fn report(&self, report: ServerErrorReport) {
        let Some(handler) = &self.on_error else {
            return;
        };
        // Error observers cannot affect server state.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(report)));
    }

    pub(crate) fn report_service_error(&self, error: PiServerError) {
        // Upstream reports the *cause* of an InternalServerError, never the
        // wrapper, so a private detail never reaches a log as a typed error.
        match error.private_cause() {
            Some(cause) => self.report(ServerErrorReport::Other(cause.to_string())),
            None => self.report(ServerErrorReport::Service(error)),
        }
    }

    /// Upstream's `toProtocolError`.
    fn to_protocol_error(&self, error: &PiServerError) -> ProtocolError {
        if let Some(cause) = error.private_cause() {
            self.report(ServerErrorReport::Other(cause.to_string()));
        }
        error.to_protocol_error()
    }

    // -- outbound ----------------------------------------------------------

    /// Encodes and enqueues synchronously so message order matches call order,
    /// then awaits the write to learn whether it landed.
    pub(crate) async fn send_message(
        &self,
        connection: &Arc<ConnectionState>,
        message: ServerMessage,
    ) -> bool {
        let Some(sending) = self.queue_message(connection, message) else {
            return false;
        };
        match sending {
            Ok(future) => match future.await {
                Ok(()) => true,
                Err(error) => {
                    self.report(ServerErrorReport::Transport(error));
                    self.close_connection(&connection.connection, None).await;
                    self.disconnect(connection).await;
                    false
                }
            },
            Err(()) => false,
        }
    }

    pub(crate) async fn send_event(
        &self,
        connection: &Arc<ConnectionState>,
        envelope: EventEnvelope,
    ) -> bool {
        self.send_message(connection, ServerMessage::Event(envelope))
            .await
    }

    /// Fire-and-forget variant of [`Self::send_message`] for upstream's
    /// `void this.sendMessage(...)` call sites.
    pub(crate) fn fire_event(&self, connection: &Arc<ConnectionState>, envelope: EventEnvelope) {
        let Some(sending) = self.queue_message(connection, ServerMessage::Event(envelope)) else {
            return;
        };
        let Ok(future) = sending else {
            return;
        };
        let Some(server) = self.me() else {
            return;
        };
        let connection = Arc::clone(connection);
        tokio::spawn(async move {
            if let Err(error) = future.await {
                server.report(ServerErrorReport::Transport(error));
                server.close_connection(&connection.connection, None).await;
                server.disconnect(&connection).await;
            }
        });
    }

    /// `None` when the connection is already gone, `Err(())` when the failure
    /// was already handled by tearing the connection down.
    #[allow(clippy::type_complexity)]
    fn queue_message(
        &self,
        connection: &Arc<ConnectionState>,
        message: ServerMessage,
    ) -> Option<Result<crate::connection::SendFuture, ()>> {
        if connection.disconnected() || connection.connection.closed() {
            return None;
        }
        match encode_server_message(&message, self.frame_options()) {
            Ok(frame) => Some(Ok(connection.connection.send(frame))),
            Err(error) => {
                self.report(ServerErrorReport::Protocol(error));
                if let Some(server) = self.me() {
                    let connection = Arc::clone(connection);
                    tokio::spawn(async move {
                        server.close_connection(&connection.connection, None).await;
                        server.disconnect(&connection).await;
                    });
                }
                Some(Err(()))
            }
        }
    }

    pub(crate) async fn close_connection(
        &self,
        connection: &Arc<dyn ByteConnection>,
        final_chunk: Option<Vec<u8>>,
    ) {
        if let Err(error) = connection.close(final_chunk).await {
            self.report(ServerErrorReport::Transport(error));
        }
    }

    // -- connection lifecycle ---------------------------------------------

    pub(crate) fn accept(
        self: &Arc<Self>,
        connection: Arc<dyn ByteConnection>,
    ) -> Arc<dyn ByteConnectionHandler> {
        if self.is_closing() {
            let server = Arc::clone(self);
            let closing = Arc::clone(&connection);
            tokio::spawn(async move { server.close_connection(&closing, None).await });
            return Arc::new(RejectingHandler {
                server: Arc::downgrade(self),
            });
        }

        let state = Arc::new(ConnectionState {
            id: uuid::Uuid::new_v4().to_string(),
            connection,
            decoder: Mutex::new(ClientMessageDecoder::new(self.frame_options())),
            inner: Mutex::new(ConnectionMutable {
                session_ids: HashSet::new(),
                stage: ConnectionStage::AwaitingHello,
                disconnected: false,
                handshake_complete: false,
                handshake: None,
                handshake_timeout: None,
            }),
        });

        let timeout = {
            let server = Arc::clone(self);
            let state = Arc::clone(&state);
            let delay = Duration::from_millis(self.handshake_timeout_ms);
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                server
                    .fail_protocol(
                        &state,
                        ProtocolError::new(ProtocolErrorCode::InvalidRequest, "Handshake timeout"),
                    )
                    .await;
            })
        };
        state.inner.lock().handshake_timeout = Some(timeout);
        self.connections.lock().push(Arc::clone(&state));

        Arc::new(ServerConnectionHandler {
            server: Arc::downgrade(self),
            state,
        })
    }

    fn receive(self: &Arc<Self>, state: &Arc<ConnectionState>, chunk: &[u8]) {
        if state.is_terminal() {
            return;
        }
        let decoded = state.decoder.lock().push(chunk);
        let messages = match decoded {
            Ok(messages) => messages,
            Err(error) => {
                let server = Arc::clone(self);
                let state = Arc::clone(state);
                let protocol_error =
                    ProtocolError::new(ProtocolErrorCode::InvalidRequest, error.to_string());
                tokio::spawn(async move { server.fail_protocol(&state, protocol_error).await });
                return;
            }
        };
        for message in messages {
            if state.is_terminal() {
                return;
            }
            self.dispatch_message(state, message);
        }
    }

    fn dispatch_message(self: &Arc<Self>, state: &Arc<ConnectionState>, message: ClientMessage) {
        let stage = state.stage();
        if stage == ConnectionStage::AwaitingHello {
            let ClientMessage::Hello(hello) = message else {
                self.spawn_fail_protocol(
                    state,
                    "The first client message must be hello",
                    ProtocolErrorCode::InvalidRequest,
                );
                return;
            };
            self.begin_handshake(state, hello);
            return;
        }

        let envelope = match message {
            ClientMessage::Hello(_) => {
                self.spawn_fail_protocol(
                    state,
                    "hello may only be sent as the first message",
                    ProtocolErrorCode::InvalidRequest,
                );
                return;
            }
            ClientMessage::Request(envelope) => envelope,
        };

        if stage == ConnectionStage::Ready {
            let server = Arc::clone(self);
            let state = Arc::clone(state);
            tokio::spawn(async move { server.handle_request(&state, envelope).await });
            return;
        }
        if stage != ConnectionStage::Handshaking {
            return;
        }
        let Some(handshake) = state.inner.lock().handshake.clone() else {
            return;
        };
        let server = Arc::clone(self);
        let state = Arc::clone(state);
        tokio::spawn(async move {
            let _ = handshake.await;
            if state.stage() == ConnectionStage::Ready && !state.disconnected() {
                server.handle_request(&state, envelope).await;
            }
        });
    }

    fn begin_handshake(self: &Arc<Self>, state: &Arc<ConnectionState>, hello: ClientHello) {
        let (signal, receiver) = futures::channel::oneshot::channel();
        {
            let mut inner = state.inner.lock();
            inner.stage = ConnectionStage::Handshaking;
            inner.handshake = Some(futures::FutureExt::shared(receiver));
        }
        let server = Arc::clone(self);
        let state = Arc::clone(state);
        tokio::spawn(async move {
            server.finish_handshake(&state, hello).await;
            // Deferred requests must unblock even when the handshake failed.
            let _ = signal.send(());
        });
    }

    async fn finish_handshake(self: &Arc<Self>, state: &Arc<ConnectionState>, hello: ClientHello) {
        if !is_supported_protocol_version(hello.version) {
            self.fail_protocol(
                state,
                ProtocolError::new(
                    ProtocolErrorCode::Version,
                    format!(
                        "Unsupported protocol version {}; expected {PROTOCOL_VERSION}",
                        hello.version
                    ),
                ),
            )
            .await;
            return;
        }

        let snapshot = match self.snapshots.get(None).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let protocol_error = self.to_protocol_error(&error);
                self.fail_protocol(state, protocol_error).await;
                return;
            }
        };
        if self.is_closing()
            || state.disconnected()
            || state.stage() != ConnectionStage::Handshaking
            || state.connection.closed()
        {
            return;
        }
        let revision = snapshot.revision;
        let sent = self
            .send_message(
                state,
                ServerMessage::Hello(ServerHello {
                    version: PROTOCOL_VERSION,
                    connection_id: state.id.clone(),
                    snapshot,
                }),
            )
            .await;
        if !sent || state.disconnected() || state.stage() != ConnectionStage::Handshaking {
            return;
        }
        {
            let mut inner = state.inner.lock();
            inner.handshake_complete = true;
            inner.stage = ConnectionStage::Ready;
        }
        state.clear_handshake_timeout();
        // The server changed while the handshake snapshot was being produced,
        // so catch this client up before it can observe a stale revision.
        if revision != self.snapshots.current_revision() {
            match self.snapshots.get(None).await {
                Ok(current) => {
                    self.send_event(
                        state,
                        EventEnvelope {
                            event: pi_protocol::ServerEvent::ServerSnapshot(
                                pi_protocol::ServerSnapshotEvent { snapshot: current },
                            ),
                        },
                    )
                    .await;
                }
                Err(error) => {
                    let protocol_error = self.to_protocol_error(&error);
                    self.fail_protocol(state, protocol_error).await;
                }
            }
        }
    }

    async fn handle_request(
        self: &Arc<Self>,
        state: &Arc<ConnectionState>,
        envelope: RequestEnvelope,
    ) {
        let response = match self.sessions.execute_command(state, envelope.request).await {
            Ok(result) => ResponseEnvelope::ok(envelope.id, result),
            Err(error) => ResponseEnvelope::failed(envelope.id, self.to_protocol_error(&error)),
        };
        self.send_message(state, ServerMessage::Response(response))
            .await;
    }

    fn transport_closed(self: &Arc<Self>, state: &Arc<ConnectionState>) {
        let should_end = {
            let inner = state.inner.lock();
            !inner.disconnected && inner.stage != ConnectionStage::Closing
        };
        if should_end {
            if let Err(error) = state.decoder.lock().end() {
                self.report(ServerErrorReport::Protocol(error));
            }
        }
        let server = Arc::clone(self);
        let state = Arc::clone(state);
        tokio::spawn(async move { server.disconnect(&state).await });
    }

    pub(crate) async fn disconnect(&self, state: &Arc<ConnectionState>) {
        let handshake_complete = {
            let mut inner = state.inner.lock();
            if inner.disconnected {
                return;
            }
            let complete = inner.handshake_complete;
            inner.disconnected = true;
            inner.stage = ConnectionStage::Closed;
            complete
        };
        state.clear_handshake_timeout();
        self.connections
            .lock()
            .retain(|candidate| !Arc::ptr_eq(candidate, state));
        self.sessions.disconnect(state).await;
        if !self.is_closing() && handshake_complete {
            self.snapshots.broadcast();
        }
    }

    fn spawn_fail_protocol(
        self: &Arc<Self>,
        state: &Arc<ConnectionState>,
        message: &str,
        code: ProtocolErrorCode,
    ) {
        let server = Arc::clone(self);
        let state = Arc::clone(state);
        let error = ProtocolError::new(code, message);
        tokio::spawn(async move { server.fail_protocol(&state, error).await });
    }

    async fn fail_protocol(&self, state: &Arc<ConnectionState>, error: ProtocolError) {
        {
            let mut inner = state.inner.lock();
            if inner.disconnected
                || inner.stage == ConnectionStage::Closing
                || inner.stage == ConnectionStage::Closed
            {
                return;
            }
            inner.stage = ConnectionStage::Closing;
        }
        state.clear_handshake_timeout();
        let final_frame = match encode_server_message(
            &ServerMessage::HelloError(ServerHelloError { error }),
            self.frame_options(),
        ) {
            Ok(frame) => Some(frame),
            Err(encode_error) => {
                self.report(ServerErrorReport::Protocol(encode_error));
                None
            }
        };
        self.close_connection(&state.connection, final_frame).await;
        self.disconnect(state).await;
    }

    async fn close_server_state(&self) {
        let connections = self.connections();
        for connection in &connections {
            connection.set_stage(ConnectionStage::Closing);
            connection.clear_handshake_timeout();
        }
        for connection in &connections {
            self.close_connection(&connection.connection, None).await;
        }
        for connection in &connections {
            self.disconnect(connection).await;
        }
        self.sessions.close().await;
        self.connections.lock().clear();
    }

    // -- helpers used by the session manager -------------------------------

    pub(crate) fn spawn_runtime_event(
        self: &Arc<Self>,
        live: Arc<LiveSession>,
        event: SessionRuntimeEvent,
    ) {
        let server = Arc::clone(self);
        tokio::spawn(async move { server.sessions.handle_runtime_event(live, event).await });
    }

    pub(crate) fn spawn_maybe_dispose(self: &Arc<Self>, live: Arc<LiveSession>) {
        let server = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(error) = server.sessions.maybe_dispose(&live).await {
                server.report_service_error(error);
            }
        });
    }

    pub(crate) fn broadcast_server_snapshot(&self) {
        self.snapshots.broadcast();
    }
}

struct ServerConnectionHandler {
    server: Weak<ServerInner>,
    state: Arc<ConnectionState>,
}

impl ByteConnectionHandler for ServerConnectionHandler {
    fn on_data(&self, chunk: &[u8]) {
        if let Some(server) = self.server.upgrade() {
            server.receive(&self.state, chunk);
        }
    }

    fn on_close(&self) {
        if let Some(server) = self.server.upgrade() {
            server.transport_closed(&self.state);
        }
    }

    fn on_error(&self, error: TransportError) {
        let Some(server) = self.server.upgrade() else {
            return;
        };
        server.report(ServerErrorReport::Transport(error));
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            server.close_connection(&state.connection, None).await;
            server.disconnect(&state).await;
        });
    }
}

/// Handler installed for connections that arrive after `close()`.
struct RejectingHandler {
    server: Weak<ServerInner>,
}

impl ByteConnectionHandler for RejectingHandler {
    fn on_data(&self, _chunk: &[u8]) {}
    fn on_close(&self) {}
    fn on_error(&self, error: TransportError) {
        if let Some(server) = self.server.upgrade() {
            server.report(ServerErrorReport::Transport(error));
        }
    }
}

struct ServerAcceptor {
    server: Weak<ServerInner>,
}

impl ByteConnectionAcceptor for ServerAcceptor {
    fn accept(&self, connection: Arc<dyn ByteConnection>) -> Arc<dyn ByteConnectionHandler> {
        match self.server.upgrade() {
            Some(server) => server.accept(connection),
            None => Arc::new(RejectingHandler {
                server: Weak::new(),
            }),
        }
    }
}

/// The session protocol server.
#[derive(Clone)]
pub struct PiServer {
    inner: Arc<ServerInner>,
}

impl PiServer {
    pub fn new(
        service: Arc<dyn SessionService>,
        options: PiServerOptions,
    ) -> Result<Self, TransportError> {
        if options.server_id.as_deref() == Some("") {
            return Err(TransportError::new("PiServer serverId must not be empty"));
        }
        let max_frame_length = options.max_frame_length.unwrap_or(DEFAULT_MAX_FRAME_LENGTH);
        if max_frame_length == 0 {
            return Err(TransportError::new(format!(
                "PiServer maxFrameLength must be an integer between 1 and {}",
                u32::MAX
            )));
        }
        let handshake_timeout_ms = options
            .handshake_timeout_ms
            .unwrap_or(DEFAULT_HANDSHAKE_TIMEOUT_MS);
        if handshake_timeout_ms == 0 {
            return Err(TransportError::new(
                "PiServer handshakeTimeoutMs must be a positive integer",
            ));
        }
        let id = options
            .server_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let sessions = LiveSessionManager::new(Arc::clone(&service));
        let snapshots = Arc::new(ServerSnapshotPublisher::new(id.clone(), service));

        let inner = Arc::new_cyclic(|weak: &Weak<ServerInner>| ServerInner {
            id,
            listeners: options.listeners,
            max_frame_length,
            handshake_timeout_ms,
            on_error: options.on_error,
            connections: Mutex::new(Vec::new()),
            sessions,
            snapshots,
            self_weak: weak.clone(),
            closing: AtomicBool::new(false),
            started: AtomicBool::new(false),
            starting: AtomicBool::new(false),
            close_guard: tokio::sync::Mutex::new(()),
            closed: AtomicBool::new(false),
        });
        inner.sessions.attach_server(Arc::downgrade(&inner));
        inner.snapshots.attach_server(Arc::downgrade(&inner));
        Ok(Self { inner })
    }

    pub fn id(&self) -> &str {
        &self.inner.id
    }

    pub fn addresses(&self) -> Vec<String> {
        self.inner
            .listeners
            .iter()
            .filter_map(|listener| listener.address())
            .collect()
    }

    pub async fn start(&self) -> Result<(), TransportError> {
        if self.inner.started.load(Ordering::SeqCst) {
            return Err(TransportError::new("PiServer is already started"));
        }
        if self.inner.starting.swap(true, Ordering::SeqCst) {
            return Err(TransportError::new("PiServer is already starting"));
        }
        if self.inner.is_closing() {
            self.inner.starting.store(false, Ordering::SeqCst);
            return Err(TransportError::new("PiServer is closing or closed"));
        }
        let outcome = self.start_internal().await;
        self.inner.starting.store(false, Ordering::SeqCst);
        outcome
    }

    async fn start_internal(&self) -> Result<(), TransportError> {
        let acceptor: Arc<dyn ByteConnectionAcceptor> = Arc::new(ServerAcceptor {
            server: Arc::downgrade(&self.inner),
        });
        let mut started: Vec<Arc<dyn PiServerListener>> = Vec::new();
        for listener in &self.inner.listeners {
            match listener.start(Arc::clone(&acceptor)).await {
                Ok(()) => started.push(Arc::clone(listener)),
                Err(error) => {
                    self.inner.closing.store(true, Ordering::SeqCst);
                    for listener in started {
                        let _ = listener.close().await;
                    }
                    self.inner.close_server_state().await;
                    return Err(error);
                }
            }
        }
        self.inner.started.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Accepts one already-established byte connection. Exposed because
    /// upstream's transport conformance tests drive the server this way.
    pub fn accept(&self, connection: Arc<dyn ByteConnection>) -> Arc<dyn ByteConnectionHandler> {
        self.inner.accept(connection)
    }

    pub async fn close(&self) {
        self.inner.closing.store(true, Ordering::SeqCst);
        let _guard = self.inner.close_guard.lock().await;
        if self.inner.closed.load(Ordering::SeqCst) {
            return;
        }
        for listener in &self.inner.listeners {
            if let Err(error) = listener.close().await {
                self.inner.report(ServerErrorReport::Transport(error));
            }
        }
        self.inner.close_server_state().await;
        self.inner.started.store(false, Ordering::SeqCst);
        self.inner.closed.store(true, Ordering::SeqCst);
    }
}

impl std::fmt::Debug for PiServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PiServer")
            .field("id", &self.inner.id)
            .field("addresses", &self.addresses())
            .finish()
    }
}
