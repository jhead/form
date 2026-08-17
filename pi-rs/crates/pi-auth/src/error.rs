//! Flat, code-tagged error for every public entry point in this crate.
//!
//! Upstream's `ModelsError` carries codes `"auth"` / `"oauth"`; those two are
//! preserved verbatim so callers that match on the code keep working. The extra
//! variants exist because Rust cannot smuggle a cancellation or a missing-UI
//! condition through a string the way a thrown `Error` does.

use pi_core::AiError;
use pi_http::HttpError;

#[derive(Debug, Clone, thiserror::Error)]
pub enum AuthError {
    /// The credential store could not be read or written.
    #[error("credential store error: {message}")]
    Store { message: String },

    /// API-key resolution failed (upstream `ModelsError` code `"auth"`).
    #[error("authentication error: {message}")]
    Auth { message: String },

    /// An OAuth login/refresh/derivation failed (upstream code `"oauth"`).
    #[error("oauth error: {message}")]
    OAuth { message: String },

    /// The host did not supply an [`crate::AuthInteraction`] able to serve this
    /// step. Headless callers get this instead of a blocked stdin read.
    #[error("interaction unavailable: {message}")]
    Interaction { message: String },

    /// Network/transport failure while talking to an OAuth endpoint.
    #[error("transport error: {message}")]
    Transport { message: String },

    /// Aborted through an `AbortSignal`, or the user cancelled a prompt.
    #[error("Login cancelled")]
    Cancelled,

    /// A device-code or login flow ran past its deadline.
    #[error("{message}")]
    TimedOut { message: String },
}

impl AuthError {
    pub fn store(message: impl Into<String>) -> Self {
        Self::Store {
            message: message.into(),
        }
    }

    pub fn auth(message: impl Into<String>) -> Self {
        Self::Auth {
            message: message.into(),
        }
    }

    pub fn oauth(message: impl Into<String>) -> Self {
        Self::OAuth {
            message: message.into(),
        }
    }

    pub fn interaction(message: impl Into<String>) -> Self {
        Self::Interaction {
            message: message.into(),
        }
    }

    pub fn transport(message: impl Into<String>) -> Self {
        Self::Transport {
            message: message.into(),
        }
    }

    pub fn timed_out(message: impl Into<String>) -> Self {
        Self::TimedOut {
            message: message.into(),
        }
    }

    /// Stable machine-readable code. FFI callers switch on this.
    pub fn code(&self) -> &'static str {
        match self {
            AuthError::Store { .. } => "store",
            AuthError::Auth { .. } => "auth",
            AuthError::OAuth { .. } => "oauth",
            AuthError::Interaction { .. } => "interaction",
            AuthError::Transport { .. } => "transport",
            AuthError::Cancelled => "cancelled",
            AuthError::TimedOut { .. } => "timeout",
        }
    }

    pub fn message(&self) -> String {
        self.to_string()
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self, AuthError::Cancelled)
    }
}

impl From<AuthError> for AiError {
    fn from(err: AuthError) -> Self {
        match err {
            AuthError::Cancelled => AiError::Aborted,
            AuthError::Transport { message } => AiError::Transport { message },
            AuthError::TimedOut { message } => AiError::Other { message },
            other => AiError::Auth {
                message: other.to_string(),
            },
        }
    }
}

impl From<HttpError> for AuthError {
    fn from(err: HttpError) -> Self {
        match err {
            HttpError::Aborted => AuthError::Cancelled,
            HttpError::Timeout(ms) => {
                AuthError::timed_out(format!("request timed out after {ms}ms"))
            }
            HttpError::Transport(message) => AuthError::transport(message),
            HttpError::InvalidRequest(message) => AuthError::auth(message),
            HttpError::Status {
                status, message, ..
            } => AuthError::oauth(format!("HTTP {status}: {message}")),
        }
    }
}
