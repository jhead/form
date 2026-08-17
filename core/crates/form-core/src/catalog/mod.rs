//! Provider and model catalog.
//!
//! **Owner: W4** (`docs/specs/04-catalog-settings.md`).
//!
//! What is here now is the shape plus a two-provider seed so the model picker has something
//! real to render. W4 replaces `builtin()` with `include_str!("../../data/catalog.json")`
//! covering every provider in F8.1, and adds fuzzy `search`.

use serde::{Deserialize, Serialize};

use crate::protocol::{ModelRef, ThinkingLevel};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthMethod {
    ApiKey,
    OAuth,
    None,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pricing {
    /// USD per 1M tokens.
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub vision: bool,
    pub tools: bool,
    pub reasoning: bool,
    pub caching: bool,
    pub streaming: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub name: String,
    pub family: String,
    pub context_window: u64,
    pub max_output: u64,
    pub pricing: Pricing,
    pub capabilities: Capabilities,
    pub thinking_levels: Vec<ThinkingLevel>,
    #[serde(default)]
    pub deprecated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub auth: Vec<AuthMethod>,
    pub env_vars: Vec<String>,
    pub models: Vec<Model>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub providers: Vec<Provider>,
}

/// TODO(W4): load the full catalog from `data/catalog.json` behind a `OnceLock`.
pub fn builtin() -> Catalog {
    let full = vec![
        ThinkingLevel::Off,
        ThinkingLevel::Minimal,
        ThinkingLevel::Low,
        ThinkingLevel::Medium,
        ThinkingLevel::High,
        ThinkingLevel::Xhigh,
        ThinkingLevel::Max,
    ];
    let caps = Capabilities {
        vision: true,
        tools: true,
        reasoning: true,
        caching: true,
        streaming: true,
    };
    Catalog {
        providers: vec![
            Provider {
                id: "anthropic".into(),
                name: "Anthropic".into(),
                base_url: "https://api.anthropic.com".into(),
                auth: vec![AuthMethod::ApiKey, AuthMethod::OAuth],
                env_vars: vec!["ANTHROPIC_API_KEY".into()],
                models: vec![
                    Model {
                        id: "claude-opus-5".into(),
                        name: "Opus 5".into(),
                        family: "claude".into(),
                        context_window: 200_000,
                        max_output: 64_000,
                        pricing: Pricing {
                            input: 5.0,
                            output: 25.0,
                            cache_read: 0.5,
                            cache_write: 6.25,
                        },
                        capabilities: caps.clone(),
                        thinking_levels: full.clone(),
                        deprecated: false,
                    },
                    Model {
                        id: "claude-sonnet-5".into(),
                        name: "Sonnet 5".into(),
                        family: "claude".into(),
                        context_window: 200_000,
                        max_output: 64_000,
                        pricing: Pricing {
                            input: 3.0,
                            output: 15.0,
                            cache_read: 0.3,
                            cache_write: 3.75,
                        },
                        capabilities: caps.clone(),
                        thinking_levels: full.clone(),
                        deprecated: false,
                    },
                ],
            },
            Provider {
                id: "openai".into(),
                name: "OpenAI".into(),
                base_url: "https://api.openai.com/v1".into(),
                auth: vec![AuthMethod::ApiKey],
                env_vars: vec!["OPENAI_API_KEY".into()],
                models: vec![Model {
                    id: "gpt-5".into(),
                    name: "GPT-5".into(),
                    family: "gpt".into(),
                    context_window: 400_000,
                    max_output: 128_000,
                    pricing: Pricing {
                        input: 1.25,
                        output: 10.0,
                        cache_read: 0.125,
                        cache_write: 0.0,
                    },
                    capabilities: caps,
                    thinking_levels: full,
                    deprecated: false,
                }],
            },
        ],
    }
}

pub fn resolve(model_ref: &ModelRef) -> Option<Model> {
    builtin()
        .providers
        .into_iter()
        .find(|p| p.id == model_ref.provider_id)?
        .models
        .into_iter()
        .find(|m| m.id == model_ref.model_id)
}
