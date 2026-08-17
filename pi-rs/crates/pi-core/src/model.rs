//! Model descriptors, thinking levels and per-API compat flags.
//! Port of the `Model`/`*Compat` interfaces in `packages/ai/src/types.ts`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Known API identifiers. Unknown strings stay usable via [`Api::Custom`],
/// mirroring TypeScript's `KnownApi | (string & {})`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Api {
    OpenAiCompletions,
    MistralConversations,
    OpenAiResponses,
    AzureOpenAiResponses,
    OpenAiCodexResponses,
    AnthropicMessages,
    BedrockConverseStream,
    GoogleGenerativeAi,
    GoogleVertex,
    PiMessages,
    Custom(String),
}

impl Api {
    pub fn as_str(&self) -> &str {
        match self {
            Api::OpenAiCompletions => "openai-completions",
            Api::MistralConversations => "mistral-conversations",
            Api::OpenAiResponses => "openai-responses",
            Api::AzureOpenAiResponses => "azure-openai-responses",
            Api::OpenAiCodexResponses => "openai-codex-responses",
            Api::AnthropicMessages => "anthropic-messages",
            Api::BedrockConverseStream => "bedrock-converse-stream",
            Api::GoogleGenerativeAi => "google-generative-ai",
            Api::GoogleVertex => "google-vertex",
            Api::PiMessages => "pi-messages",
            Api::Custom(s) => s.as_str(),
        }
    }
}

impl From<String> for Api {
    fn from(s: String) -> Self {
        match s.as_str() {
            "openai-completions" => Api::OpenAiCompletions,
            "mistral-conversations" => Api::MistralConversations,
            "openai-responses" => Api::OpenAiResponses,
            "azure-openai-responses" => Api::AzureOpenAiResponses,
            "openai-codex-responses" => Api::OpenAiCodexResponses,
            "anthropic-messages" => Api::AnthropicMessages,
            "bedrock-converse-stream" => Api::BedrockConverseStream,
            "google-generative-ai" => Api::GoogleGenerativeAi,
            "google-vertex" => Api::GoogleVertex,
            "pi-messages" => Api::PiMessages,
            _ => Api::Custom(s),
        }
    }
}

impl From<Api> for String {
    fn from(a: Api) -> String {
        a.as_str().to_string()
    }
}

impl std::fmt::Display for Api {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Provider ids are open strings (`KnownProvider | string` upstream).
pub type ProviderId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThinkingLevel {
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ThinkingLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            ThinkingLevel::Minimal => "minimal",
            ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
            ThinkingLevel::Xhigh => "xhigh",
            ThinkingLevel::Max => "max",
        }
    }
}

/// `"off" | ThinkingLevel` — the level stored on models and agent config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ModelThinkingLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            ModelThinkingLevel::Off => "off",
            ModelThinkingLevel::Minimal => "minimal",
            ModelThinkingLevel::Low => "low",
            ModelThinkingLevel::Medium => "medium",
            ModelThinkingLevel::High => "high",
            ModelThinkingLevel::Xhigh => "xhigh",
            ModelThinkingLevel::Max => "max",
        }
    }

    pub fn level(self) -> Option<ThinkingLevel> {
        Some(match self {
            ModelThinkingLevel::Off => return None,
            ModelThinkingLevel::Minimal => ThinkingLevel::Minimal,
            ModelThinkingLevel::Low => ThinkingLevel::Low,
            ModelThinkingLevel::Medium => ThinkingLevel::Medium,
            ModelThinkingLevel::High => ThinkingLevel::High,
            ModelThinkingLevel::Xhigh => ThinkingLevel::Xhigh,
            ModelThinkingLevel::Max => ThinkingLevel::Max,
        })
    }
}

impl From<ThinkingLevel> for ModelThinkingLevel {
    fn from(l: ThinkingLevel) -> Self {
        match l {
            ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
            ThinkingLevel::Low => ModelThinkingLevel::Low,
            ThinkingLevel::Medium => ModelThinkingLevel::Medium,
            ThinkingLevel::High => ModelThinkingLevel::High,
            ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
            ThinkingLevel::Max => ModelThinkingLevel::Max,
        }
    }
}

/// Maps pi thinking levels to provider values. `None` value marks a level as
/// explicitly unsupported (TypeScript `null`); a missing key means "use the
/// provider default".
pub type ThinkingLevelMap = BTreeMap<ModelThinkingLevel, Option<String>>;

/// Token budgets per thinking level (token-based providers only).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingBudgets {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimal: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub medium: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CacheRetention {
    None,
    #[default]
    Short,
    Long,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    Sse,
    Websocket,
    WebsocketCached,
    #[default]
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionAffinityFormat {
    Openai,
    OpenaiNosession,
    Openrouter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modality {
    Text,
    Image,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostRates {
    /// $/million tokens.
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostTier {
    #[serde(flatten)]
    pub rates: ModelCostRates,
    /// Use this tier when total input usage exceeds this token count.
    pub input_tokens_above: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCost {
    #[serde(flatten)]
    pub rates: ModelCostRates,
    /// Request-wide pricing tiers; the highest matching threshold applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<ModelCostTier>>,
}

/// Per-API compatibility overrides. The TypeScript type is API-conditional; in
/// Rust it is a tagged-by-shape struct where each API reads the fields it knows.
/// Unknown keys are preserved in `extra` so custom providers round-trip.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCompat {
    // --- openai-completions ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_store: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_developer_role: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_reasoning_effort: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_usage_in_streaming: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_finish_reason: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_field: Option<MaxTokensField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_tool_result_name: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_assistant_after_tool_result: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_thinking_as_text: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_reasoning_content_on_assistant_messages: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_format: Option<ThinkingFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_template_kwargs: Option<serde_json::Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_template_args: Option<serde_json::Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_router_routing: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vercel_gateway_routing: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zai_tool_stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_thinking_token_budget: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_session_affinity_headers: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_tools_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_affinity_format: Option<SessionAffinityFormat>,

    // --- shared / openai-responses ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_long_cache_retention: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_strict_mode: Option<bool>,
    /// `rename_all = "camelCase"` would produce `supportsOpenaiGrammarTools`,
    /// but upstream's wire key capitalises the initialism. Without the explicit
    /// rename the flag silently falls through to `extra` on ~100 models.
    #[serde(
        rename = "supportsOpenAIGrammarTools",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_openai_grammar_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_additional_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_tool_search: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_explicit_prompt_cache_mode: Option<bool>,

    // --- anthropic-messages ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_eager_tool_input_streaming: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_cache_control_on_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_temperature: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force_adaptive_thinking: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_empty_signature: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_strict_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_tool_references: Option<bool>,

    /// Any provider-specific keys not modelled above.
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaxTokensField {
    MaxCompletionTokens,
    MaxTokens,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThinkingFormat {
    Openai,
    Openrouter,
    Deepseek,
    Together,
    Baseten,
    Zai,
    Qwen,
    ChatTemplate,
    QwenChatTemplate,
    StringThinking,
    AntLing,
}

/// Thinking levels in ascending order, matching upstream's
/// `EXTENDED_THINKING_LEVELS` in `packages/ai/src/models.ts`.
///
/// [`Model::clamp_thinking_level`] walks this order, so the ordering is
/// behaviour, not presentation.
pub const EXTENDED_THINKING_LEVELS: [ModelThinkingLevel; 7] = [
    ModelThinkingLevel::Off,
    ModelThinkingLevel::Minimal,
    ModelThinkingLevel::Low,
    ModelThinkingLevel::Medium,
    ModelThinkingLevel::High,
    ModelThinkingLevel::Xhigh,
    ModelThinkingLevel::Max,
];

/// A concrete model in the unified model system.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub name: String,
    pub api: Api,
    pub provider: ProviderId,
    pub base_url: String,
    pub reasoning: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    #[serde(default)]
    pub input: Vec<Modality>,
    pub cost: ModelCost,
    pub context_window: u64,
    pub max_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_params: Option<serde_json::Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<ModelCompat>,
}

impl Model {
    /// Minimal constructor for tests and custom providers.
    pub fn new(
        id: impl Into<String>,
        api: Api,
        provider: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        let id = id.into();
        Self {
            name: id.clone(),
            id,
            api,
            provider: provider.into(),
            base_url: base_url.into(),
            reasoning: false,
            thinking_level_map: None,
            input: vec![Modality::Text],
            cost: ModelCost::default(),
            context_window: 128_000,
            max_tokens: 8_192,
            sampling_params: None,
            headers: None,
            compat: None,
        }
    }

    pub fn supports_images(&self) -> bool {
        self.input.contains(&Modality::Image)
    }

    /// Thinking levels this model actually accepts.
    ///
    /// Port of `getSupportedThinkingLevels` from `packages/ai/src/models.ts`.
    /// It lives here rather than in `pi-catalog` because it reads nothing but
    /// [`Model`], and every crate that speaks to a provider needs it — several
    /// had grown their own copy.
    ///
    /// A `thinkingLevelMap` entry that is explicitly `null` upstream
    /// (`Some(None)` here) marks the level unsupported. `xhigh` and `max` are
    /// opt-in: a missing key excludes them, while for every other level a
    /// missing key means "use the provider default", which is supported.
    pub fn supported_thinking_levels(&self) -> Vec<ModelThinkingLevel> {
        if !self.reasoning {
            return vec![ModelThinkingLevel::Off];
        }

        EXTENDED_THINKING_LEVELS
            .into_iter()
            .filter(
                |level| match self.thinking_level_map.as_ref().and_then(|m| m.get(level)) {
                    Some(None) => false,
                    Some(Some(_)) => true,
                    None => !matches!(level, ModelThinkingLevel::Xhigh | ModelThinkingLevel::Max),
                },
            )
            .collect()
    }

    /// Nearest supported level to `level`: search upward through
    /// [`EXTENDED_THINKING_LEVELS`] first, then downward, falling back to the
    /// model's lowest supported level.
    ///
    /// Port of `clampThinkingLevel` from `packages/ai/src/models.ts`.
    pub fn clamp_thinking_level(&self, level: ModelThinkingLevel) -> ModelThinkingLevel {
        let available = self.supported_thinking_levels();
        if available.contains(&level) {
            return level;
        }

        let Some(requested) = EXTENDED_THINKING_LEVELS.iter().position(|l| *l == level) else {
            return available
                .first()
                .copied()
                .unwrap_or(ModelThinkingLevel::Off);
        };

        for candidate in &EXTENDED_THINKING_LEVELS[requested..] {
            if available.contains(candidate) {
                return *candidate;
            }
        }
        for candidate in EXTENDED_THINKING_LEVELS[..requested].iter().rev() {
            if available.contains(candidate) {
                return *candidate;
            }
        }
        available
            .first()
            .copied()
            .unwrap_or(ModelThinkingLevel::Off)
    }

    /// Cost rates that apply for a request with `input_tokens` of input usage.
    ///
    /// Tiers are request-wide: the highest threshold the usage *exceeds* wins.
    /// `input_tokens` is signed because it is summed from [`Usage`] counters,
    /// which carry corrective negatives; the comparison runs in `i128` so a
    /// negative total simply matches no tier.
    ///
    /// [`Usage`]: crate::message::Usage
    pub fn rates_for(&self, input_tokens: i64) -> &ModelCostRates {
        let input_tokens = i128::from(input_tokens);
        let mut best: &ModelCostRates = &self.cost.rates;
        // Seeded below zero, and compared with `>`, so that a tier at threshold
        // 0 is still selectable and ties resolve to the first tier declared —
        // upstream's `models.ts` semantics.
        let mut best_threshold: i128 = -1;
        if let Some(tiers) = &self.cost.tiers {
            for tier in tiers {
                let threshold = i128::from(tier.input_tokens_above);
                if input_tokens > threshold && threshold > best_threshold {
                    best_threshold = threshold;
                    best = &tier.rates;
                }
            }
        }
        best
    }
}

/// An image-generation model (`ImagesModel` upstream).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImagesModel {
    pub id: String,
    pub name: String,
    pub api: String,
    pub provider: ProviderId,
    pub base_url: String,
    #[serde(default)]
    pub input: Vec<Modality>,
    #[serde(default)]
    pub output: Vec<Modality>,
    pub cost: ModelCost,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reasoning_model() -> Model {
        let mut model = Model::new("m", Api::AnthropicMessages, "p", "https://example.test");
        model.reasoning = true;
        model
    }

    fn level_map(entries: &[(ModelThinkingLevel, Option<&str>)]) -> ThinkingLevelMap {
        entries
            .iter()
            .map(|(level, value)| (*level, value.map(str::to_string)))
            .collect()
    }

    #[test]
    fn a_non_reasoning_model_supports_only_off() {
        let model = Model::new("m", Api::AnthropicMessages, "p", "https://example.test");
        assert_eq!(
            model.supported_thinking_levels(),
            vec![ModelThinkingLevel::Off]
        );
    }

    #[test]
    fn xhigh_and_max_are_opt_in_and_explicit_nulls_remove_a_level() {
        // No map at all: every level except the opt-in pair.
        assert_eq!(
            reasoning_model().supported_thinking_levels(),
            vec![
                ModelThinkingLevel::Off,
                ModelThinkingLevel::Minimal,
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High,
            ]
        );

        let mut model = reasoning_model();
        model.thinking_level_map = Some(level_map(&[
            (ModelThinkingLevel::Minimal, None),
            (ModelThinkingLevel::Xhigh, Some("xhigh")),
        ]));
        assert_eq!(
            model.supported_thinking_levels(),
            vec![
                ModelThinkingLevel::Off,
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High,
                ModelThinkingLevel::Xhigh,
            ]
        );
    }

    #[test]
    fn clamping_searches_upward_before_downward() {
        let mut model = reasoning_model();
        model.thinking_level_map = Some(level_map(&[
            (ModelThinkingLevel::Minimal, None),
            (ModelThinkingLevel::Low, None),
        ]));

        // `low` is unsupported, so the search walks up to `medium` rather than
        // down to `off`.
        assert_eq!(
            model.clamp_thinking_level(ModelThinkingLevel::Low),
            ModelThinkingLevel::Medium
        );
        // Already supported levels pass through untouched.
        assert_eq!(
            model.clamp_thinking_level(ModelThinkingLevel::High),
            ModelThinkingLevel::High
        );
        // Nothing above `max`, so the downward pass picks the highest supported.
        assert_eq!(
            model.clamp_thinking_level(ModelThinkingLevel::Max),
            ModelThinkingLevel::High
        );
    }

    #[test]
    fn clamping_a_non_reasoning_model_always_yields_off() {
        let model = Model::new("m", Api::AnthropicMessages, "p", "https://example.test");
        assert_eq!(
            model.clamp_thinking_level(ModelThinkingLevel::High),
            ModelThinkingLevel::Off
        );
    }
}
