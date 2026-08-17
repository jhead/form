//! Port of `.upstream/packages/server/src/errors.ts`.

use pi_protocol::{JsonValue, ProtocolError, ProtocolErrorCode};

pub const INTERNAL_SERVER_ERROR_MESSAGE: &str = "Internal server error";
pub const NOT_IMPLEMENTED_MESSAGE: &str = "Operation is not implemented";

/// A service/runtime failure.
///
/// Upstream splits this in two: `PiServerError` (a safe, typed failure whose
/// message crosses the protocol boundary) and `InternalServerError` (an unsafe
/// failure whose cause is reported but never serialized). Both live here;
/// [`PiServerError::Internal`] is the unsafe one and always serializes as a
/// bare `internal_error`.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PiServerError {
    #[error("{message}")]
    Busy {
        message: String,
        details: Option<JsonValue>,
    },
    #[error("{message}")]
    SessionLocked {
        message: String,
        details: Option<JsonValue>,
    },
    #[error("{message}")]
    NotFound {
        message: String,
        details: Option<JsonValue>,
    },
    #[error("{message}")]
    InvalidRequest {
        message: String,
        details: Option<JsonValue>,
    },
    #[error("{NOT_IMPLEMENTED_MESSAGE}")]
    NotImplemented,
    /// The cause is retained for `on_error` reporting and never serialized.
    #[error("{INTERNAL_SERVER_ERROR_MESSAGE}")]
    Internal { cause: String },
}

impl PiServerError {
    pub fn busy(message: impl Into<String>) -> Self {
        Self::Busy {
            message: message.into(),
            details: None,
        }
    }

    pub fn session_locked(message: impl Into<String>) -> Self {
        Self::SessionLocked {
            message: message.into(),
            details: None,
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound {
            message: message.into(),
            details: None,
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::InvalidRequest {
            message: message.into(),
            details: None,
        }
    }

    /// Upstream's `new InternalServerError(cause)`.
    pub fn internal(cause: impl std::fmt::Display) -> Self {
        Self::Internal {
            cause: cause.to_string(),
        }
    }

    /// Stable identifier for FFI consumers; also the wire code.
    pub fn code(&self) -> &'static str {
        self.protocol_code().as_str()
    }

    pub fn protocol_code(&self) -> ProtocolErrorCode {
        match self {
            Self::Busy { .. } => ProtocolErrorCode::Busy,
            Self::SessionLocked { .. } => ProtocolErrorCode::SessionLocked,
            Self::NotFound { .. } => ProtocolErrorCode::NotFound,
            Self::InvalidRequest { .. } => ProtocolErrorCode::InvalidRequest,
            Self::NotImplemented => ProtocolErrorCode::NotImplemented,
            Self::Internal { .. } => ProtocolErrorCode::InternalError,
        }
    }

    /// The cause an `on_error` observer should see, when there is one that must
    /// not reach the client.
    pub fn private_cause(&self) -> Option<&str> {
        match self {
            Self::Internal { cause } => Some(cause),
            _ => None,
        }
    }

    /// Upstream's `PiServer#toProtocolError`.
    pub fn to_protocol_error(&self) -> ProtocolError {
        match self {
            Self::NotImplemented => {
                ProtocolError::new(ProtocolErrorCode::NotImplemented, NOT_IMPLEMENTED_MESSAGE)
            }
            Self::Internal { .. } => ProtocolError::new(
                ProtocolErrorCode::InternalError,
                INTERNAL_SERVER_ERROR_MESSAGE,
            ),
            Self::Busy { message, details }
            | Self::SessionLocked { message, details }
            | Self::NotFound { message, details }
            | Self::InvalidRequest { message, details } => ProtocolError {
                code: self.protocol_code(),
                message: message.clone(),
                details: details.clone(),
            },
        }
    }
}

/// A transport-level failure: listener startup, socket writes, socket close.
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

impl From<TransportError> for PiServerError {
    fn from(error: TransportError) -> Self {
        Self::internal(error.message)
    }
}
