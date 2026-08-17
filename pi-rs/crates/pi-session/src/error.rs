//! Session errors. Port of `SessionError` (`session/types.ts`) and
//! `JsonlDecodeError` (`session/jsonl/errors.ts`).
//!
//! Flat, code-tagged enum per the workspace convention: FFI callers match on
//! [`SessionError::code`], which mirrors upstream's `SessionErrorCode` strings
//! exactly because the conformance suite asserts on them.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum SessionError {
    #[error("{message}")]
    NotFound { message: String },
    #[error("{message}")]
    AlreadyExists { message: String },
    #[error("{message}")]
    InvalidEntry { message: String },
    #[error("{message}")]
    InvalidPayload { message: String },
    #[error("{message}")]
    InvalidLane { message: String },
    #[error("{message}")]
    InvalidQuery { message: String },
    #[error("{message}")]
    InvalidForkTarget { message: String },
    #[error("{message}")]
    Storage { message: String },
}

macro_rules! ctor {
    ($name:ident, $variant:ident) => {
        pub fn $name(message: impl Into<String>) -> Self {
            SessionError::$variant {
                message: message.into(),
            }
        }
    };
}

impl SessionError {
    ctor!(not_found, NotFound);
    ctor!(already_exists, AlreadyExists);
    ctor!(invalid_entry, InvalidEntry);
    ctor!(invalid_payload, InvalidPayload);
    ctor!(invalid_lane, InvalidLane);
    ctor!(invalid_query, InvalidQuery);
    ctor!(invalid_fork_target, InvalidForkTarget);
    ctor!(storage, Storage);

    /// Stable machine-readable code, identical to upstream's `SessionErrorCode`.
    pub fn code(&self) -> &'static str {
        match self {
            SessionError::NotFound { .. } => "not_found",
            SessionError::AlreadyExists { .. } => "already_exists",
            SessionError::InvalidEntry { .. } => "invalid_entry",
            SessionError::InvalidPayload { .. } => "invalid_payload",
            SessionError::InvalidLane { .. } => "invalid_lane",
            SessionError::InvalidQuery { .. } => "invalid_query",
            SessionError::InvalidForkTarget { .. } => "invalid_fork_target",
            SessionError::Storage { .. } => "storage",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            SessionError::NotFound { message }
            | SessionError::AlreadyExists { message }
            | SessionError::InvalidEntry { message }
            | SessionError::InvalidPayload { message }
            | SessionError::InvalidLane { message }
            | SessionError::InvalidQuery { message }
            | SessionError::InvalidForkTarget { message }
            | SessionError::Storage { message } => message,
        }
    }
}

pub type SessionResult<T> = Result<T, SessionError>;

/// Why a JSONL line could not be decoded. `Syntax` is load-bearing: the storage
/// loader treats a syntax error on the *final* line as a torn tail from an
/// unacknowledged append and repairs the file instead of failing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonlDecodeErrorKind {
    Syntax,
    Schema,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct JsonlDecodeError {
    pub kind: JsonlDecodeErrorKind,
    pub message: String,
}

impl JsonlDecodeError {
    pub fn syntax(message: impl Into<String>) -> Self {
        Self {
            kind: JsonlDecodeErrorKind::Syntax,
            message: message.into(),
        }
    }

    pub fn schema(message: impl Into<String>) -> Self {
        Self {
            kind: JsonlDecodeErrorKind::Schema,
            message: message.into(),
        }
    }
}

/// `invalidFile` from `jsonl/errors.ts`; the message shape is asserted upstream.
pub fn invalid_file(path: &str, line: usize, cause: &JsonlDecodeError) -> SessionError {
    SessionError::invalid_entry(format!(
        "Invalid JSONL v4 session {path}: line {line} {}",
        cause.message
    ))
}
