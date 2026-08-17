//! Tool definitions and the request context handed to a provider.
//!
//! Upstream uses TypeBox `TSchema` for parameters. The Rust port carries JSON
//! Schema as `serde_json::Value`, which is what every provider wire format
//! needs anyway and keeps the type FFI-friendly.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::message::Message;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrammarFormat {
    OpenaiLark,
    OpenaiRegex,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GrammarVariants {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_lark: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_regex: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StrictMode {
    Prefer,
    Require,
}

/// Provider-side constrained sampling config for a tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConstrainedSamplingConfig {
    JsonSchema { strict: StrictMode },
    Grammar { variants: GrammarVariants },
}

/// `false | ConstrainedSamplingConfig` from TypeScript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConstrainedSampling {
    /// Serializes as `false`: explicitly disable constrained sampling.
    Disabled(bool),
    Config(ConstrainedSamplingConfig),
}

/// A tool exposed to the model. `parameters` is a JSON Schema object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constrained_sampling: Option<ConstrainedSampling>,
}

impl Tool {
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            constrained_sampling: None,
        }
    }

    /// An empty object schema, for tools that take no arguments.
    pub fn no_params(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self::new(
            name,
            description,
            serde_json::json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        )
    }
}

/// What gets sent to the model: system prompt, conversation, available tools.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
}

impl Context {
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            system_prompt: None,
            messages,
            tools: None,
        }
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub fn with_tools(mut self, tools: Vec<Tool>) -> Self {
        self.tools = Some(tools);
        self
    }

    pub fn tools(&self) -> &[Tool] {
        self.tools.as_deref().unwrap_or(&[])
    }
}
