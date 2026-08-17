//! `GoogleOptions` / `GoogleVertexOptions` — the API-specific knobs upstream
//! declares on top of `StreamOptions`.
//!
//! `pi_core::StreamOptions` has no per-API generic parameter (FFI rule 1), so
//! these ride in `provider_options` under the same camelCase keys the
//! TypeScript interfaces use.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use pi_core::options::StreamOptions;

use crate::wire::GoogleThinkingLevel;

pub const TOOL_CHOICE: &str = "toolChoice";
pub const THINKING: &str = "thinking";
pub const PROJECT: &str = "project";
pub const LOCATION: &str = "location";
pub const SAFETY_SETTINGS: &str = "safetySettings";
pub const GENERATION_CONFIG: &str = "generationConfig";
pub const EXTRA_TOOLS: &str = "extraTools";
pub const CACHED_CONTENT: &str = "cachedContent";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoogleToolChoice {
    Auto,
    None,
    Any,
}

impl GoogleToolChoice {
    pub fn as_str(self) -> &'static str {
        match self {
            GoogleToolChoice::Auto => "auto",
            GoogleToolChoice::None => "none",
            GoogleToolChoice::Any => "any",
        }
    }
}

/// `GoogleOptions["thinking"]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleThinking {
    pub enabled: bool,
    /// `-1` for a dynamic budget, `0` to disable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<GoogleThinkingLevel>,
}

impl GoogleThinking {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            budget_tokens: None,
            level: None,
        }
    }

    pub fn with_level(level: GoogleThinkingLevel) -> Self {
        Self {
            enabled: true,
            budget_tokens: None,
            level: Some(level),
        }
    }

    pub fn with_budget(budget_tokens: i64) -> Self {
        Self {
            enabled: true,
            budget_tokens: Some(budget_tokens),
            level: None,
        }
    }
}

/// Everything both Google adapters read out of `provider_options`.
#[derive(Debug, Clone, Default)]
pub struct GoogleOptions {
    pub tool_choice: Option<GoogleToolChoice>,
    pub thinking: Option<GoogleThinking>,
    /// Vertex only.
    pub project: Option<String>,
    /// Vertex only.
    pub location: Option<String>,
    /// Extension points: upstream never sets these, but Gemini accepts them and
    /// rewriting the payload through `on_payload` for a safety setting is a
    /// poor trade. Absent options produce byte-identical requests to upstream.
    pub safety_settings: Option<Value>,
    pub generation_config: Map<String, Value>,
    pub extra_tools: Vec<Value>,
    pub cached_content: Option<String>,
}

impl GoogleOptions {
    pub fn from_stream_options(options: &StreamOptions) -> Self {
        Self {
            tool_choice: options.provider_option(TOOL_CHOICE),
            thinking: options.provider_option(THINKING),
            project: options.provider_option(PROJECT),
            location: options.provider_option(LOCATION),
            safety_settings: options.provider_options.get(SAFETY_SETTINGS).cloned(),
            generation_config: options
                .provider_option(GENERATION_CONFIG)
                .unwrap_or_default(),
            extra_tools: options.provider_option(EXTRA_TOOLS).unwrap_or_default(),
            cached_content: options.provider_option(CACHED_CONTENT),
        }
    }
}

/// Builder helpers so callers do not hand-roll `provider_options` JSON.
pub trait GoogleStreamOptionsExt {
    fn with_google_tool_choice(self, choice: GoogleToolChoice) -> Self;
    fn with_google_thinking(self, thinking: GoogleThinking) -> Self;
    fn with_vertex_project(self, project: impl Into<String>) -> Self;
    fn with_vertex_location(self, location: impl Into<String>) -> Self;
}

impl GoogleStreamOptionsExt for StreamOptions {
    fn with_google_tool_choice(self, choice: GoogleToolChoice) -> Self {
        self.with_provider_option(TOOL_CHOICE, serde_json::json!(choice))
    }

    fn with_google_thinking(self, thinking: GoogleThinking) -> Self {
        self.with_provider_option(THINKING, serde_json::json!(thinking))
    }

    fn with_vertex_project(self, project: impl Into<String>) -> Self {
        self.with_provider_option(PROJECT, Value::String(project.into()))
    }

    fn with_vertex_location(self, location: impl Into<String>) -> Self {
        self.with_provider_option(LOCATION, Value::String(location.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_provider_options() {
        let options = StreamOptions::default()
            .with_google_tool_choice(GoogleToolChoice::Any)
            .with_google_thinking(GoogleThinking::with_level(GoogleThinkingLevel::High))
            .with_vertex_project("p")
            .with_vertex_location("us-central1");
        let parsed = GoogleOptions::from_stream_options(&options);
        assert_eq!(parsed.tool_choice, Some(GoogleToolChoice::Any));
        assert_eq!(
            parsed.thinking.unwrap().level,
            Some(GoogleThinkingLevel::High)
        );
        assert_eq!(parsed.project.as_deref(), Some("p"));
        assert_eq!(parsed.location.as_deref(), Some("us-central1"));
    }

    #[test]
    fn thinking_serializes_like_the_typescript_shape() {
        let json = serde_json::to_value(GoogleThinking::with_budget(-1)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"enabled": true, "budgetTokens": -1})
        );
    }
}
