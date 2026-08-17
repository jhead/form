//! Transcript wire types.
//!
//! **These are copied verbatim from `pi-core`** (`pi-rs/crates/pi-core/src/{content,message,event}.rs`)
//! and must stay structurally identical: same field names, same `camelCase` serde renaming,
//! same `snake_case` event tags. When `pi-rs` ships, this module is deleted and replaced by
//! `pub use pi_core::{...}`. Divergence is a bug, not a variation.
//!
//! See `docs/specs/00-protocol.md` §6.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Unix timestamp in milliseconds.
pub type TimestampMs = i64;

pub fn now_ms() -> TimestampMs {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

pub(crate) fn is_false(b: &bool) -> bool {
    !*b
}

// ---------------------------------------------------------------- usage

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub total: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_1h: Option<u64>,
    /// Reasoning tokens when reported. Subset of `output`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    pub total_tokens: u64,
    pub cost: Cost,
}

impl Usage {
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

// ---------------------------------------------------------------- content

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextContent {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_signature: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingContent {
    pub thinking: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_signature: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub redacted: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageContent {
    /// Base64-encoded image data.
    pub data: String,
    pub mime_type: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

impl ToolCall {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments: Map::new(),
            thought_signature: None,
            namespace: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AssistantContent {
    Text(TextContent),
    Thinking(ThinkingContent),
    ToolCall(ToolCall),
}

impl AssistantContent {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(TextContent {
            text: text.into(),
            text_signature: None,
        })
    }

    pub fn thinking(thinking: impl Into<String>) -> Self {
        Self::Thinking(ThinkingContent {
            thinking: thinking.into(),
            ..Default::default()
        })
    }

    pub fn as_text(&self) -> Option<&TextContent> {
        match self {
            Self::Text(t) => Some(t),
            _ => None,
        }
    }

    pub fn as_tool_call(&self) -> Option<&ToolCall> {
        match self {
            Self::ToolCall(t) => Some(t),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum InputContent {
    Text(TextContent),
    Image(ImageContent),
}

impl InputContent {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(TextContent {
            text: text.into(),
            text_signature: None,
        })
    }

    pub fn as_text(&self) -> Option<&TextContent> {
        match self {
            Self::Text(t) => Some(t),
            Self::Image(_) => None,
        }
    }
}

// ---------------------------------------------------------------- messages

/// `string | (TextContent | ImageContent)[]` from TypeScript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Blocks(Vec<InputContent>),
}

impl UserContent {
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
pub struct AssistantMessageDiagnostic {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<TimestampMs>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    #[serde(default)]
    pub content: Vec<AssistantContent>,
    /// API identifier, e.g. `anthropic-messages`.
    pub api: String,
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<AssistantMessageDiagnostic>>,
    pub usage: Usage,
    pub stop_reason: StopReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub timestamp: TimestampMs,
}

impl AssistantMessage {
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
            response_id: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Pending,
            error_message: None,
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

    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.content.iter().filter_map(|c| c.as_tool_call())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultMessage {
    pub tool_call_id: String,
    pub tool_name: String,
    #[serde(default)]
    pub content: Vec<InputContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_error: bool,
    pub timestamp: TimestampMs,
}

/// The transcript message union. Tagged on `role`, matching `pi`'s `Message`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum Message {
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
}

// ---------------------------------------------------------------- events

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DoneReason {
    Stop,
    Length,
    ToolUse,
    Deferred,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorReason {
    Aborted,
    Error,
}

/// Streaming protocol event. Tags are `snake_case`, and the tool-call variants are spelled
/// `toolcall_*` — inherited from `pi`'s TypeScript union. **Do not normalize these.**
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantMessageEvent {
    Start {
        partial: AssistantMessage,
    },
    TextStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        partial: AssistantMessage,
    },
    TextDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
        partial: AssistantMessage,
    },
    TextEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        content: String,
        partial: AssistantMessage,
    },
    ThinkingStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        partial: AssistantMessage,
    },
    ThinkingDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
        partial: AssistantMessage,
    },
    ThinkingEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        content: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_start")]
    ToolCallStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_delta")]
    ToolCallDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_end")]
    ToolCallEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        #[serde(rename = "toolCall")]
        tool_call: ToolCall,
        partial: AssistantMessage,
    },
    Done {
        reason: DoneReason,
        message: AssistantMessage,
    },
    Error {
        reason: ErrorReason,
        error: AssistantMessage,
    },
}

impl AssistantMessageEvent {
    pub fn terminal_message(&self) -> Option<&AssistantMessage> {
        match self {
            Self::Done { message, .. } => Some(message),
            Self::Error { error, .. } => Some(error),
            _ => None,
        }
    }

    pub fn is_terminal(&self) -> bool {
        self.terminal_message().is_some()
    }

    pub fn partial(&self) -> Option<&AssistantMessage> {
        match self {
            Self::Start { partial }
            | Self::TextStart { partial, .. }
            | Self::TextDelta { partial, .. }
            | Self::TextEnd { partial, .. }
            | Self::ThinkingStart { partial, .. }
            | Self::ThinkingDelta { partial, .. }
            | Self::ThinkingEnd { partial, .. }
            | Self::ToolCallStart { partial, .. }
            | Self::ToolCallDelta { partial, .. }
            | Self::ToolCallEnd { partial, .. } => Some(partial),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------- entries

/// Append-only transcript log entry. Mirrors `pi`'s harness `Entry` union.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
// `Message` dominates the size, but boxing it would complicate every match for no gain —
// entries live in a `Vec` and are never passed around by value in bulk.
#[allow(clippy::large_enum_variant)]
pub enum EntryKind {
    Message {
        message: Message,
    },
    ModelChange {
        provider: String,
        model_id: String,
    },
    ThinkingLevelChange {
        thinking_level: String,
    },
    Compaction {
        summary: String,
        tokens_before: u64,
    },
    BranchSummary {
        from_id: String,
        summary: String,
    },
    Custom {
        custom_type: String,
        data: Option<Value>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    pub session_id: String,
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub timestamp: TimestampMs,
    #[serde(flatten)]
    pub kind: EntryKind,
}
