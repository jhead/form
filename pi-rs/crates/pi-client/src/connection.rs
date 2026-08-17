//! Port of `.upstream/packages/client/src/connection.ts`.
//!
//! Upstream identifies "is this still the same connection?" by comparing the
//! lifecycle object's identity. Rust has no such identity for an enum value, so
//! every attempt carries a monotonically increasing `id` and the equivalent
//! check is `state == Connected && id == captured`. That is what makes the
//! re-entrancy rules hold: a listener that disconnects (or reconnects) while
//! the handshake is being applied bumps the id and every later step bails.

use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use pi_protocol::{
    encode_client_message, ClientHello, ClientMessage, FrameDecoderOptions, ServerMessage,
    ServerMessageDecoder, DEFAULT_MAX_FRAME_LENGTH, PROTOCOL_VERSION,
};
use tokio::sync::oneshot;

use crate::errors::{PiClientError, TransportError};
use crate::transport::{ByteTransport, ByteTransportFactory, ByteTransportHandlers};
use crate::types::{ConnectionState, ConnectionStateChange};

/// Everything `Connection` needs from the client that owns it.
pub(crate) trait ConnectionHost: Send + Sync + 'static {
    fn connection(&self) -> &Connection;
    fn transport_factory(&self) -> Arc<dyn ByteTransportFactory>;
    fn on_handshake(&self, snapshot: pi_protocol::ServerSnapshot);
    /// Never a `hello`/`hello_error`; the connection consumes those itself.
    fn on_message(&self, message: ServerMessage);
    fn on_state_change(&self, change: ConnectionStateChange);
}

type Handshake = oneshot::Sender<Result<pi_protocol::ServerSnapshot, PiClientError>>;

struct Active {
    id: u64,
    decoder: ServerMessageDecoder,
    transport: Option<Arc<dyn ByteTransport>>,
    handshake: Option<Handshake>,
}

enum Lifecycle {
    Disconnected,
    Connecting(Active),
    Connected(Active),
}

impl Lifecycle {
    fn state(&self) -> ConnectionState {
        match self {
            Self::Disconnected => ConnectionState::Disconnected,
            Self::Connecting(_) => ConnectionState::Connecting,
            Self::Connected(_) => ConnectionState::Connected,
        }
    }

    fn active(&self) -> Option<&Active> {
        match self {
            Self::Disconnected => None,
            Self::Connecting(active) | Self::Connected(active) => Some(active),
        }
    }

    fn active_mut(&mut self) -> Option<&mut Active> {
        match self {
            Self::Disconnected => None,
            Self::Connecting(active) | Self::Connected(active) => Some(active),
        }
    }
}

pub(crate) struct Connection {
    host: Mutex<Weak<dyn ConnectionHost>>,
    lifecycle: Mutex<Lifecycle>,
    sequence: Mutex<u64>,
    max_frame_length: u32,
}

impl Connection {
    pub(crate) fn new(max_frame_length: Option<u32>) -> Result<Self, PiClientError> {
        let max_frame_length = max_frame_length.unwrap_or(DEFAULT_MAX_FRAME_LENGTH);
        if max_frame_length == 0 {
            return Err(PiClientError::InvalidOptions(format!(
                "PiClient maxFrameLength must be an integer between 1 and {}",
                u32::MAX
            )));
        }
        Ok(Self {
            host: Mutex::new(Weak::<crate::client::ClientInner>::new()),
            lifecycle: Mutex::new(Lifecycle::Disconnected),
            sequence: Mutex::new(0),
            max_frame_length,
        })
    }

    pub(crate) fn attach_host(&self, host: Weak<dyn ConnectionHost>) {
        *self.host.lock() = host;
    }

    fn host(&self) -> Option<Arc<dyn ConnectionHost>> {
        self.host.lock().upgrade()
    }

    pub(crate) fn state(&self) -> ConnectionState {
        self.lifecycle.lock().state()
    }

    pub(crate) fn max_frame_length(&self) -> u32 {
        self.max_frame_length
    }

    fn frame_options(&self) -> FrameDecoderOptions {
        FrameDecoderOptions::with_max_frame_length(self.max_frame_length)
    }

    pub(crate) async fn connect(&self) -> Result<pi_protocol::ServerSnapshot, PiClientError> {
        let (id, receiver) = {
            let mut lifecycle = self.lifecycle.lock();
            if !matches!(*lifecycle, Lifecycle::Disconnected) {
                return Err(PiClientError::Disconnected(format!(
                    "PiClient is already {}",
                    lifecycle.state().as_str()
                )));
            }
            let mut sequence = self.sequence.lock();
            *sequence += 1;
            let id = *sequence;
            let (sender, receiver) = oneshot::channel();
            *lifecycle = Lifecycle::Connecting(Active {
                id,
                decoder: ServerMessageDecoder::new(self.frame_options()),
                transport: None,
                handshake: Some(sender),
            });
            (id, receiver)
        };

        if let Some(host) = self.host() {
            host.on_state_change(ConnectionStateChange {
                state: ConnectionState::Connecting,
                error: None,
            });
        }

        self.open_transport(id).await;

        match receiver.await {
            Ok(result) => result,
            // The sender was dropped without being used; treat it as a plain
            // disconnection rather than panicking.
            Err(_) => Err(PiClientError::disconnected()),
        }
    }

    pub(crate) fn disconnect(&self, reason: PiClientError) {
        if matches!(*self.lifecycle.lock(), Lifecycle::Disconnected) {
            return;
        }
        self.fail_and_close(reason);
    }

    pub(crate) fn fail(&self, error: PiClientError) {
        self.fail_and_close(error);
    }

    /// Upstream's synchronous `send`: the write is queued now and its failure
    /// is reported later through the connection's terminal path.
    pub(crate) fn send(&self, frame: Vec<u8>) -> Result<(), PiClientError> {
        let (transport, id) = {
            let lifecycle = self.lifecycle.lock();
            match &*lifecycle {
                Lifecycle::Connected(active) => (
                    active
                        .transport
                        .clone()
                        .expect("a connected lifecycle always has a transport"),
                    active.id,
                ),
                _ => return Err(PiClientError::disconnected()),
            }
        };
        let sending = transport.send(frame);
        let host = self.host();
        tokio::spawn(async move {
            let Err(error) = sending.await else {
                return;
            };
            let Some(host) = host else {
                return;
            };
            let connection = host.connection();
            if connection.is_current(id) {
                connection.fail_and_close(error.into());
            }
        });
        Ok(())
    }

    async fn open_transport(&self, id: u64) {
        let Some(host) = self.host() else {
            return;
        };
        let handlers: Arc<dyn ByteTransportHandlers> = Arc::new(TransportHandlers {
            host: Arc::downgrade(&host),
            id,
        });
        let transport = match host.transport_factory().connect(handlers).await {
            Ok(transport) => transport,
            Err(error) => {
                if self.is_current(id) {
                    self.fail(error.into());
                }
                return;
            }
        };

        {
            let mut lifecycle = self.lifecycle.lock();
            match &mut *lifecycle {
                Lifecycle::Connecting(active) if active.id == id => {
                    active.transport = Some(transport.clone());
                }
                _ => {
                    drop(lifecycle);
                    transport.close();
                    return;
                }
            }
        }

        let hello = ClientMessage::Hello(ClientHello {
            version: u64::from(PROTOCOL_VERSION),
        });
        let frame = match encode_client_message(&hello, self.frame_options()) {
            Ok(frame) => frame,
            Err(error) => {
                if self.is_current(id) {
                    self.fail_and_close(error.into());
                }
                return;
            }
        };
        if let Err(error) = transport.send(frame).await {
            if self.is_current(id) {
                self.fail_and_close(error.into());
            }
        }
    }

    fn handle_data(&self, id: u64, chunk: &[u8]) {
        type Decoded = Result<Vec<ServerMessage>, pi_protocol::ProtocolValidationError>;
        enum Step {
            Ignore,
            NoTransport,
            Decoded(Decoded),
        }
        let step = {
            let mut lifecycle = self.lifecycle.lock();
            match &mut *lifecycle {
                Lifecycle::Disconnected => Step::Ignore,
                Lifecycle::Connecting(active) if active.id == id && active.transport.is_none() => {
                    Step::NoTransport
                }
                Lifecycle::Connecting(active) | Lifecycle::Connected(active) => {
                    if active.id == id {
                        Step::Decoded(active.decoder.push(chunk))
                    } else {
                        Step::Ignore
                    }
                }
            }
        };
        let decoded = match step {
            Step::Ignore => return,
            Step::NoTransport => {
                self.fail_and_close(PiClientError::ProtocolViolation(
                    "Received server data before the client hello was sent".to_string(),
                ));
                return;
            }
            Step::Decoded(decoded) => decoded,
        };
        let messages = match decoded {
            Ok(messages) => messages,
            Err(error) => {
                self.fail_and_close(error.into());
                return;
            }
        };
        for message in messages {
            if matches!(*self.lifecycle.lock(), Lifecycle::Disconnected) {
                return;
            }
            self.handle_message(message);
        }
    }

    fn handle_message(&self, message: ServerMessage) {
        let Some(host) = self.host() else {
            return;
        };
        let handshaking = {
            let lifecycle = self.lifecycle.lock();
            match &*lifecycle {
                Lifecycle::Connecting(active) => Some(active.id),
                Lifecycle::Connected(_) => None,
                Lifecycle::Disconnected => return,
            }
        };
        if let Some(id) = handshaking {
            self.handle_handshake_message(host.as_ref(), id, message);
            return;
        }
        match message {
            ServerMessage::Hello(_) | ServerMessage::HelloError(_) => {
                self.fail_and_close(PiClientError::ProtocolViolation(
                    "Unexpected handshake message".to_string(),
                ));
            }
            other => host.on_message(other),
        }
    }

    fn handle_handshake_message(&self, host: &dyn ConnectionHost, id: u64, message: ServerMessage) {
        let hello = match message {
            ServerMessage::HelloError(error) => {
                self.fail_and_close(error.error.into());
                return;
            }
            ServerMessage::Hello(hello) => hello,
            _ => {
                self.fail_and_close(PiClientError::ProtocolViolation(
                    "Expected server hello as first message".to_string(),
                ));
                return;
            }
        };

        // Promote connecting -> connected, keeping the handshake resolver.
        enum Promotion {
            Done,
            Stale,
            NoTransport,
        }
        let promotion = {
            let mut lifecycle = self.lifecycle.lock();
            match &*lifecycle {
                Lifecycle::Connecting(active) if active.id == id => {
                    if active.transport.is_none() {
                        Promotion::NoTransport
                    } else {
                        let previous = std::mem::replace(&mut *lifecycle, Lifecycle::Disconnected);
                        let Lifecycle::Connecting(active) = previous else {
                            unreachable!("matched above")
                        };
                        *lifecycle = Lifecycle::Connected(active);
                        Promotion::Done
                    }
                }
                _ => Promotion::Stale,
            }
        };
        match promotion {
            Promotion::Done => {}
            Promotion::Stale => return,
            Promotion::NoTransport => {
                self.fail_and_close(PiClientError::ProtocolViolation(
                    "Received server hello before the client hello was sent".to_string(),
                ));
                return;
            }
        }

        host.on_handshake(hello.snapshot.clone());
        if !self.is_current_connected(id) {
            return;
        }
        host.on_state_change(ConnectionStateChange {
            state: ConnectionState::Connected,
            error: None,
        });
        let handshake = {
            let mut lifecycle = self.lifecycle.lock();
            match &mut *lifecycle {
                Lifecycle::Connected(active) if active.id == id => active.handshake.take(),
                _ => return,
            }
        };
        if let Some(handshake) = handshake {
            let _ = handshake.send(Ok(hello.snapshot));
        }
    }

    fn handle_close(&self) {
        let error = {
            let mut lifecycle = self.lifecycle.lock();
            let Some(active) = lifecycle.active_mut() else {
                return;
            };
            match active.decoder.end() {
                Ok(()) => PiClientError::Disconnected("Byte transport closed".to_string()),
                Err(error) => error.into(),
            }
        };
        self.fail_inner(error);
    }

    fn fail_and_close(&self, error: PiClientError) {
        let transport = self
            .lifecycle
            .lock()
            .active()
            .and_then(|active| active.transport.clone());
        self.fail_inner(error);
        if let Some(transport) = transport {
            transport.close();
        }
    }

    fn fail_inner(&self, error: PiClientError) {
        let handshake = {
            let mut lifecycle = self.lifecycle.lock();
            if matches!(*lifecycle, Lifecycle::Disconnected) {
                return;
            }
            let previous = std::mem::replace(&mut *lifecycle, Lifecycle::Disconnected);
            match previous {
                Lifecycle::Connecting(active) | Lifecycle::Connected(active) => active.handshake,
                Lifecycle::Disconnected => None,
            }
        };
        if let Some(handshake) = handshake {
            let _ = handshake.send(Err(error.clone()));
        }
        if let Some(host) = self.host() {
            host.on_state_change(ConnectionStateChange {
                state: ConnectionState::Disconnected,
                error: Some(error),
            });
        }
    }

    fn is_current(&self, id: u64) -> bool {
        self.lifecycle
            .lock()
            .active()
            .is_some_and(|active| active.id == id)
    }

    fn is_current_connected(&self, id: u64) -> bool {
        matches!(&*self.lifecycle.lock(), Lifecycle::Connected(active) if active.id == id)
    }
}

struct TransportHandlers {
    host: Weak<dyn ConnectionHost>,
    id: u64,
}

impl TransportHandlers {
    fn host(&self) -> Option<Arc<dyn ConnectionHost>> {
        self.host.upgrade()
    }
}

impl ByteTransportHandlers for TransportHandlers {
    fn on_data(&self, chunk: &[u8]) {
        if let Some(host) = self.host() {
            host.connection().handle_data(self.id, chunk);
        }
    }

    fn on_close(&self) {
        if let Some(host) = self.host() {
            let connection = host.connection();
            if connection.is_current(self.id) {
                connection.handle_close();
            }
        }
    }

    fn on_error(&self, error: TransportError) {
        if let Some(host) = self.host() {
            let connection = host.connection();
            if connection.is_current(self.id) {
                connection.fail_and_close(error.into());
            }
        }
    }
}
