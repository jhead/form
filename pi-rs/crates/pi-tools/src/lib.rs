//! The built-in tool set and the filesystem/shell abstractions it runs on.
//!
//! Rust port of `.upstream/packages/agent/src/harness/{tools,env,utils}` plus
//! the glob/content search tools.
//!
//! ## Layout
//!
//! - [`types`] — the [`FileSystem`], [`Shell`] and [`ExecutionEnv`] capability
//!   traits. These are the Swift host's substitution points, so they are
//!   object-safe and used as `Arc<dyn Trait>`.
//! - [`local`] — the real implementations ([`LocalFileSystem`], [`LocalShell`],
//!   [`LocalExecutionEnv`]).
//! - [`memory`] — an in-memory environment for tests, with hooks for blocking a
//!   write or scripting a shell.
//! - [`tool`] — the [`AgentTool`] trait, [`ToolResult`] and [`ToolContext`].
//! - [`tools`] — bash, read, write, edit, find, grep.
//! - [`edit_diff`], [`truncate`], [`shell_output`], [`path_utils`],
//!   [`file_mutation_queue`], [`search`] — the supporting machinery.
//!
//! ## Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use pi_tools::{AgentTool, ExecutionEnvRef, LocalExecutionEnv, ToolContext};
//! use pi_tools::tools::read::ReadTool;
//!
//! # async fn run() -> Result<(), pi_tools::ToolError> {
//! let env: ExecutionEnvRef = Arc::new(LocalExecutionEnv::new("/tmp/project"));
//! let context = ToolContext::new(env).with_tool_call_id("call-1");
//! let result = ReadTool::new()
//!     .execute(serde_json::json!({ "path": "README.md" }), &context, None)
//!     .await?;
//! println!("{}", result.text_output());
//! # Ok(())
//! # }
//! ```

pub mod edit_diff;
pub mod error;
pub mod file_mutation_queue;
pub mod local;
pub mod memory;
pub mod path_utils;
pub mod search;
pub mod shell_output;
pub mod tool;
pub mod tools;
pub mod truncate;
pub mod types;

pub use error::{
    ExecResult, ExecutionError, ExecutionErrorCode, FileError, FileErrorCode, FileResult, ToolError,
};
pub use file_mutation_queue::with_file_mutation_queue;
pub use local::{LocalExecutionEnv, LocalFileSystem, LocalShell};
pub use memory::MemoryExecutionEnv;
pub use path_utils::{resolve_read_tool_path, resolve_tool_path};
pub use shell_output::{
    execute_shell_with_capture, ShellCaptureOptions, ShellCaptureProgress, ShellCaptureResult,
};
pub use tool::{
    AgentTool, AgentToolRef, ToolContext, ToolExecutionMode, ToolResult, ToolUpdateCallback,
};
pub use tools::bash::{BashExecution, BashPrepare, BashTool, BashToolDetails, BashToolOptions};
pub use tools::default_tools;
pub use tools::edit::{EditTool, EditToolDetails};
pub use tools::find::{FindTool, FindToolDetails};
pub use tools::grep::{GrepTool, GrepToolDetails};
pub use tools::read::{
    ImageProcessor, ImageProcessorResult, ReadTool, ReadToolDetails, ReadToolOptions,
};
pub use tools::write::WriteTool;
pub use truncate::{
    format_size, truncate_head, truncate_line, truncate_tail, TruncatedBy, TruncationOptions,
    TruncationResult, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, GREP_MAX_LINE_LENGTH,
};
pub use types::{
    ExecutionEnv, ExecutionEnvRef, FileInfo, FileKind, FileSystem, OutputCallback, Shell,
    ShellExecOptions, ShellOutput,
};
