//! The Gemini `generateContent` wire types.
//!
//! Upstream calls `@google/genai`; this port speaks the same HTTP directly, so
//! the shapes here mirror what that SDK's `generateContentParametersTo{Mldev,
//! Vertex}` emit and what `generateContentResponseFrom*` reads. Field placement
//! matters: `systemInstruction`, `tools`, `toolConfig`, `safetySettings` and
//! `cachedContent` sit at the request root while everything else the SDK calls
//! "config" is nested under `generationConfig`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Blob {
    pub mime_type: String,
    pub data: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
    /// Gemini 3+ multimodal function responses: images nested in the response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<Part>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `true` marks a thought summary. The definitive thinking marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought: Option<bool>,
    /// Opaque encrypted reasoning context. Can ride on *any* part type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<Blob>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_call: Option<FunctionCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_response: Option<FunctionResponse>,
}

impl Part {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            ..Default::default()
        }
    }

    pub fn inline_data(mime_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            inline_data: Some(Blob {
                mime_type: mime_type.into(),
                data: data.into(),
            }),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Content {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<Part>>,
}

impl Content {
    pub fn new(role: &str, parts: Vec<Part>) -> Self {
        Self {
            role: Some(role.to_string()),
            parts: Some(parts),
        }
    }

    pub fn parts_mut(&mut self) -> &mut Vec<Part> {
        self.parts.get_or_insert_with(Vec::new)
    }

    pub fn parts(&self) -> &[Part] {
        self.parts.as_deref().unwrap_or(&[])
    }
}

/// `ThinkingLevel` from `@google/genai`. Vertex and Gemini share the strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoogleThinkingLevel {
    #[serde(rename = "THINKING_LEVEL_UNSPECIFIED")]
    Unspecified,
    #[serde(rename = "MINIMAL")]
    Minimal,
    #[serde(rename = "LOW")]
    Low,
    #[serde(rename = "MEDIUM")]
    Medium,
    #[serde(rename = "HIGH")]
    High,
}

impl GoogleThinkingLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            GoogleThinkingLevel::Unspecified => "THINKING_LEVEL_UNSPECIFIED",
            GoogleThinkingLevel::Minimal => "MINIMAL",
            GoogleThinkingLevel::Low => "LOW",
            GoogleThinkingLevel::Medium => "MEDIUM",
            GoogleThinkingLevel::High => "HIGH",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_thoughts: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<GoogleThinkingLevel>,
    /// `-1` asks for a dynamic budget, `0` disables thinking on Gemini 2.x.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FunctionCallingConfigMode {
    #[serde(rename = "AUTO")]
    Auto,
    #[serde(rename = "NONE")]
    None,
    #[serde(rename = "ANY")]
    Any,
    #[serde(rename = "VALIDATED")]
    Validated,
}

impl FunctionCallingConfigMode {
    pub fn as_str(self) -> &'static str {
        match self {
            FunctionCallingConfigMode::Auto => "AUTO",
            FunctionCallingConfigMode::None => "NONE",
            FunctionCallingConfigMode::Any => "ANY",
            FunctionCallingConfigMode::Validated => "VALIDATED",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionCallingConfig {
    pub mode: FunctionCallingConfigMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolConfig {
    pub function_calling_config: FunctionCallingConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_config: Option<ThinkingConfig>,
    /// Structured output: everything else the caller sets through the
    /// `generationConfig` provider option (`responseMimeType`,
    /// `responseSchema`, `responseJsonSchema`, `topP`, `stopSequences`, ...).
    /// Upstream never populates these; they exist so callers do not have to
    /// rewrite the whole payload through `on_payload`.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

/// The POST body for `:streamGenerateContent`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentRequest {
    pub contents: Vec<Content>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<Content>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_config: Option<ToolConfig>,
    /// Not sent by upstream; populated from the `safetySettings` provider option.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety_settings: Option<Value>,
    /// Not sent by upstream; populated from the `cachedContent` provider option.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_content: Option<String>,
    /// Always present, even when empty — the SDK unconditionally emits it.
    pub generation_config: GenerationConfig,
}

/// Counts are signed because they feed `Usage` directly, whose counters are
/// signed so corrective records can carry negative deltas.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageMetadata {
    #[serde(default)]
    pub prompt_token_count: Option<i64>,
    #[serde(default)]
    pub candidates_token_count: Option<i64>,
    #[serde(default)]
    pub cached_content_token_count: Option<i64>,
    #[serde(default)]
    pub thoughts_token_count: Option<i64>,
    #[serde(default)]
    pub total_token_count: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    #[serde(default)]
    pub content: Option<Content>,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentResponse {
    #[serde(default)]
    pub candidates: Option<Vec<Candidate>>,
    #[serde(default)]
    pub usage_metadata: Option<UsageMetadata>,
    #[serde(default)]
    pub response_id: Option<String>,
    #[serde(default)]
    pub model_version: Option<String>,
    /// Present when the request itself was rejected mid-stream.
    #[serde(default)]
    pub error: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_request_still_carries_generation_config() {
        let req = GenerateContentRequest {
            contents: vec![Content::new("user", vec![Part::text("hi")])],
            ..Default::default()
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
                "generationConfig": {}
            })
        );
    }

    #[test]
    fn generation_config_extra_keys_flatten() {
        let mut config = GenerationConfig {
            temperature: Some(0.5),
            ..Default::default()
        };
        config
            .extra
            .insert("responseMimeType".into(), "application/json".into());
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["temperature"], 0.5);
        assert_eq!(json["responseMimeType"], "application/json");
    }
}
