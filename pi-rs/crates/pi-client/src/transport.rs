//! Port of `.upstream/packages/client/src/transport.ts`.
//!
//! The three traits are the client's transport extension point, so they are
//! object-safe and used behind `Arc<dyn _>` (AGENTS.md rule 4).
//!
//! `send` deliberately returns a boxed future instead of being an
//! `#[async_trait]` method: upstream relies on the call having *eager* effect
//! (a queued write reserves its share of the pending-byte budget before the
//! caller awaits anything), and an `async fn` body would not run until polled.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::errors::TransportError;

/// Result of a queued write. `'static`, so no lifetime appears in the trait.
pub type SendFuture = Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'static>>;

/// Result of establishing a transport.
pub type ConnectFuture =
    Pin<Box<dyn Future<Output = Result<Arc<dyn ByteTransport>, TransportError>> + Send + 'static>>;

pub trait ByteTransport: Send + Sync + 'static {
    /// Queues one byte chunk. Calls must be delivered in invocation order.
    fn send(&self, chunk: Vec<u8>) -> SendFuture;
    /// Closes the transport. Repeated calls must be harmless.
    fn close(&self);
}

pub trait ByteTransportHandlers: Send + Sync + 'static {
    /// Delivers an arbitrary inbound byte chunk.
    fn on_data(&self, chunk: &[u8]);
    /// Reports an orderly terminal close.
    fn on_close(&self);
    /// Reports a terminal transport failure.
    fn on_error(&self, error: TransportError);
}

/// Creates a fresh connected, authenticated transport. Exactly one terminal
/// handler call is expected per transport.
pub trait ByteTransportFactory: Send + Sync + 'static {
    fn connect(&self, handlers: Arc<dyn ByteTransportHandlers>) -> ConnectFuture;
}
