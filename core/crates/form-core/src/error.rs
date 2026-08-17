//! Flat error enum with a stable machine-readable code. Never leak `anyhow` or
//! `Box<dyn Error>` across a crate boundary — the code is part of the wire contract.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("session not found: {0}")]
    SessionNotFound(String),

    #[error("group not found: {0}")]
    GroupNotFound(String),

    #[error("attachment not found: {0}")]
    AttachmentNotFound(String),

    #[error("attachment rejected: {reason}")]
    AttachmentRejected { reason: String },

    #[error("model not found: {0}")]
    ModelNotFound(String),

    #[error("path escapes workspace root: {0}")]
    PathEscapesRoot(String),

    #[error("a run is already active for session {0}")]
    RunAlreadyActive(String),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("io error: {0}")]
    Io(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl CoreError {
    /// Stable code carried on the wire. Swift switches on this, never on the message.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotImplemented(_) => "not_implemented",
            Self::InvalidRequest(_) => "invalid_request",
            Self::SessionNotFound(_) => "session_not_found",
            Self::GroupNotFound(_) => "group_not_found",
            Self::AttachmentNotFound(_) => "attachment_not_found",
            Self::AttachmentRejected { .. } => "attachment_rejected",
            Self::ModelNotFound(_) => "model_not_found",
            Self::PathEscapesRoot(_) => "path_escapes_root",
            Self::RunAlreadyActive(_) => "run_already_active",
            Self::Storage(_) => "storage",
            Self::Io(_) => "io",
            Self::Serialization(_) => "serialization",
            Self::Internal(_) => "internal",
        }
    }
}

impl From<serde_json::Error> for CoreError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serialization(e.to_string())
    }
}

impl From<std::io::Error> for CoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

impl From<rusqlite::Error> for CoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Storage(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, CoreError>;
