//! Messages and usage accounting. Port of `packages/ai/src/types.ts`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::content::{AssistantContent, InputContent};

/// Unix timestamp in milliseconds, matching the TypeScript `timestamp` fields.
pub type TimestampMs = i64;

pub fn now_ms() -> TimestampMs {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub total: f64,
}

/// Token counters are signed. Providers issue *corrective* usage records
/// (upstream's `UsageCause::Adjustment`) carrying negative deltas such as
/// `input: -2`, and the session ledger sums them; unsigned counters would make
/// those records unrepresentable and files written by upstream unloadable.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    /// Subset of `cache_write` written with 1h retention. Only Anthropic reports this split.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_1h: Option<i64>,
    /// Reasoning tokens when reported. Subset of `output`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<i64>,
    pub total_tokens: i64,
    pub cost: Cost,
}

impl Usage {
    /// Total context tokens this record represents.
    ///
    /// Upstream is `usage.totalTokens || <sum of components>`, so only a
    /// *zero* total falls through to the components — a corrective record's
    /// negative total is a real value and must not be recomputed away.
    pub fn context_tokens(&self) -> i64 {
        if self.total_tokens != 0 {
            self.total_tokens
        } else {
            self.input + self.output + self.cache_read + self.cache_write
        }
    }

    /// Sum two usage records (costs included). Used by the agent loop and compaction.
    pub fn add(&self, other: &Usage) -> Usage {
        Usage {
            input: self.input + other.input,
            output: self.output + other.output,
            cache_read: self.cache_read + other.cache_read,
            cache_write: self.cache_write + other.cache_write,
            cache_write_1h: match (self.cache_write_1h, other.cache_write_1h) {
                (None, None) => None,
                (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
            },
            reasoning: match (self.reasoning, other.reasoning) {
                (None, None) => None,
                (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
            },
            total_tokens: self.total_tokens + other.total_tokens,
            cost: Cost {
                input: self.cost.input + other.cost.input,
                output: self.cost.output + other.cost.output,
                cache_read: self.cost.cache_read + other.cost.cache_read,
                cache_write: self.cost.cache_write + other.cost.cache_write,
                total: self.cost.total + other.cost.total,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StopReason {
    Pending,
    Stop,
    Length,
    ToolUse,
    Error,
    Aborted,
    Deferred,
}

impl StopReason {
    pub fn is_terminal(self) -> bool {
        !matches!(self, StopReason::Pending)
    }
}

/// Durable handle for provider deferred/async responses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeferredHandle {
    pub provider: String,
    pub model_id: String,
    pub api: String,
    /// Provider token, such as a response id or batch id plus row id.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_after_ms: Option<u64>,
    /// Provider conversion data required to reconstruct the final assistant message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Redacted provider/runtime diagnostic attached to an assistant message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessageDiagnostic {
    /// Stable machine-readable code, e.g. `provider_http_error`, `stream_retry`.
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<DiagnosticSeverity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<TimestampMs>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

/// User message content: a bare string or a list of text/image blocks.
/// Mirrors `string | (TextContent | ImageContent)[]` from TypeScript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Blocks(Vec<InputContent>),
}

impl UserContent {
    /// Flatten to plain text, concatenating text blocks and ignoring images.
    pub fn to_text(&self) -> String {
        match self {
            UserContent::Text(s) => s.clone(),
            UserContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| b.as_text().map(|t| t.text.as_str()))
                .collect::<Vec<_>>()
                .join(""),
        }
    }

    pub fn blocks(&self) -> Vec<InputContent> {
        match self {
            UserContent::Text(s) => vec![InputContent::text(s.clone())],
            UserContent::Blocks(b) => b.clone(),
        }
    }
}

impl From<&str> for UserContent {
    fn from(s: &str) -> Self {
        UserContent::Text(s.to_string())
    }
}

impl From<String> for UserContent {
    fn from(s: String) -> Self {
        UserContent::Text(s)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMessage {
    pub content: UserContent,
    pub timestamp: TimestampMs,
}

impl UserMessage {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: UserContent::Text(content.into()),
            timestamp: now_ms(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    #[serde(default)]
    pub content: Vec<AssistantContent>,
    /// API identifier, e.g. `anthropic-messages`.
    pub api: String,
    /// Provider identifier, e.g. `anthropic`.
    pub provider: String,
    pub model: String,
    /// Concrete `chunk.model` when different from the requested model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<AssistantMessageDiagnostic>>,
    pub usage: Usage,
    pub stop_reason: StopReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred: Option<DeferredHandle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_stop_reason: Option<String>,
    /// Provider indication that the model explicitly ended its turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_turn: Option<bool>,
    pub timestamp: TimestampMs,
}

impl AssistantMessage {
    /// Empty in-progress message used as the `partial` of a `start` event.
    pub fn pending(
        api: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            content: Vec::new(),
            api: api.into(),
            provider: provider.into(),
            model: model.into(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Pending,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: now_ms(),
        }
    }

    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.as_str()))
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn tool_calls(&self) -> impl Iterator<Item = &crate::content::ToolCall> {
        self.content.iter().filter_map(|c| c.as_tool_call())
    }

    pub fn push_diagnostic(&mut self, diagnostic: AssistantMessageDiagnostic) {
        self.diagnostics
            .get_or_insert_with(Vec::new)
            .push(diagnostic);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultMessage {
    pub tool_call_id: String,
    pub tool_name: String,
    #[serde(default)]
    pub content: Vec<InputContent>,
    /// Tool-specific structured payload (`TDetails` in TypeScript).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// Usage from the tool execution itself. Not part of main LLM context accounting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Names from `Context.tools` that became available after this result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_tool_names: Option<Vec<String>>,
    /// Always serialized. Upstream declares `isError: boolean` as required, so
    /// omitting it when false produces a message the TypeScript side rejects.
    #[serde(default)]
    pub is_error: bool,
    pub timestamp: TimestampMs,
}

impl ToolResultMessage {
    pub fn text(
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        text: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            content: vec![InputContent::text(text)],
            details: None,
            usage: None,
            added_tool_names: None,
            is_error,
            timestamp: now_ms(),
        }
    }
}

/// The message union. Serializes with the TypeScript `role` discriminator.
///
/// `AssistantMessage` makes this enum large (~550 bytes), and boxing it would
/// shrink it. It stays unboxed deliberately: `Message` is the type that crosses
/// the FFI boundary and appears in every public signature, and a boxed variant
/// costs every consumer an extra indirection in pattern matches for a saving
/// that does not matter at conversation-sized `Vec<Message>` lengths.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum Message {
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
}

impl Message {
    pub fn role(&self) -> &'static str {
        match self {
            Message::User(_) => "user",
            Message::Assistant(_) => "assistant",
            Message::ToolResult(_) => "toolResult",
        }
    }

    pub fn timestamp(&self) -> TimestampMs {
        match self {
            Message::User(m) => m.timestamp,
            Message::Assistant(m) => m.timestamp,
            Message::ToolResult(m) => m.timestamp,
        }
    }

    pub fn as_assistant(&self) -> Option<&AssistantMessage> {
        match self {
            Message::Assistant(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_user(&self) -> Option<&UserMessage> {
        match self {
            Message::User(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_tool_result(&self) -> Option<&ToolResultMessage> {
        match self {
            Message::ToolResult(m) => Some(m),
            _ => None,
        }
    }
}

impl From<UserMessage> for Message {
    fn from(m: UserMessage) -> Self {
        Message::User(m)
    }
}

impl From<AssistantMessage> for Message {
    fn from(m: AssistantMessage) -> Self {
        Message::Assistant(m)
    }
}

impl From<ToolResultMessage> for Message {
    fn from(m: ToolResultMessage) -> Self {
        Message::ToolResult(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_round_trips_typescript_shape() {
        let msg = Message::User(UserMessage {
            content: UserContent::Text("hi".into()),
            timestamp: 1700000000000,
        });
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"], "hi");
        let back: Message = serde_json::from_value(json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn assistant_content_is_type_tagged() {
        let json = serde_json::json!({"type": "toolCall", "id": "1", "name": "bash", "arguments": {"cmd": "ls"}});
        let c: AssistantContent = serde_json::from_value(json).unwrap();
        assert_eq!(c.kind(), "toolCall");
    }
}
