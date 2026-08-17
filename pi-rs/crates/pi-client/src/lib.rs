//! Client half of the session protocol, including the Unix-socket transport.
//!
//! Port of `.upstream/packages/client/src/`. The wire types come from
//! `pi-protocol`; this crate owns the connection lifecycle, request
//! correlation, the client-side snapshot cache and the session lease rules.
//!
//! ```no_run
//! use std::sync::Arc;
//! use pi_client::{PiClient, PiClientOptions, UnixTransportFactory, UnixTransportOptions};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let factory = UnixTransportFactory::new(UnixTransportOptions::new("/tmp/pi.sock"))?;
//! let client = PiClient::connect_new(PiClientOptions::new(factory)).await?;
//! let sessions = client.list_sessions().await?;
//! # let _ = sessions;
//! # Ok(())
//! # }
//! ```

mod client;
mod connection;
mod errors;
mod session_handle;
mod state;
mod transport;
mod types;
#[cfg(unix)]
mod unix;

pub use client::PiClient;
pub use errors::{PiClientError, TransportError, DISCONNECTED_MESSAGE};
pub use session_handle::{AcquireSessionOptions, PiSessionHandle, SessionLease, SessionLeaseMode};
pub use transport::{
    ByteTransport, ByteTransportFactory, ByteTransportHandlers, ConnectFuture, SendFuture,
};
pub use types::{
    ConnectionState, ConnectionStateChange, CreateSessionOptions, Listener, ListenerError,
    ListenerErrorHandler, PiClientOptions, Unsubscribe,
};
#[cfg(unix)]
pub use unix::{
    validate_unix_socket_path, UnixTransportFactory, UnixTransportOptions,
    MAX_UNIX_SOCKET_PATH_BYTES,
};
