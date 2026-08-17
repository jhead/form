//! Shell output capture, truncation and spill-to-file.
//!
//! Port of `.upstream/packages/agent/src/harness/utils/shell-output.ts`.
//!
//! The capture keeps only the tail of the output in memory (2x the byte limit)
//! and, once either limit is exceeded, streams everything to a temp file so the
//! tool can point the model at the full log.
//!
//! Upstream can `await` inside its `onStdout` handler because it chains
//! promises. The Rust output callbacks are synchronous, so chunks are forwarded
//! over a channel to a consumer task that owns the capture state and performs
//! the appends. Chunks that arrive after the command settles are dropped when
//! the channel closes, which is the same behaviour as upstream's
//! `acceptingOutput` flag.

use std::collections::BTreeMap;
use std::sync::Arc;

use pi_core::AbortSignal;

use crate::error::{ExecResult, ExecutionError, ExecutionErrorCode};
use crate::truncate::{
    truncate_tail, TruncatedBy, TruncationOptions, TruncationResult, DEFAULT_MAX_BYTES,
    DEFAULT_MAX_LINES,
};
use crate::types::{ExecutionEnvRef, ShellExecOptions};

/// Snapshot of the capture while the command is still running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCaptureProgress {
    pub output: String,
    pub truncation: TruncationResult,
    pub full_output_path: Option<String>,
    /// Bytes in the currently open (unterminated) last line.
    pub last_line_bytes: usize,
}

/// Progress callback: the sanitized chunk plus the capture state after it.
pub type ChunkCallback = Arc<dyn Fn(&str, &ShellCaptureProgress) + Send + Sync>;

#[derive(Clone, Default)]
pub struct ShellCaptureOptions {
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
    pub inherit_env: bool,
    pub timeout_secs: Option<f64>,
    pub abort: Option<AbortSignal>,
    pub on_chunk: Option<ChunkCallback>,
    /// Return shell failures alongside the captured output instead of as `Err`.
    pub return_execution_errors: bool,
}

impl ShellCaptureOptions {
    pub fn new() -> Self {
        Self {
            inherit_env: true,
            ..Default::default()
        }
    }
}

impl std::fmt::Debug for ShellCaptureOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellCaptureOptions")
            .field("cwd", &self.cwd)
            .field("env", &self.env)
            .field("inherit_env", &self.inherit_env)
            .field("timeout_secs", &self.timeout_secs)
            .field("return_execution_errors", &self.return_execution_errors)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCaptureResult {
    pub output: String,
    pub truncation: TruncationResult,
    pub full_output_path: Option<String>,
    pub last_line_bytes: usize,
    /// `None` when the command was cancelled or failed to run.
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub truncated: bool,
    pub execution_error: Option<ExecutionError>,
}

/// Drop control characters that would corrupt a transcript, keeping tab, LF and CR.
pub fn sanitize_binary_output(text: &str) -> String {
    text.chars()
        .filter(|c| {
            let code = *c as u32;
            if code == 0x09 || code == 0x0a || code == 0x0d {
                return true;
            }
            if code <= 0x1f {
                return false;
            }
            // Interlinear annotation controls.
            !(0xfff9..=0xfffb).contains(&code)
        })
        .collect()
}

/// Keep the last `max_bytes` bytes, snapped up to a char boundary.
fn trim_to_last_utf8_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut start = text.len() - max_bytes;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}

#[derive(Default)]
struct CaptureState {
    tail_output: String,
    total_bytes: usize,
    completed_lines: usize,
    has_open_line: bool,
    current_line_bytes: usize,
    full_output_path: Option<String>,
    full_output_requested: bool,
}

impl CaptureState {
    fn progress(&self) -> ShellCaptureProgress {
        let tail_truncation = truncate_tail(&self.tail_output, TruncationOptions::default());
        let total_lines = self.completed_lines + usize::from(self.has_open_line);
        let truncated = total_lines > DEFAULT_MAX_LINES || self.total_bytes > DEFAULT_MAX_BYTES;
        let truncated_by = if truncated {
            tail_truncation
                .truncated_by
                .or(Some(if self.total_bytes > DEFAULT_MAX_BYTES {
                    TruncatedBy::Bytes
                } else {
                    TruncatedBy::Lines
                }))
        } else {
            None
        };
        let truncation = TruncationResult {
            truncated,
            truncated_by,
            total_lines,
            total_bytes: self.total_bytes,
            ..tail_truncation
        };
        ShellCaptureProgress {
            output: if truncated {
                truncation.content.clone()
            } else {
                self.tail_output.clone()
            },
            truncation,
            full_output_path: self.full_output_path.clone(),
            last_line_bytes: self.current_line_bytes,
        }
    }
}

async fn ensure_full_output_file(
    env: &ExecutionEnvRef,
    state: &mut CaptureState,
    initial_content: &str,
) -> ExecResult<()> {
    if state.full_output_requested {
        return Ok(());
    }
    state.full_output_requested = true;
    let path = env
        .create_temp_file(Some("bash-"), Some(".log"), None)
        .await
        .map_err(|e| ExecutionError::new(ExecutionErrorCode::Unknown, e.message))?;
    env.append_file(&path, initial_content.as_bytes(), None)
        .await
        .map_err(|e| ExecutionError::new(ExecutionErrorCode::Unknown, e.message))?;
    state.full_output_path = Some(path);
    Ok(())
}

/// Run a command, capturing its combined output with truncation and spill-to-file.
pub async fn execute_shell_with_capture(
    env: &ExecutionEnvRef,
    command: &str,
    options: ShellCaptureOptions,
) -> ExecResult<ShellCaptureResult> {
    let max_output_bytes = DEFAULT_MAX_BYTES * 2;
    // `None` is the end-of-stream sentinel: a shell implementation may hold a
    // clone of the output callback (and therefore of the sender) past `exec`.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Option<String>>();

    let consumer = {
        let env = env.clone();
        let on_chunk = options.on_chunk.clone();
        tokio::spawn(async move {
            let mut state = CaptureState::default();
            let mut capture_error: Option<ExecutionError> = None;
            while let Some(Some(chunk)) = rx.recv().await {
                if capture_error.is_some() {
                    continue;
                }
                let text = sanitize_binary_output(&chunk).replace('\r', "");
                let text_bytes = text.len();
                state.total_bytes += text_bytes;
                state.completed_lines += text.matches('\n').count();
                if let Some(last_newline) = text.rfind('\n') {
                    let trailing = &text[last_newline + 1..];
                    state.current_line_bytes = trailing.len();
                    state.has_open_line = !trailing.is_empty();
                } else if !text.is_empty() {
                    state.current_line_bytes += text_bytes;
                    state.has_open_line = true;
                }

                state.tail_output.push_str(&text);
                let total_lines = state.completed_lines + usize::from(state.has_open_line);
                let over_limit =
                    state.total_bytes > DEFAULT_MAX_BYTES || total_lines > DEFAULT_MAX_LINES;
                let write = if over_limit && !state.full_output_requested {
                    let initial = state.tail_output.clone();
                    ensure_full_output_file(&env, &mut state, &initial).await
                } else if state.full_output_requested {
                    let path = state.full_output_path.clone().unwrap_or_default();
                    env.append_file(&path, text.as_bytes(), None)
                        .await
                        .map_err(|e| ExecutionError::new(ExecutionErrorCode::Unknown, e.message))
                } else {
                    Ok(())
                };
                if let Err(error) = write {
                    capture_error = Some(error);
                    continue;
                }

                state.tail_output = trim_to_last_utf8_bytes(&state.tail_output, max_output_bytes);
                if let Some(on_chunk) = &on_chunk {
                    on_chunk(&text, &state.progress());
                }
            }
            (state, capture_error)
        })
    };

    // Upstream's `acceptingOutput` flag: a shell implementation can keep a
    // reference to the callback and fire it after `exec` resolves.
    let accepting = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let sink = {
        let tx = tx.clone();
        let accepting = accepting.clone();
        Arc::new(move |chunk: &str| {
            if accepting.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = tx.send(Some(chunk.to_string()));
            }
            Ok(())
        })
    };
    let exec_options = ShellExecOptions {
        cwd: options.cwd.clone(),
        env: options.env.clone(),
        inherit_env: options.inherit_env,
        timeout_secs: options.timeout_secs,
        abort: options.abort.clone(),
        on_stdout: Some(sink.clone()),
        on_stderr: Some(sink),
    };

    let result = env.exec(command, exec_options).await;
    accepting.store(false, std::sync::atomic::Ordering::SeqCst);
    let _ = tx.send(None);
    drop(tx);
    let (mut state, capture_error) = consumer.await.map_err(|e| {
        ExecutionError::new(
            ExecutionErrorCode::Unknown,
            format!("output capture task failed: {e}"),
        )
    })?;

    let mut progress = state.progress();
    if progress.truncation.truncated && !state.full_output_requested {
        let initial = state.tail_output.clone();
        ensure_full_output_file(env, &mut state, &initial).await?;
        progress = state.progress();
    }
    if let Some(error) = capture_error {
        return Err(error);
    }

    let aborted = options.abort.as_ref().is_some_and(|s| s.is_aborted());
    match result {
        Err(error) => {
            if error.code == ExecutionErrorCode::Aborted || aborted {
                return Ok(ShellCaptureResult {
                    truncated: progress.truncation.truncated,
                    output: progress.output,
                    truncation: progress.truncation,
                    full_output_path: progress.full_output_path,
                    last_line_bytes: progress.last_line_bytes,
                    exit_code: None,
                    cancelled: true,
                    execution_error: None,
                });
            }
            if options.return_execution_errors {
                return Ok(ShellCaptureResult {
                    truncated: progress.truncation.truncated,
                    output: progress.output,
                    truncation: progress.truncation,
                    full_output_path: progress.full_output_path,
                    last_line_bytes: progress.last_line_bytes,
                    exit_code: None,
                    cancelled: false,
                    execution_error: Some(error),
                });
            }
            Err(error)
        }
        Ok(output) => Ok(ShellCaptureResult {
            truncated: progress.truncation.truncated,
            output: progress.output,
            truncation: progress.truncation,
            full_output_path: progress.full_output_path,
            last_line_bytes: progress.last_line_bytes,
            exit_code: if aborted {
                None
            } else {
                Some(output.exit_code)
            },
            cancelled: aborted,
            execution_error: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::memory::MemoryExecutionEnv;
    use crate::types::ShellOutput;

    fn env_emitting(chunks: Vec<String>) -> ExecutionEnvRef {
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
                    exit_code: 0,
                })
            })
        }));
        env
    }

    #[test]
    fn sanitizes_control_characters() {
        assert_eq!(sanitize_binary_output("a\u{0}b\u{7}c"), "abc");
        assert_eq!(sanitize_binary_output("a\tb\nc\rd"), "a\tb\nc\rd");
        assert_eq!(sanitize_binary_output("a\u{fffa}b"), "ab");
    }

    #[tokio::test]
    async fn captures_small_output_without_spilling() {
        let env = env_emitting(vec!["hello\n".into(), "world\n".into()]);
        let result = execute_shell_with_capture(&env, "x", ShellCaptureOptions::new())
            .await
            .unwrap();

        assert_eq!(result.output, "hello\nworld\n");
        assert_eq!(result.exit_code, Some(0));
        assert!(!result.truncated);
        assert!(result.full_output_path.is_none());
    }

    #[tokio::test]
    async fn strips_carriage_returns_and_control_bytes() {
        let env = env_emitting(vec!["a\r\nb\u{0}\n".into()]);
        let result = execute_shell_with_capture(&env, "x", ShellCaptureOptions::new())
            .await
            .unwrap();
        assert_eq!(result.output, "a\nb\n");
    }

    #[tokio::test]
    async fn spills_large_output_to_a_full_output_file() {
        let lines: String = (1..=DEFAULT_MAX_LINES + 500)
            .map(|i| format!("line-{i}\n"))
            .collect();
        let env = env_emitting(vec![lines]);
        let result = execute_shell_with_capture(&env, "x", ShellCaptureOptions::new())
            .await
            .unwrap();

        assert!(result.truncated);
        assert_eq!(result.truncation.total_lines, DEFAULT_MAX_LINES + 500);
        assert_eq!(result.truncation.output_lines, DEFAULT_MAX_LINES);
        let path = result.full_output_path.expect("full output path");
        let full = env.read_text_file(&path, None).await.unwrap();
        assert!(full.starts_with("line-1\nline-2\n"));
        assert!(full.contains(&format!("line-{}\n", DEFAULT_MAX_LINES + 500)));
        // The in-memory tail keeps only the end of the output.
        assert!(result
            .output
            .contains(&format!("line-{}", DEFAULT_MAX_LINES + 500)));
        assert!(!result.output.contains("line-1\n"));
    }

    #[tokio::test]
    async fn reports_chunk_progress() {
        let seen = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let env = env_emitting(vec!["one\n".into(), "two\n".into()]);
        let mut options = ShellCaptureOptions::new();
        let sink = seen.clone();
        options.on_chunk = Some(Arc::new(move |chunk, progress| {
            sink.lock()
                .push(format!("{chunk}|{}", progress.truncation.total_lines));
        }));

        execute_shell_with_capture(&env, "x", options)
            .await
            .unwrap();
        assert_eq!(
            *seen.lock(),
            vec!["one\n|1".to_string(), "two\n|2".to_string()]
        );
    }

    #[tokio::test]
    async fn returns_execution_errors_with_the_captured_output() {
        let env = Arc::new(MemoryExecutionEnv::new("/work"));
        env.set_shell_handler(Arc::new(|_command, options| {
            Box::pin(async move {
                if let Some(cb) = &options.on_stdout {
                    cb("partial\n").ok();
                }
                Err(ExecutionError::new(
                    ExecutionErrorCode::Timeout,
                    "timeout:0.05",
                ))
            })
        }));
        let env: ExecutionEnvRef = env;

        let mut options = ShellCaptureOptions::new();
        options.return_execution_errors = true;
        let result = execute_shell_with_capture(&env, "x", options)
            .await
            .unwrap();

        assert_eq!(result.output, "partial\n");
        assert_eq!(
            result.execution_error.map(|e| e.code),
            Some(ExecutionErrorCode::Timeout)
        );
        assert_eq!(result.exit_code, None);
    }

    #[tokio::test]
    async fn surfaces_abort_as_cancelled() {
        let env = Arc::new(MemoryExecutionEnv::new("/work"));
        env.set_shell_handler(Arc::new(|_command, _options| {
            Box::pin(async move { Err(ExecutionError::aborted()) })
        }));
        let env: ExecutionEnvRef = env;

        let result = execute_shell_with_capture(&env, "x", ShellCaptureOptions::new())
            .await
            .unwrap();
        assert!(result.cancelled);
        assert_eq!(result.exit_code, None);
    }

    #[tokio::test]
    async fn ignores_chunks_delivered_after_the_command_settles() {
        let env = Arc::new(MemoryExecutionEnv::new("/work"));
        env.set_shell_handler(Arc::new(|_command, options| {
            Box::pin(async move {
                if let Some(cb) = &options.on_stdout {
                    cb("before\n").ok();
                    let cb = cb.clone();
                    // Fires after `exec` has returned and the channel is closed.
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                        cb("late\n").ok();
                    });
                }
                Ok(ShellOutput {
                    stdout: "before\n".into(),
                    stderr: String::new(),
                    exit_code: 0,
                })
            })
        }));
        let env: ExecutionEnvRef = env;

        let result = execute_shell_with_capture(&env, "x", ShellCaptureOptions::new())
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(result.output, "before\n");
    }
}
