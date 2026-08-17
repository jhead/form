//! Server half of the session protocol: session registry, subscriptions and
//! the Unix-socket listener.
//!
//! Port of `.upstream/packages/server/src/`. The wire types come from
//! `pi-protocol`; this crate owns the connection handshake, request routing,
//! the live-session registry and snapshot publication.
//!
//! The only dependency on the agent/session runtime is [`SessionService`] and
//! [`SessionRuntime`] — narrow, object-safe traits this crate owns. Anything
//! that can produce a [`pi_protocol::SessionSnapshot`] can serve this protocol.

pub mod connection;
pub mod errors;
pub mod listener;
pub mod protocol;
mod server;
mod sessions;
mod snapshots;
pub mod testing;
pub mod types;
#[cfg(unix)]
pub mod unix;

pub use connection::{
    ByteConnection, ByteConnectionAcceptor, ByteConnectionHandler, CloseFuture, ConnectionStage,
    SendFuture,
};
pub use errors::{
    PiServerError, TransportError, INTERNAL_SERVER_ERROR_MESSAGE, NOT_IMPLEMENTED_MESSAGE,
};
pub use listener::PiServerListener;
pub use server::PiServer;
pub use types::{
    CreateSessionOptions, PiServerOptions, PromptInput, RuntimeEventListener, ServerErrorHandler,
    ServerErrorReport, SessionRuntime, SessionRuntimeEvent, SessionService, SteerInput,
    Unsubscribe,
};
#[cfg(unix)]
pub use unix::{create_unix_listener, create_unix_server, UnixListenerOptions, UnixServerOptions};
