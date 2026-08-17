//! Port of `.upstream/packages/client/src/errors.ts`.
//!
//! Upstream has one `Error` subclass per failure mode. AGENTS.md requires a
//! flat enum with a stable `code()` instead, so the subclasses become variants
//! and `error instanceof PiDisconnectedError` becomes a `matches!`.

use pi_protocol::{JsonValue, ProtocolError, ProtocolErrorCode, ProtocolValidationError};

/// The default message of upstream's `PiDisconnectedError`.
pub const DISCONNECTED_MESSAGE: &str = "Pi client is disconnected";

/// A transport failure reported by a [`crate::ByteTransport`] implementation.
///
/// Transports are an extension point, so they get their own error rather than
/// constructing a [`PiClientError`] the client would have to re-interpret.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct TransportError {
    pub message: String,
}

impl TransportError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl From<std::io::Error> for TransportError {
    fn from(error: std::io::Error) -> Self {
        Self::new(error.to_string())
    }
}

/// Everything a `PiClient` call can fail with.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PiClientError {
    /// Upstream `PiServerError`: a `ProtocolError` the server sent back.
    #[error("{message}")]
    Server {
        code: ProtocolErrorCode,
        message: String,
        details: Option<JsonValue>,
    },

    /// Upstream `PiDisconnectedError`.
    #[error("{0}")]
    Disconnected(String),

    /// Upstream `PiClientDisposedError`.
    #[error("Pi client is disposed")]
    Disposed,

    /// Upstream `PiSessionOwnershipError`.
    #[error("{message}")]
    SessionOwnership { session_id: String, message: String },

    /// Upstream `PiSessionDetachedError`.
    #[error("Session {session_id} is not attached")]
    SessionDetached { session_id: String },

    /// A framing/CBOR/schema failure from `pi-protocol`.
    #[error("{0}")]
    Protocol(#[from] ProtocolValidationError),

    /// A protocol violation the client detected itself (unexpected handshake
    /// ordering, a response with no matching request, …). Upstream raises
    /// `ProtocolValidationError` with a free-form message for these; the
    /// `pi-protocol` error is a closed enum, so they get their own variant with
    /// upstream's message text preserved.
    #[error("{0}")]
    ProtocolViolation(String),

    /// Rejected constructor options. Upstream throws `TypeError`.
    #[error("{0}")]
    InvalidOptions(String),
}

impl PiClientError {
    /// Stable identifier for FFI consumers.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Server { .. } => "server",
            Self::Disconnected(_) => "disconnected",
            Self::Disposed => "disposed",
            Self::SessionOwnership { .. } => "session_ownership",
            Self::SessionDetached { .. } => "session_detached",
            Self::Protocol(_) => "protocol",
            Self::ProtocolViolation(_) => "protocol_violation",
            Self::InvalidOptions(_) => "invalid_options",
        }
    }

    /// The wire error code, when this came back from the server.
    pub fn server_code(&self) -> Option<ProtocolErrorCode> {
        match self {
            Self::Server { code, .. } => Some(*code),
            _ => None,
        }
    }

    pub fn disconnected() -> Self {
        Self::Disconnected(DISCONNECTED_MESSAGE.to_string())
    }

    pub fn is_disconnected(&self) -> bool {
        matches!(self, Self::Disconnected(_))
    }
}

impl From<ProtocolError> for PiClientError {
    fn from(error: ProtocolError) -> Self {
        Self::Server {
            code: error.code,
            message: error.message,
            details: error.details,
        }
    }
}

/// Upstream's `toDisconnectedError`: keep an existing disconnection reason,
/// otherwise wrap the cause's message.
impl From<TransportError> for PiClientError {
    fn from(error: TransportError) -> Self {
        Self::Disconnected(error.message)
    }
}
