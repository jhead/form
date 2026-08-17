//! Port of `.upstream/packages/server/src/listener.ts`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::connection::ByteConnectionAcceptor;
use crate::errors::TransportError;

/// Supplies established byte connections after any required transport
/// authentication.
#[async_trait]
pub trait PiServerListener: Send + Sync + 'static {
    /// Human-readable bound address after startup, when the transport has one.
    fn address(&self) -> Option<String> {
        None
    }
    /// Starts listening and passes authorized connections to `accept`.
    async fn start(&self, accept: Arc<dyn ByteConnectionAcceptor>) -> Result<(), TransportError>;
    async fn close(&self) -> Result<(), TransportError>;
}
