//! Filesystem and shell capability traits.
//!
//! Port of the `FileSystem` / `Shell` / `ExecutionEnv` interfaces from
//! `.upstream/packages/agent/src/harness/types.ts`.
//!
//! These are the harness's extension points: the Swift host substitutes its own
//! sandboxed or remote implementation, so they are object-safe `#[async_trait]`
//! traits used behind `Arc<dyn Trait>`. Paths cross the boundary as `str`, never
//! `Path`, for the same reason.
//!
//! Every method returns a `Result` rather than panicking. Upstream states the
//! invariant explicitly ("operation methods must never throw"); in Rust that
//! means implementations must not panic and must encode backend failures in
//! [`crate::error::FileError`] /
//! [`crate::error::ExecutionError`].

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use pi_core::AbortSignal;
use serde::{Deserialize, Serialize};

use crate::error::{ExecResult, ExecutionError, FileResult};

/// Kind of filesystem object as addressed by a [`FileSystem`]. Symlinks are not followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileKind {
    File,
    Directory,
    Symlink,
}

/// Metadata for one filesystem object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    /// Basename of [`FileInfo::path`].
    pub name: String,
    /// Absolute, syntactically normalized path. Symlinks are not resolved.
    pub path: String,
    pub kind: FileKind,
    pub size: u64,
    /// Modification time in milliseconds since the Unix epoch.
    pub mtime_ms: i64,
}

/// Result of one shell command.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Streaming output sink. Returning `Err` makes [`Shell::exec`] fail with
/// [`crate::ExecutionErrorCode::CallbackError`], matching upstream's behaviour
/// when an `onStdout`/`onStderr` handler throws.
pub type OutputCallback = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// Options for [`Shell::exec`].
#[derive(Clone, Default)]
pub struct ShellExecOptions {
    /// Working directory. Relative paths resolve against the env cwd. Defaults to the env cwd.
    pub cwd: Option<String>,
    /// Extra environment variables. These win over inherited defaults.
    pub env: BTreeMap<String, String>,
    /// Whether to inherit the environment's default variables. Use
    /// [`ShellExecOptions::new`] to get the upstream default of `true`.
    pub inherit_env: bool,
    /// Timeout in seconds. `None` means no timeout.
    pub timeout_secs: Option<f64>,
    pub abort: Option<AbortSignal>,
    pub on_stdout: Option<OutputCallback>,
    pub on_stderr: Option<OutputCallback>,
}

impl ShellExecOptions {
    /// Upstream defaults: inherit the environment, no timeout, no callbacks.
    pub fn new() -> Self {
        Self {
            inherit_env: true,
            ..Default::default()
        }
    }

    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn with_timeout_secs(mut self, timeout: Option<f64>) -> Self {
        self.timeout_secs = timeout;
        self
    }

    pub fn with_abort(mut self, abort: Option<AbortSignal>) -> Self {
        self.abort = abort;
        self
    }

    pub fn is_aborted(&self) -> bool {
        self.abort.as_ref().is_some_and(|s| s.is_aborted())
    }
}

impl std::fmt::Debug for ShellExecOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellExecOptions")
            .field("cwd", &self.cwd)
            .field("env", &self.env)
            .field("inherit_env", &self.inherit_env)
            .field("timeout_secs", &self.timeout_secs)
            .finish_non_exhaustive()
    }
}

/// Filesystem capability used by the harness.
///
/// Paths may be absolute or relative to [`FileSystem::cwd`]. Returned paths are
/// absolute but not canonicalized through symlinks unless produced by
/// [`FileSystem::canonical_path`].
#[async_trait]
pub trait FileSystem: Send + Sync + 'static {
    /// Current working directory used to resolve relative paths.
    fn cwd(&self) -> &str;

    /// Absolutize without requiring the path to exist and without resolving symlinks.
    async fn absolute_path(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<String>;

    /// Join path segments in the filesystem namespace.
    async fn join_path(&self, parts: &[String], abort: Option<AbortSignal>) -> FileResult<String>;

    async fn read_text_file(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<String>;

    /// Read UTF-8 lines, stopping once `max_lines` lines have been read.
    async fn read_text_lines(
        &self,
        path: &str,
        max_lines: Option<usize>,
        abort: Option<AbortSignal>,
    ) -> FileResult<Vec<String>>;

    async fn read_binary_file(&self, path: &str, abort: Option<AbortSignal>)
        -> FileResult<Vec<u8>>;

    /// Create or overwrite a file, creating parent directories.
    async fn write_file(
        &self,
        path: &str,
        content: &[u8],
        abort: Option<AbortSignal>,
    ) -> FileResult<()>;

    /// Create or append to a file, creating parent directories.
    async fn append_file(
        &self,
        path: &str,
        content: &[u8],
        abort: Option<AbortSignal>,
    ) -> FileResult<()>;

    /// Rename a file, replacing the destination when it exists.
    async fn rename_file(
        &self,
        source_path: &str,
        destination_path: &str,
        abort: Option<AbortSignal>,
    ) -> FileResult<()>;

    /// Metadata for the addressed path, without following symlinks.
    async fn file_info(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<FileInfo>;

    /// Direct children of a directory, without following symlinks.
    async fn list_dir(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<Vec<FileInfo>>;

    async fn canonical_path(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<String>;

    /// `false` for missing paths; other failures return a [`crate::FileError`].
    async fn exists(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<bool>;

    async fn create_dir(
        &self,
        path: &str,
        recursive: bool,
        abort: Option<AbortSignal>,
    ) -> FileResult<()>;

    async fn remove(
        &self,
        path: &str,
        recursive: bool,
        force: bool,
        abort: Option<AbortSignal>,
    ) -> FileResult<()>;

    /// Create a temporary directory and return its absolute path.
    async fn create_temp_dir(
        &self,
        prefix: Option<&str>,
        abort: Option<AbortSignal>,
    ) -> FileResult<String>;

    /// Create a temporary file and return its absolute path.
    async fn create_temp_file(
        &self,
        prefix: Option<&str>,
        suffix: Option<&str>,
        abort: Option<AbortSignal>,
    ) -> FileResult<String>;

    /// Release filesystem resources. Best-effort; must not panic.
    async fn cleanup_files(&self) {}
}

/// Shell execution capability used by the harness.
#[async_trait]
pub trait Shell: Send + Sync + 'static {
    /// Execute a shell command. A non-zero exit is a successful [`ShellOutput`],
    /// not an error; only spawn/timeout/abort/callback failures are `Err`.
    async fn exec(&self, command: &str, options: ShellExecOptions) -> ExecResult<ShellOutput>;

    /// Release shell resources, terminating still-running children. Best-effort.
    async fn cleanup_shell(&self) {}
}

/// Filesystem plus process execution: what the built-in tools run against.
///
/// Blanket-implemented for anything that is both a [`FileSystem`] and a
/// [`Shell`], so implementors only write those two.
#[async_trait]
pub trait ExecutionEnv: FileSystem + Shell {
    /// Release every resource held by the environment.
    async fn cleanup(&self) {
        self.cleanup_files().await;
        self.cleanup_shell().await;
    }
}

#[async_trait]
impl<T: FileSystem + Shell> ExecutionEnv for T {}

/// Shared handle to an execution environment. This is what tools hold.
pub type ExecutionEnvRef = Arc<dyn ExecutionEnv>;

/// Reject a pre-aborted signal before doing any work.
pub(crate) fn check_abort(abort: &Option<AbortSignal>, path: Option<&str>) -> FileResult<()> {
    match abort {
        Some(signal) if signal.is_aborted() => {
            Err(crate::error::FileError::aborted(path.map(str::to_string)))
        }
        _ => Ok(()),
    }
}

/// Timeouts are seconds in the tool API but milliseconds in the runtime, and
/// upstream caps them at the maximum `setTimeout` delay. Kept for wire parity.
pub const MAX_TIMEOUT_MS: f64 = 2_147_483_647.0;
pub const MAX_TIMEOUT_SECONDS: f64 = MAX_TIMEOUT_MS / 1000.0;

pub(crate) fn resolve_timeout_ms(timeout_secs: Option<f64>) -> ExecResult<Option<f64>> {
    let Some(timeout) = timeout_secs else {
        return Ok(None);
    };
    if !timeout.is_finite() || timeout <= 0.0 {
        return Err(ExecutionError::new(
            crate::error::ExecutionErrorCode::Timeout,
            "Invalid timeout: must be a finite number of seconds",
        ));
    }
    let timeout_ms = timeout * 1000.0;
    if timeout_ms > MAX_TIMEOUT_MS {
        return Err(ExecutionError::new(
            crate::error::ExecutionErrorCode::Timeout,
            format!("Invalid timeout: maximum is {MAX_TIMEOUT_SECONDS} seconds"),
        ));
    }
    Ok(Some(timeout_ms))
}
