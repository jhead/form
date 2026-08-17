//! Anthropic-specific stream options.
//!
//! Upstream these are extra fields on `AnthropicOptions extends StreamOptions`.
//! `pi_core::StreamOptions` is not generic, so they travel in its
//! `provider_options` map under the same camelCase keys the TypeScript uses.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use pi_core::options::StreamOptions;

/// Effort level for adaptive thinking models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnthropicEffort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl AnthropicEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            AnthropicEffort::Low => "low",
            AnthropicEffort::Medium => "medium",
            AnthropicEffort::High => "high",
            AnthropicEffort::Xhigh => "xhigh",
            AnthropicEffort::Max => "max",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "low" => AnthropicEffort::Low,
            "medium" => AnthropicEffort::Medium,
            "high" => AnthropicEffort::High,
            "xhigh" => AnthropicEffort::Xhigh,
            "max" => AnthropicEffort::Max,
            _ => return None,
        })
    }
}

/// How thinking content comes back in responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnthropicThinkingDisplay {
    Summarized,
    Omitted,
}

impl AnthropicThinkingDisplay {
    pub fn as_str(self) -> &'static str {
        match self {
            AnthropicThinkingDisplay::Summarized => "summarized",
            AnthropicThinkingDisplay::Omitted => "omitted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoiceMode {
    Auto,
    Any,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolChoiceTool {
    #[serde(rename = "type", default = "tool_choice_tool_kind")]
    pub kind: String,
    pub name: String,
}

fn tool_choice_tool_kind() -> String {
    "tool".to_string()
}

/// `"auto" | "any" | "none" | { type: "tool", name }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnthropicToolChoice {
    Mode(ToolChoiceMode),
    Tool(ToolChoiceTool),
}

impl AnthropicToolChoice {
    pub fn tool(name: impl Into<String>) -> Self {
        AnthropicToolChoice::Tool(ToolChoiceTool {
            kind: tool_choice_tool_kind(),
            name: name.into(),
        })
    }

    /// The `tool_choice` wire value.
    pub fn to_wire(&self) -> Value {
        match self {
            AnthropicToolChoice::Mode(ToolChoiceMode::Auto) => json!({ "type": "auto" }),
            AnthropicToolChoice::Mode(ToolChoiceMode::Any) => json!({ "type": "any" }),
            AnthropicToolChoice::Mode(ToolChoiceMode::None) => json!({ "type": "none" }),
            AnthropicToolChoice::Tool(tool) => json!({ "type": "tool", "name": tool.name }),
        }
    }
}

/// The provider-specific knobs of `AnthropicOptions`.
///
/// Read out of [`StreamOptions::provider_options`]; unknown keys are ignored.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnthropicOptions {
    /// Enable extended thinking. `None` omits the field entirely, `Some(false)`
    /// sends `thinking: { type: "disabled" }` for models that accept it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_enabled: Option<bool>,
    /// Token budget for budget-based thinking. Defaults to 1024 when thinking is
    /// enabled and no budget is given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_budget_tokens: Option<u64>,
    /// Effort level for adaptive thinking models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<AnthropicEffort>,
    /// Defaults to `summarized` when thinking is enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_display: Option<AnthropicThinkingDisplay>,
    /// Request the interleaved-thinking beta for non-adaptive models. Default true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interleaved_thinking: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<AnthropicToolChoice>,
}

impl AnthropicOptions {
    /// Read the Anthropic knobs out of a generic [`StreamOptions`].
    pub fn from_stream_options(options: &StreamOptions) -> Self {
        serde_json::from_value(Value::Object(options.provider_options.clone())).unwrap_or_default()
    }

    /// Write these knobs into a [`StreamOptions`]'s provider options.
    pub fn apply(&self, options: &mut StreamOptions) {
        if let Ok(Value::Object(map)) = serde_json::to_value(self) {
            for (key, value) in map {
                options.provider_options.insert(key, value);
            }
        }
    }

    pub fn to_provider_options(&self) -> Map<String, Value> {
        match serde_json::to_value(self) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        }
    }

    pub fn interleaved_thinking_enabled(&self) -> bool {
        self.interleaved_thinking.unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_provider_options() {
        let options = AnthropicOptions {
            thinking_enabled: Some(true),
            thinking_budget_tokens: Some(4096),
            effort: Some(AnthropicEffort::Xhigh),
            thinking_display: Some(AnthropicThinkingDisplay::Omitted),
            interleaved_thinking: Some(false),
            tool_choice: Some(AnthropicToolChoice::tool("bash")),
        };
        let mut stream = StreamOptions::default();
        options.apply(&mut stream);
        assert_eq!(stream.provider_options["thinkingEnabled"], json!(true));
        assert_eq!(stream.provider_options["effort"], json!("xhigh"));
        assert_eq!(AnthropicOptions::from_stream_options(&stream), options);
    }

    #[test]
    fn parses_string_tool_choice() {
        let mut stream = StreamOptions::default();
        stream
            .provider_options
            .insert("toolChoice".into(), json!("any"));
        let options = AnthropicOptions::from_stream_options(&stream);
        assert_eq!(
            options.tool_choice,
            Some(AnthropicToolChoice::Mode(ToolChoiceMode::Any))
        );
        assert_eq!(
            options.tool_choice.unwrap().to_wire(),
            json!({ "type": "any" })
        );
    }

    #[test]
    fn ignores_unknown_provider_options() {
        let mut stream = StreamOptions::default();
        stream
            .provider_options
            .insert("someOtherAdapterKey".into(), json!(1));
        assert_eq!(
            AnthropicOptions::from_stream_options(&stream),
            AnthropicOptions::default()
        );
    }
}
