//! Backend-independent error types for the filesystem, shell and tool layers.
//!
//! Port of the `FileError` / `ExecutionError` classes in
//! `.upstream/packages/agent/src/harness/types.ts`. Upstream throws these; the
//! Rust port returns them in `Result`, which is the same contract expressed
//! natively ("operation methods must never throw").
//!
//! Like [`pi_core::AiError`] these are flat, `serde`-derivable enums with a
//! stable `code()` string, because Swift callers match on the code.

use serde::{Deserialize, Serialize};

/// Stable, backend-independent file error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileErrorCode {
    Aborted,
    NotFound,
    PermissionDenied,
    NotDirectory,
    IsDirectory,
    Invalid,
    NotSupported,
    Unknown,
}

impl FileErrorCode {
    /// Stable machine-readable code. Matches the TypeScript `FileErrorCode` strings.
    pub fn as_str(self) -> &'static str {
        match self {
            FileErrorCode::Aborted => "aborted",
            FileErrorCode::NotFound => "not_found",
            FileErrorCode::PermissionDenied => "permission_denied",
            FileErrorCode::NotDirectory => "not_directory",
            FileErrorCode::IsDirectory => "is_directory",
            FileErrorCode::Invalid => "invalid",
            FileErrorCode::NotSupported => "not_supported",
            FileErrorCode::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for FileErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned by [`crate::FileSystem`] operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[error("{message}")]
pub struct FileError {
    pub code: FileErrorCode,
    pub message: String,
    /// Absolute addressed path associated with the failure, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl FileError {
    pub fn new(code: FileErrorCode, message: impl Into<String>, path: Option<String>) -> Self {
        Self {
            code,
            message: message.into(),
            path,
        }
    }

    pub fn aborted(path: Option<String>) -> Self {
        Self::new(FileErrorCode::Aborted, "aborted", path)
    }

    pub fn not_found(message: impl Into<String>, path: Option<String>) -> Self {
        Self::new(FileErrorCode::NotFound, message, path)
    }

    pub fn code(&self) -> &'static str {
        self.code.as_str()
    }

    /// Map a `std::io::Error` onto the backend-independent codes, the way
    /// upstream's `toFileError` maps Node errno strings.
    pub fn from_io(error: &std::io::Error, path: Option<String>) -> Self {
        use std::io::ErrorKind;
        let code = match error.kind() {
            ErrorKind::NotFound => FileErrorCode::NotFound,
            ErrorKind::PermissionDenied => FileErrorCode::PermissionDenied,
            ErrorKind::IsADirectory => FileErrorCode::IsDirectory,
            ErrorKind::NotADirectory => FileErrorCode::NotDirectory,
            ErrorKind::InvalidInput | ErrorKind::InvalidData => FileErrorCode::Invalid,
            ErrorKind::Unsupported => FileErrorCode::NotSupported,
            ErrorKind::Interrupted => FileErrorCode::Aborted,
            _ => match error.raw_os_error() {
                // `IsADirectory` / `NotADirectory` are only surfaced on some
                // platforms, so fall back to the raw errno.
                Some(libc_code) => match libc_code {
                    20 => FileErrorCode::NotDirectory, // ENOTDIR
                    21 => FileErrorCode::IsDirectory,  // EISDIR
                    22 => FileErrorCode::Invalid,      // EINVAL
                    39 | 66 => FileErrorCode::Invalid, // ENOTEMPTY (linux | macos)
                    _ => FileErrorCode::Unknown,
                },
                None => FileErrorCode::Unknown,
            },
        };
        Self::new(code, error.to_string(), path)
    }
}

/// Stable, backend-independent execution error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionErrorCode {
    Aborted,
    Timeout,
    ShellUnavailable,
    SpawnError,
    CallbackError,
    Unknown,
}

impl ExecutionErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionErrorCode::Aborted => "aborted",
            ExecutionErrorCode::Timeout => "timeout",
            ExecutionErrorCode::ShellUnavailable => "shell_unavailable",
            ExecutionErrorCode::SpawnError => "spawn_error",
            ExecutionErrorCode::CallbackError => "callback_error",
            ExecutionErrorCode::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for ExecutionErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned by [`crate::Shell::exec`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[error("{message}")]
pub struct ExecutionError {
    pub code: ExecutionErrorCode,
    pub message: String,
}

impl ExecutionError {
    pub fn new(code: ExecutionErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn aborted() -> Self {
        Self::new(ExecutionErrorCode::Aborted, "aborted")
    }

    pub fn code(&self) -> &'static str {
        self.code.as_str()
    }
}

/// Failure of a tool invocation.
///
/// Upstream tools `throw`; the agent loop catches and turns the message into an
/// error tool result. The port returns this enum instead, and
/// [`ToolError::message`] is what ends up in the tool result text.
#[derive(Debug, Clone, PartialEq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ToolError {
    /// Arguments did not match the tool's JSON Schema, or failed a tool-specific check.
    #[error("{message}")]
    InvalidArguments { message: String },
    /// The call was cancelled through its [`pi_core::AbortSignal`].
    #[error("Operation aborted")]
    Aborted,
    #[error("{0}")]
    File(#[from] FileError),
    #[error("{0}")]
    Execution(#[from] ExecutionError),
    /// Anything else the tool wants to report to the model.
    #[error("{message}")]
    Failed { message: String },
}

impl ToolError {
    pub fn invalid_arguments(message: impl Into<String>) -> Self {
        ToolError::InvalidArguments {
            message: message.into(),
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        ToolError::Failed {
            message: message.into(),
        }
    }

    /// Stable machine-readable code. Do not change these strings.
    pub fn code(&self) -> &'static str {
        match self {
            ToolError::InvalidArguments { .. } => "invalid_arguments",
            ToolError::Aborted => "aborted",
            ToolError::File(e) => e.code(),
            ToolError::Execution(e) => e.code(),
            ToolError::Failed { .. } => "failed",
        }
    }

    pub fn message(&self) -> String {
        self.to_string()
    }

    pub fn is_aborted(&self) -> bool {
        match self {
            ToolError::Aborted => true,
            ToolError::File(e) => e.code == FileErrorCode::Aborted,
            ToolError::Execution(e) => e.code == ExecutionErrorCode::Aborted,
            _ => false,
        }
    }
}

/// `Result<T, FileError>`, the return type of every [`crate::FileSystem`] method.
pub type FileResult<T> = Result<T, FileError>;
/// `Result<T, ExecutionError>`, the return type of [`crate::Shell::exec`].
pub type ExecResult<T> = Result<T, ExecutionError>;
/// `Result<T, ToolError>`, the return type of [`crate::AgentTool::execute`].
pub type ToolResultOf<T> = Result<T, ToolError>;
