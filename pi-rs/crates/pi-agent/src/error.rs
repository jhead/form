//! Flat, code-tagged error type for the agent runtime.
//!
//! Upstream throws `Error` everywhere and the loop turns the message into an
//! error tool result. The port keeps that behaviour but routes it through a
//! typed enum so FFI callers get a stable `code()`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AgentError {
    /// A tool's `execute` failed. `message` is what upstream would have put in
    /// the error tool result.
    #[error("{message}")]
    Tool { message: String },

    /// Tool arguments did not satisfy the tool's JSON Schema.
    #[error("{message}")]
    Validation { message: String },

    /// The assistant asked for a tool the current context does not expose.
    #[error("Tool {name} not found")]
    ToolNotFound { name: String },

    /// `Agent` API misuse: prompting while a run is active, continuing from an
    /// assistant tail, resetting mid-run, ...
    #[error("{message}")]
    InvalidState { message: String },

    /// Cooperative cancellation through an [`pi_core::AbortSignal`].
    #[error("Operation aborted")]
    Aborted,

    /// No `StreamFn` was passed and none was installed as the process default.
    #[error("No default stream function configured. Pass streamFn explicitly or call set_default_stream_fn().")]
    NoStreamFn,

    /// The stream function rejected before producing a stream. Providers are
    /// contractually supposed to encode failures in the stream instead, so this
    /// is a programmer error on the adapter side.
    #[error("{message}")]
    Stream { code: String, message: String },
}

impl AgentError {
    pub fn tool(message: impl Into<String>) -> Self {
        AgentError::Tool {
            message: message.into(),
        }
    }

    pub fn validation(message: impl Into<String>) -> Self {
        AgentError::Validation {
            message: message.into(),
        }
    }

    pub fn invalid_state(message: impl Into<String>) -> Self {
        AgentError::InvalidState {
            message: message.into(),
        }
    }

    /// Stable machine-readable code. Do not change these strings.
    pub fn code(&self) -> &'static str {
        match self {
            AgentError::Tool { .. } => "tool",
            AgentError::Validation { .. } => "validation",
            AgentError::ToolNotFound { .. } => "tool_not_found",
            AgentError::InvalidState { .. } => "invalid_state",
            AgentError::Aborted => "aborted",
            AgentError::NoStreamFn => "no_stream_fn",
            AgentError::Stream { .. } => "stream",
        }
    }

    /// The text upstream would surface as `error.message`.
    pub fn message(&self) -> String {
        self.to_string()
    }
}

impl From<pi_core::AiError> for AgentError {
    fn from(e: pi_core::AiError) -> Self {
        AgentError::Stream {
            code: e.code().to_string(),
            message: e.message(),
        }
    }
}
