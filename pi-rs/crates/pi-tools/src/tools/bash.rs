//! The `bash` tool.
//!
//! Port of `.upstream/packages/agent/src/harness/tools/bash.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use parking_lot::Mutex;
use pi_core::AbortSignal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{ExecutionErrorCode, ToolError, ToolResultOf};
use crate::shell_output::{execute_shell_with_capture, ShellCaptureOptions, ShellCaptureProgress};
use crate::tool::{parse_args, AgentTool, ToolContext, ToolResult};
use crate::truncate::{
    format_size, TruncatedBy, TruncationResult, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES,
};
use crate::types::MAX_TIMEOUT_SECONDS;

/// Upstream coalesces streamed output updates on a 100ms timer.
const BASH_UPDATE_THROTTLE: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashToolInput {
    pub command: String,
    /// Timeout in seconds. No default.
    #[serde(default)]
    pub timeout: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashToolDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
}

/// The command as it will actually be run. A [`BashPrepare`] hook may rewrite it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashExecution {
    pub command: String,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub inherit_env: bool,
}

/// Hook that adjusts the command, working directory or environment per call.
///
/// Upstream passes the application's turn context; here the hook receives the
/// [`ToolContext`] and can close over whatever else it needs.
#[async_trait]
pub trait BashPrepare: Send + Sync + 'static {
    async fn prepare(
        &self,
        execution: &mut BashExecution,
        context: &ToolContext,
        abort: Option<AbortSignal>,
    ) -> ToolResultOf<()>;
}

#[derive(Clone, Default)]
pub struct BashToolOptions {
    /// Prepended to every command, separated by a newline.
    pub command_prefix: Option<String>,
    pub prepare: Option<Arc<dyn BashPrepare>>,
}

impl std::fmt::Debug for BashToolOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BashToolOptions")
            .field("command_prefix", &self.command_prefix)
            .field("has_prepare", &self.prepare.is_some())
            .finish()
    }
}

#[derive(Clone, Default)]
pub struct BashTool {
    options: BashToolOptions,
}

impl BashTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_options(options: BashToolOptions) -> Self {
        Self { options }
    }
}

fn validate_timeout(timeout: Option<f64>) -> ToolResultOf<()> {
    let Some(timeout) = timeout else {
        return Ok(());
    };
    if !timeout.is_finite() || timeout <= 0.0 {
        return Err(ToolError::invalid_arguments(
            "Invalid timeout: must be a finite number of seconds",
        ));
    }
    if timeout > MAX_TIMEOUT_SECONDS {
        return Err(ToolError::invalid_arguments(format!(
            "Invalid timeout: maximum is {MAX_TIMEOUT_SECONDS} seconds"
        )));
    }
    Ok(())
}

fn update_from_progress(progress: &ShellCaptureProgress) -> ToolResult {
    let details = BashToolDetails {
        truncation: progress
            .truncation
            .truncated
            .then(|| progress.truncation.clone()),
        full_output_path: progress.full_output_path.clone(),
    };
    ToolResult::text(progress.output.clone())
        .with_details(Some(serde_json::to_value(details).unwrap_or(Value::Null)))
}

#[async_trait]
impl AgentTool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> String {
        format!(
            "Execute a bash command in the current working directory. Returns stdout and stderr. \
Output is truncated to last {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). If truncated, \
full output is saved to a temp file. Optionally provide a timeout in seconds.",
            DEFAULT_MAX_BYTES / 1024
        )
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Bash command to execute" },
                "timeout": { "type": "number", "description": "Timeout in seconds (optional, no default timeout)" }
            },
            "required": ["command"]
        })
    }

    async fn execute(
        &self,
        args: Value,
        context: &ToolContext,
        abort: Option<AbortSignal>,
    ) -> ToolResultOf<ToolResult> {
        let input: BashToolInput = parse_args("bash", args)?;
        validate_timeout(input.timeout)?;

        let mut execution = BashExecution {
            command: match &self.options.command_prefix {
                Some(prefix) => format!("{prefix}\n{}", input.command),
                None => input.command.clone(),
            },
            cwd: context.env.cwd().to_string(),
            env: BTreeMap::new(),
            inherit_env: true,
        };
        if let Some(prepare) = &self.options.prepare {
            prepare
                .prepare(&mut execution, context, abort.clone())
                .await?;
        }

        // Upstream opens with an empty update so the UI can show the tool running.
        context.emit_update(ToolResult::default());

        let last_update_at: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
        let on_chunk = context.on_update.clone().map(|on_update| {
            let last_update_at = last_update_at.clone();
            Arc::new(move |_chunk: &str, progress: &ShellCaptureProgress| {
                let now = Instant::now();
                let mut last = last_update_at.lock();
                let due = last.is_none_or(|at| now.duration_since(at) >= BASH_UPDATE_THROTTLE);
                if !due {
                    return;
                }
                *last = Some(now);
                drop(last);
                on_update(update_from_progress(progress));
            }) as Arc<dyn Fn(&str, &ShellCaptureProgress) + Send + Sync>
        });

        let capture = execute_shell_with_capture(
            &context.env,
            &execution.command,
            ShellCaptureOptions {
                cwd: Some(execution.cwd.clone()),
                env: execution.env.clone(),
                inherit_env: execution.inherit_env,
                timeout_secs: input.timeout,
                abort: abort.clone(),
                on_chunk,
                return_execution_errors: true,
            },
        )
        .await?;

        // Always flush a final update with the settled state.
        let final_progress = ShellCaptureProgress {
            output: capture.output.clone(),
            truncation: capture.truncation.clone(),
            full_output_path: capture.full_output_path.clone(),
            last_line_bytes: capture.last_line_bytes,
        };
        context.emit_update(update_from_progress(&final_progress));

        let mut output_text = capture.output.clone();
        let mut details: Option<BashToolDetails> = None;
        if capture.truncation.truncated {
            details = Some(BashToolDetails {
                truncation: Some(capture.truncation.clone()),
                full_output_path: capture.full_output_path.clone(),
            });
            let start_line = capture
                .truncation
                .total_lines
                .saturating_sub(capture.truncation.output_lines)
                + 1;
            let end_line = capture.truncation.total_lines;
            let full_output_path = capture.full_output_path.clone().unwrap_or_default();
            if capture.truncation.last_line_partial {
                let last_line_size = format_size(capture.last_line_bytes);
                output_text.push_str(&format!(
                    "\n\n[Showing last {} of line {end_line} (line is {last_line_size}). Full output: {full_output_path}]",
                    format_size(capture.truncation.output_bytes)
                ));
            } else if capture.truncation.truncated_by == Some(TruncatedBy::Lines) {
                output_text.push_str(&format!(
                    "\n\n[Showing lines {start_line}-{end_line} of {}. Full output: {full_output_path}]",
                    capture.truncation.total_lines
                ));
            } else {
                output_text.push_str(&format!(
                    "\n\n[Showing lines {start_line}-{end_line} of {} ({} limit). Full output: {full_output_path}]",
                    capture.truncation.total_lines,
                    format_size(DEFAULT_MAX_BYTES)
                ));
            }
        }

        let append_status = |status: &str| {
            if output_text.is_empty() {
                status.to_string()
            } else {
                format!("{output_text}\n\n{status}")
            }
        };

        if capture.cancelled {
            return Err(ToolError::failed(append_status("Command aborted")));
        }
        if let Some(error) = &capture.execution_error {
            if error.code == ExecutionErrorCode::Timeout {
                let seconds = input.timeout.unwrap_or_default();
                return Err(ToolError::failed(append_status(&format!(
                    "Command timed out after {seconds} seconds"
                ))));
            }
            return Err(ToolError::Execution(error.clone()));
        }
        if let Some(exit_code) = capture.exit_code {
            if exit_code != 0 {
                return Err(ToolError::failed(append_status(&format!(
                    "Command exited with code {exit_code}"
                ))));
            }
        }

        Ok(ToolResult::text(if output_text.is_empty() {
            "(no output)".to_string()
        } else {
            output_text
        })
        .with_details(details.map(|d| serde_json::to_value(d).unwrap_or(Value::Null))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ExecutionError;
    use crate::memory::MemoryExecutionEnv;
    use crate::types::{ExecutionEnvRef, ShellOutput};

    fn env_with_output(chunks: Vec<String>, exit_code: i32) -> Arc<MemoryExecutionEnv> {
        let env = Arc::new(MemoryExecutionEnv::new("/work"));
        env.set_shell_handler(Arc::new(move |_command, options| {
            let chunks = chunks.clone();
            Box::pin(async move {
                for chunk in &chunks {
                    if let Some(cb) = &options.on_stdout {
                        cb(chunk).ok();
                    }
                }
                Ok(ShellOutput {
                    stdout: chunks.join(""),
                    stderr: String::new(),
                    exit_code,
                })
            })
        }));
        env
    }

    #[tokio::test]
    async fn returns_captured_output() {
        let env = env_with_output(vec!["out".into(), "err".into()], 0);
        let context = ToolContext::new(env as ExecutionEnvRef);

        let result = BashTool::new()
            .execute(json!({ "command": "printf out" }), &context, None)
            .await
            .unwrap();

        assert_eq!(result.text_output(), "outerr");
    }

    #[tokio::test]
    async fn reports_no_output_for_silent_commands() {
        let env = env_with_output(vec![], 0);
        let context = ToolContext::new(env as ExecutionEnvRef);

        let result = BashTool::new()
            .execute(json!({ "command": "true" }), &context, None)
            .await
            .unwrap();

        assert_eq!(result.text_output(), "(no output)");
    }

    #[tokio::test]
    async fn reports_nonzero_exits_with_the_output() {
        let env = env_with_output(vec!["failed".into()], 7);
        let context = ToolContext::new(env as ExecutionEnvRef);

        let error = BashTool::new()
            .execute(json!({ "command": "exit 7" }), &context, None)
            .await
            .unwrap_err();

        assert_eq!(error.message(), "failed\n\nCommand exited with code 7");
    }

    #[tokio::test]
    async fn rejects_invalid_timeouts() {
        let env = env_with_output(vec![], 0);
        let context = ToolContext::new(env as ExecutionEnvRef);

        let error = BashTool::new()
            .execute(json!({ "command": "x", "timeout": 0 }), &context, None)
            .await
            .unwrap_err();
        assert_eq!(
            error.message(),
            "Invalid timeout: must be a finite number of seconds"
        );
    }

    /// Upstream: "preserves truncated output when a command times out".
    #[tokio::test]
    async fn preserves_truncated_output_when_a_command_times_out() {
        let lines: String = (1..=DEFAULT_MAX_LINES + 1)
            .map(|i| format!("line-{i}\n"))
            .collect();
        let env = Arc::new(MemoryExecutionEnv::new("/work"));
        env.set_shell_handler(Arc::new(move |_command, options| {
            let lines = lines.clone();
            Box::pin(async move {
                if let Some(cb) = &options.on_stdout {
                    cb(&lines).ok();
                }
                Err(ExecutionError::new(
                    ExecutionErrorCode::Timeout,
                    "timeout:0.05",
                ))
            })
        }));
        let context = ToolContext::new(env.clone() as ExecutionEnvRef);

        let error = BashTool::new()
            .execute(
                json!({ "command": "emit-output-then-time-out", "timeout": 0.05 }),
                &context,
                None,
            )
            .await
            .unwrap_err();

        let message = error.message();
        assert!(
            message.contains("Command timed out after 0.05 seconds"),
            "{message}"
        );
        let full_output_path = message
            .split("Full output: ")
            .nth(1)
            .and_then(|rest| rest.split(']').next())
            .expect("full output path in message");
        let full_output = env.read_text(full_output_path).await;
        assert!(full_output.contains("line-1\nline-2"));
        assert!(full_output.contains(&format!(
            "line-{DEFAULT_MAX_LINES}\nline-{}",
            DEFAULT_MAX_LINES + 1
        )));
    }

    /// Upstream: "coalesces updates and persists truncated full output".
    #[tokio::test]
    async fn coalesces_updates_and_persists_truncated_full_output() {
        let chunks: Vec<String> = (1..=3000).map(|i| format!("line-{i}\n")).collect();
        let env = env_with_output(chunks, 0);
        let updates: Arc<Mutex<Vec<ToolResult>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = updates.clone();
        let context = ToolContext::new(env.clone() as ExecutionEnvRef)
            .with_on_update(Arc::new(move |update| sink.lock().push(update)));

        let result = BashTool::new()
            .execute(json!({ "command": "emit 3000 lines" }), &context, None)
            .await
            .unwrap();

        let updates = updates.lock().clone();
        assert!(updates.len() < 25, "got {} updates", updates.len());

        let details: BashToolDetails =
            serde_json::from_value(result.details.clone().unwrap()).unwrap();
        let truncation = details.truncation.clone().unwrap();
        assert!(truncation.truncated);
        assert_eq!(truncation.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!(truncation.total_lines, 3000);
        assert_eq!(truncation.output_lines, 2000);
        assert!(result.text_output().contains("line-3000"));

        let full_output_path = details.full_output_path.clone().expect("full output path");
        let final_update = updates.last().expect("a final update");
        assert!(final_update.text_output().contains("line-3000"));
        let final_details: BashToolDetails =
            serde_json::from_value(final_update.details.clone().unwrap()).unwrap();
        assert_eq!(final_details.truncation.map(|t| t.total_lines), Some(3000));
        assert_eq!(
            final_details.full_output_path,
            Some(full_output_path.clone())
        );

        let full_output = env.read_text(&full_output_path).await;
        assert!(full_output.contains("line-1\nline-2"));
        assert!(full_output.contains("line-2999\nline-3000"));
    }

    #[tokio::test]
    async fn reports_the_total_size_of_an_oversized_final_line() {
        let env = env_with_output(vec!["0".repeat(60000)], 0);
        let context = ToolContext::new(env as ExecutionEnvRef);

        let result = BashTool::new()
            .execute(json!({ "command": "printf '%060000d' 0" }), &context, None)
            .await
            .unwrap();

        assert!(
            result
                .text_output()
                .contains("[Showing last 50.0KB of line 1 (line is 58.6KB). Full output:"),
            "{}",
            result.text_output()
        );
    }

    struct RecordingPrepare {
        seen: Mutex<Option<(String, String)>>,
    }

    #[async_trait]
    impl BashPrepare for RecordingPrepare {
        async fn prepare(
            &self,
            execution: &mut BashExecution,
            context: &ToolContext,
            _abort: Option<AbortSignal>,
        ) -> ToolResultOf<()> {
            *self.seen.lock() = Some((execution.command.clone(), context.tool_call_id.clone()));
            execution.cwd = "/workspace".into();
            execution.env.insert("EXPLICIT".into(), "explicit".into());
            execution.inherit_env = false;
            execution.command.push_str("\necho prepared");
            Ok(())
        }
    }

    #[tokio::test]
    async fn prepares_the_command_cwd_and_environment() {
        /// What the scripted shell saw: command, cwd, inherit_env, env.
        type Observed = (String, Option<String>, bool, BTreeMap<String, String>);

        let env = Arc::new(MemoryExecutionEnv::new("/work"));
        let observed: Arc<Mutex<Option<Observed>>> = Arc::new(Mutex::new(None));
        {
            let observed = observed.clone();
            env.set_shell_handler(Arc::new(move |command, options| {
                *observed.lock() = Some((
                    command,
                    options.cwd.clone(),
                    options.inherit_env,
                    options.env.clone(),
                ));
                Box::pin(async move {
                    Ok(ShellOutput {
                        stdout: String::new(),
                        stderr: String::new(),
                        exit_code: 0,
                    })
                })
            }));
        }
        let context = ToolContext::new(env as ExecutionEnvRef).with_tool_call_id("bash-prepare");
        let prepare = Arc::new(RecordingPrepare {
            seen: Mutex::new(None),
        });
        let tool = BashTool::with_options(BashToolOptions {
            command_prefix: Some("prefix=ready".into()),
            prepare: Some(prepare.clone()),
        });

        tool.execute(json!({ "command": ":" }), &context, None)
            .await
            .unwrap();

        let seen = prepare.seen.lock().clone().expect("prepare called");
        assert_eq!(seen.0, "prefix=ready\n:");
        assert_eq!(seen.1, "bash-prepare");

        let observed = observed.lock().clone().expect("exec called");
        assert_eq!(observed.0, "prefix=ready\n:\necho prepared");
        assert_eq!(observed.1.as_deref(), Some("/workspace"));
        assert!(!observed.2);
        assert_eq!(
            observed.3.get("EXPLICIT").map(String::as_str),
            Some("explicit")
        );
    }

    #[tokio::test]
    async fn supports_command_prefixes() {
        let env = Arc::new(MemoryExecutionEnv::new("/work"));
        let seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        {
            let seen = seen.clone();
            env.set_shell_handler(Arc::new(move |command, options| {
                *seen.lock() = Some(command);
                Box::pin(async move {
                    if let Some(cb) = &options.on_stdout {
                        cb("hello").ok();
                    }
                    Ok(ShellOutput {
                        stdout: "hello".into(),
                        stderr: String::new(),
                        exit_code: 0,
                    })
                })
            }));
        }
        let context = ToolContext::new(env as ExecutionEnvRef);

        let result = BashTool::with_options(BashToolOptions {
            command_prefix: Some("value=hello".into()),
            prepare: None,
        })
        .execute(json!({ "command": "printf $value" }), &context, None)
        .await
        .unwrap();

        assert_eq!(seen.lock().clone().unwrap(), "value=hello\nprintf $value");
        assert_eq!(result.text_output(), "hello");
    }

    #[tokio::test]
    async fn reports_cancellation() {
        let env = Arc::new(MemoryExecutionEnv::new("/work"));
        env.set_shell_handler(Arc::new(|_command, _options| {
            Box::pin(async { Err(ExecutionError::aborted()) })
        }));
        let context = ToolContext::new(env as ExecutionEnvRef);

        let error = BashTool::new()
            .execute(json!({ "command": "sleep 5" }), &context, None)
            .await
            .unwrap_err();
        assert_eq!(error.message(), "Command aborted");
    }
}
