//! The executable tool trait.
//!
//! Port of `AgentTool` / `AgentToolResult` / `AgentToolUpdateCallback` from
//! `.upstream/packages/agent/src/types.ts` and `AgentHarnessTool` from
//! `harness/types.ts`, collapsed into one object-safe trait.
//!
//! Upstream is generic over a TypeBox schema (`TSchema`) and a details type.
//! Neither survives the FFI boundary, so parameters are JSON Schema carried as
//! [`serde_json::Value`], arguments arrive as `Value`, and `details` is `Value`.
//! Validation against the schema is the caller's job (the agent loop), exactly
//! as upstream validates before calling `execute`.

use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{AbortSignal, InputContent, ToolResultMessage, Usage};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ToolError, ToolResultOf};
use crate::types::ExecutionEnvRef;

/// Per-tool override for the agent loop's tool batching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolExecutionMode {
    /// Must run alone, never concurrently with other tool calls.
    Sequential,
    /// May run concurrently with other tool calls.
    Parallel,
}

/// Final or partial result produced by a tool.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    /// Text or image content returned to the model.
    #[serde(default)]
    pub content: Vec<InputContent>,
    /// Arbitrary structured details for logs or UI rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// Usage from the tool execution itself. Not part of LLM context accounting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Tools introduced by this result and available from here onward.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_tool_names: Option<Vec<String>>,
    /// Hint that the agent should stop after the current tool batch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

impl ToolResult {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![InputContent::text(text)],
            ..Default::default()
        }
    }

    pub fn with_details(mut self, details: Option<Value>) -> Self {
        self.details = details;
        self
    }

    /// Concatenate the text blocks, ignoring images. Convenience for tests and UIs.
    pub fn text_output(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.as_str()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Build the transcript message for a successful call.
    pub fn into_message(
        self,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
    ) -> ToolResultMessage {
        ToolResultMessage {
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            content: self.content,
            details: self.details,
            usage: self.usage,
            added_tool_names: self.added_tool_names,
            is_error: false,
            timestamp: pi_core::now_ms(),
        }
    }

    /// Build the transcript message for a failed call. Upstream turns the thrown
    /// error's message into the error tool result text.
    pub fn error_message(
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        error: &ToolError,
    ) -> ToolResultMessage {
        ToolResultMessage::text(tool_call_id, tool_name, error.message(), true)
    }
}

/// Callback used by tools to stream partial execution updates.
///
/// Scoped to one `execute` invocation; the runtime ignores calls made after the
/// future settles.
pub type ToolUpdateCallback = Arc<dyn Fn(ToolResult) + Send + Sync>;

/// Everything a built-in tool needs for one call.
///
/// Upstream splits this into the tool-call id, the update callback and an
/// application-defined `TContext` (which for the built-in tools is
/// `ExecutionToolContext { env }`). The port carries the concrete fields, since
/// a generic context parameter cannot cross FFI.
#[derive(Clone)]
pub struct ToolContext {
    pub env: ExecutionEnvRef,
    pub tool_call_id: String,
    pub on_update: Option<ToolUpdateCallback>,
}

impl ToolContext {
    pub fn new(env: ExecutionEnvRef) -> Self {
        Self {
            env,
            tool_call_id: String::new(),
            on_update: None,
        }
    }

    pub fn with_tool_call_id(mut self, id: impl Into<String>) -> Self {
        self.tool_call_id = id.into();
        self
    }

    pub fn with_on_update(mut self, on_update: ToolUpdateCallback) -> Self {
        self.on_update = Some(on_update);
        self
    }

    pub fn emit_update(&self, result: ToolResult) {
        if let Some(cb) = &self.on_update {
            cb(result);
        }
    }
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("cwd", &self.env.cwd())
            .field("tool_call_id", &self.tool_call_id)
            .field("has_on_update", &self.on_update.is_some())
            .finish()
    }
}

/// A tool the agent can call.
#[async_trait]
pub trait AgentTool: Send + Sync + 'static {
    /// Stable name the model calls.
    fn name(&self) -> &str;

    /// Human-readable label for UI display. Defaults to [`AgentTool::name`].
    fn label(&self) -> &str {
        self.name()
    }

    fn description(&self) -> String;

    /// JSON Schema for the arguments object.
    fn parameters(&self) -> Value;

    fn execution_mode(&self) -> Option<ToolExecutionMode> {
        None
    }

    /// Compatibility shim applied to raw tool-call arguments before schema
    /// validation. Port of upstream's `prepareArguments`.
    fn prepare_arguments(&self, args: Value) -> Value {
        args
    }

    /// Run the call. Errors are returned, never panicked, and become an error
    /// tool result in the transcript.
    async fn execute(
        &self,
        args: Value,
        context: &ToolContext,
        abort: Option<AbortSignal>,
    ) -> ToolResultOf<ToolResult>;

    /// The provider-facing declaration for [`pi_core::Context::tools`].
    fn declaration(&self) -> pi_core::Tool {
        pi_core::Tool::new(self.name(), self.description(), self.parameters())
    }
}

/// Shared handle to a tool. Tool sets are `Vec<AgentToolRef>`.
pub type AgentToolRef = Arc<dyn AgentTool>;

/// Fail fast when the call has already been cancelled.
pub(crate) fn check_tool_abort(abort: &Option<AbortSignal>) -> ToolResultOf<()> {
    match abort {
        Some(signal) if signal.is_aborted() => Err(ToolError::Aborted),
        _ => Ok(()),
    }
}

/// Deserialize tool arguments, reporting a schema-shaped failure instead of panicking.
pub(crate) fn parse_args<T: serde::de::DeserializeOwned>(
    tool: &str,
    args: Value,
) -> ToolResultOf<T> {
    serde_json::from_value(args)
        .map_err(|e| ToolError::invalid_arguments(format!("Invalid {tool} tool arguments: {e}")))
}
