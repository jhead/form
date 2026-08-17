//! Port of `.upstream/packages/server/src/transports/unix/types.ts`.

use std::path::PathBuf;

use crate::types::ServerErrorHandler;

#[derive(Clone, Default)]
pub struct UnixListenerOptions {
    pub path: PathBuf,
    /// Socket filesystem permissions. Defaults to owner read/write only (0o600).
    pub mode: Option<u32>,
    /// Maximum framed bytes queued per connection before a slow peer is
    /// disconnected.
    pub max_pending_bytes: Option<u64>,
    pub graceful_close_timeout_ms: Option<u64>,
    /// Used to derive and validate `max_pending_bytes`. Must match the server
    /// when customized.
    pub max_frame_length: Option<u32>,
    pub on_error: Option<ServerErrorHandler>,
}

impl UnixListenerOptions {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            ..Self::default()
        }
    }
}

/// `UnixListenerOptions & Omit<PiServerOptions, "listeners">`.
#[derive(Clone, Default)]
pub struct UnixServerOptions {
    pub path: PathBuf,
    pub mode: Option<u32>,
    pub max_pending_bytes: Option<u64>,
    pub graceful_close_timeout_ms: Option<u64>,
    pub max_frame_length: Option<u32>,
    pub handshake_timeout_ms: Option<u64>,
    pub server_id: Option<String>,
    pub on_error: Option<ServerErrorHandler>,
}

impl UnixServerOptions {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            ..Self::default()
        }
    }

    pub fn with_max_frame_length(mut self, max_frame_length: u32) -> Self {
        self.max_frame_length = Some(max_frame_length);
        self
    }

    pub fn with_max_pending_bytes(mut self, max_pending_bytes: u64) -> Self {
        self.max_pending_bytes = Some(max_pending_bytes);
        self
    }

    pub fn with_handshake_timeout_ms(mut self, handshake_timeout_ms: u64) -> Self {
        self.handshake_timeout_ms = Some(handshake_timeout_ms);
        self
    }

    pub fn with_graceful_close_timeout_ms(mut self, graceful_close_timeout_ms: u64) -> Self {
        self.graceful_close_timeout_ms = Some(graceful_close_timeout_ms);
        self
    }

    pub fn with_server_id(mut self, server_id: impl Into<String>) -> Self {
        self.server_id = Some(server_id.into());
        self
    }

    pub fn with_error_handler(mut self, on_error: ServerErrorHandler) -> Self {
        self.on_error = Some(on_error);
        self
    }
}
