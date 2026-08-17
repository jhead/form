//! Port of `.upstream/packages/server/src/connection.ts`.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use parking_lot::Mutex;
use pi_protocol::ClientMessageDecoder;

use crate::errors::TransportError;

pub type SendFuture = Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'static>>;
pub type CloseFuture = Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'static>>;

/// An established, authorized ordered byte connection.
///
/// `send` returns an eagerly-queued future for the same reason the client's
/// transport does: upstream's pending-byte budget is charged at call time.
pub trait ByteConnection: Send + Sync + 'static {
    fn closed(&self) -> bool;
    fn send(&self, chunk: Vec<u8>) -> SendFuture;
    fn close(&self, final_chunk: Option<Vec<u8>>) -> CloseFuture;
}

pub trait ByteConnectionHandler: Send + Sync + 'static {
    fn on_data(&self, chunk: &[u8]);
    fn on_close(&self);
    fn on_error(&self, error: TransportError);
}

/// Supplies a handler for each established byte connection.
pub trait ByteConnectionAcceptor: Send + Sync + 'static {
    fn accept(&self, connection: Arc<dyn ByteConnection>) -> Arc<dyn ByteConnectionHandler>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStage {
    AwaitingHello,
    Handshaking,
    Ready,
    Closing,
    Closed,
}

/// A completion other tasks can await; the handshake gate uses it to defer
/// requests that arrive while the server hello is still being produced.
pub(crate) type Completion = futures::future::Shared<futures::channel::oneshot::Receiver<()>>;

pub(crate) struct ConnectionMutable {
    pub(crate) session_ids: HashSet<String>,
    pub(crate) stage: ConnectionStage,
    pub(crate) disconnected: bool,
    pub(crate) handshake_complete: bool,
    pub(crate) handshake: Option<Completion>,
    pub(crate) handshake_timeout: Option<tokio::task::JoinHandle<()>>,
}

pub(crate) struct ConnectionState {
    pub(crate) id: String,
    pub(crate) connection: Arc<dyn ByteConnection>,
    pub(crate) decoder: Mutex<ClientMessageDecoder>,
    pub(crate) inner: Mutex<ConnectionMutable>,
}

impl ConnectionState {
    pub(crate) fn stage(&self) -> ConnectionStage {
        self.inner.lock().stage
    }

    pub(crate) fn set_stage(&self, stage: ConnectionStage) {
        self.inner.lock().stage = stage;
    }

    pub(crate) fn disconnected(&self) -> bool {
        self.inner.lock().disconnected
    }

    /// Upstream's `isTerminalConnection`.
    pub(crate) fn is_terminal(&self) -> bool {
        let inner = self.inner.lock();
        inner.disconnected
            || inner.stage == ConnectionStage::Closing
            || inner.stage == ConnectionStage::Closed
    }

    pub(crate) fn clear_handshake_timeout(&self) {
        if let Some(handle) = self.inner.lock().handshake_timeout.take() {
            handle.abort();
        }
    }

    pub(crate) fn is_attached(&self, session_id: &str) -> bool {
        self.inner.lock().session_ids.contains(session_id)
    }
}
