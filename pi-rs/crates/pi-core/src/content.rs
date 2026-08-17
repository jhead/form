//! Content blocks. Port of the content interfaces in `packages/ai/src/types.ts`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Parsed form of `TextContent.textSignature` when it carries the v1 JSON payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextSignatureV1 {
    pub v: u8,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<TextPhase>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextPhase {
    Commentary,
    FinalAnswer,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextContent {
    pub text: String,
    /// e.g. for OpenAI responses: message metadata (legacy id string or [`TextSignatureV1`] JSON).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_signature: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingContent {
    pub thinking: String,
    /// e.g. for OpenAI responses: the reasoning item ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_signature: Option<String>,
    /// When true the thinking content was redacted by safety filters; the opaque
    /// encrypted payload lives in `thinking_signature` for multi-turn continuity.
    #[serde(default, skip_serializing_if = "is_false")]
    pub redacted: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageContent {
    /// Base64 encoded image data.
    pub data: String,
    /// e.g. `image/jpeg`, `image/png`.
    pub mime_type: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: Map<String, Value>,
    /// Google-specific: opaque signature for reusing thought context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
    /// OpenAI Responses namespace for calls to dynamically loaded or namespaced tools.
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

/// A block of assistant output. Serializes with an internal `type` tag exactly
/// like the TypeScript union (`{"type":"text","text":"..."}`).
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

    pub fn as_thinking(&self) -> Option<&ThinkingContent> {
        match self {
            Self::Thinking(t) => Some(t),
            _ => None,
        }
    }

    pub fn as_tool_call(&self) -> Option<&ToolCall> {
        match self {
            Self::ToolCall(t) => Some(t),
            _ => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::Thinking(_) => "thinking",
            Self::ToolCall(_) => "toolCall",
        }
    }
}

/// Content accepted as user input and produced by tool results: text or image.
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

    pub fn image(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self::Image(ImageContent {
            data: data.into(),
            mime_type: mime_type.into(),
        })
    }

    pub fn as_text(&self) -> Option<&TextContent> {
        match self {
            Self::Text(t) => Some(t),
            Self::Image(_) => None,
        }
    }
}

pub(crate) fn is_false(b: &bool) -> bool {
    !*b
}
