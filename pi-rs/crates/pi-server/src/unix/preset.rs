//! Port of `.upstream/packages/server/src/transports/unix/preset.ts`.

use std::sync::Arc;

use crate::errors::TransportError;
use crate::server::PiServer;
use crate::types::{PiServerOptions, SessionService};
use crate::unix::listener::create_unix_listener;
use crate::unix::types::{UnixListenerOptions, UnixServerOptions};

/// Composes `PiServer` with one Unix-domain socket listener.
pub fn create_unix_server(
    service: Arc<dyn SessionService>,
    options: UnixServerOptions,
) -> Result<PiServer, TransportError> {
    let listener = create_unix_listener(UnixListenerOptions {
        path: options.path,
        mode: options.mode,
        max_pending_bytes: options.max_pending_bytes,
        graceful_close_timeout_ms: options.graceful_close_timeout_ms,
        max_frame_length: options.max_frame_length,
        on_error: options.on_error.clone(),
    })?;
    PiServer::new(
        service,
        PiServerOptions {
            listeners: vec![listener],
            max_frame_length: options.max_frame_length,
            handshake_timeout_ms: options.handshake_timeout_ms,
            server_id: options.server_id,
            on_error: options.on_error,
        },
    )
}
