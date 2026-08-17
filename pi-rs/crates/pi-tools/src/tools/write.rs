//! The `write` tool.
//!
//! Port of `.upstream/packages/agent/src/harness/tools/write.ts`.

use async_trait::async_trait;
use pi_core::AbortSignal;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::ToolResultOf;
use crate::file_mutation_queue::with_file_mutation_queue;
use crate::path_utils::resolve_tool_path;
use crate::tool::{check_tool_abort, parse_args, AgentTool, ToolContext, ToolResult};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteToolInput {
    pub path: String,
    pub content: String,
}

#[derive(Clone, Default)]
pub struct WriteTool;

impl WriteTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl AgentTool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> String {
        "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. \
Automatically creates parent directories."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to write (relative or absolute)" },
                "content": { "type": "string", "description": "Content to write to the file" }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(
        &self,
        args: Value,
        context: &ToolContext,
        abort: Option<AbortSignal>,
    ) -> ToolResultOf<ToolResult> {
        let input: WriteToolInput = parse_args("write", args)?;
        let absolute_path =
            resolve_tool_path(context.env.as_ref(), &input.path, abort.clone()).await?;

        with_file_mutation_queue(&context.env, &absolute_path, || async {
            check_tool_abort(&abort)?;
            context
                .env
                .write_file(&absolute_path, input.content.as_bytes(), abort.clone())
                .await?;
            check_tool_abort(&abort)?;
            Ok(ToolResult::text(format!(
                "Successfully wrote {} bytes to {}",
                input.content.len(),
                input.path
            )))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::memory::MemoryExecutionEnv;
    use crate::types::{ExecutionEnvRef, FileSystem};

    #[tokio::test]
    async fn writes_files_and_creates_parent_directories() {
        let env = Arc::new(MemoryExecutionEnv::new("/work"));
        let context = ToolContext::new(env.clone() as ExecutionEnvRef);

        let result = WriteTool::new()
            .execute(
                json!({ "path": "nested/dir/file.txt", "content": "hello" }),
                &context,
                None,
            )
            .await
            .unwrap();

        assert_eq!(
            result.text_output(),
            "Successfully wrote 5 bytes to nested/dir/file.txt"
        );
        assert_eq!(env.read_text("nested/dir/file.txt").await, "hello");
    }

    /// Upstream: "keeps the mutation queue locked until an aborted write settles".
    #[tokio::test]
    async fn keeps_the_mutation_queue_locked_until_an_aborted_write_settles() {
        let env = Arc::new(MemoryExecutionEnv::new("/work"));
        let (first_started_tx, first_started_rx) = tokio::sync::oneshot::channel::<()>();
        let (finish_first_tx, finish_first_rx) = tokio::sync::oneshot::channel::<()>();
        let first_started_tx = Arc::new(parking_lot::Mutex::new(Some(first_started_tx)));
        let finish_first_rx = Arc::new(tokio::sync::Mutex::new(Some(finish_first_rx)));
        let second_write_started = Arc::new(std::sync::atomic::AtomicBool::new(false));

        {
            let second_write_started = second_write_started.clone();
            env.set_write_hook(Arc::new(move |_path, content| {
                let first_started_tx = first_started_tx.clone();
                let finish_first_rx = finish_first_rx.clone();
                let second_write_started = second_write_started.clone();
                Box::pin(async move {
                    if content == b"first\n" {
                        if let Some(tx) = first_started_tx.lock().take() {
                            let _ = tx.send(());
                        }
                        let rx = finish_first_rx.lock().await.take();
                        if let Some(rx) = rx {
                            let _ = rx.await;
                        }
                    } else if content == b"second\n" {
                        second_write_started.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                })
            }));
        }

        let env_ref: ExecutionEnvRef = env.clone();
        let context = ToolContext::new(env_ref.clone());
        let (handle, signal) = pi_core::AbortHandle::new();

        let first = {
            let context = context.clone();
            let signal = signal.clone();
            tokio::spawn(async move {
                WriteTool::new()
                    .execute(
                        json!({ "path": "file.txt", "content": "first\n" }),
                        &context,
                        Some(signal),
                    )
                    .await
            })
        };
        first_started_rx.await.unwrap();
        handle.abort();

        let second = {
            let context = context.clone();
            tokio::spawn(async move {
                WriteTool::new()
                    .execute(
                        json!({ "path": "file.txt", "content": "second\n" }),
                        &context,
                        None,
                    )
                    .await
            })
        };

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!second_write_started.load(std::sync::atomic::Ordering::SeqCst));
        finish_first_tx.send(()).unwrap();

        assert!(first.await.unwrap().is_err());
        second.await.unwrap().unwrap();
        assert_eq!(env.read_text("file.txt").await, "second\n");
    }

    #[tokio::test]
    async fn fails_fast_on_a_pre_aborted_call() {
        let env = Arc::new(MemoryExecutionEnv::new("/work"));
        let context = ToolContext::new(env.clone() as ExecutionEnvRef);
        let (handle, signal) = pi_core::AbortHandle::new();
        handle.abort();

        let error = WriteTool::new()
            .execute(
                json!({ "path": "file.txt", "content": "x" }),
                &context,
                Some(signal),
            )
            .await
            .unwrap_err();

        assert!(error.is_aborted());
        assert!(!env.exists("file.txt", None).await.unwrap());
    }
}
