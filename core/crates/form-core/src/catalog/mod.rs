//! Provider and model catalog.
//!
//! **Owner: W4** (`docs/specs/04-catalog-settings.md`).
//!
//! The data is compiled in from `data/catalog.json` and parsed once behind a `OnceLock`.
//! The shape mirrors `pi`'s provider/model descriptors, so swapping in `pi-catalog` later is
//! a change to [`load`] and nothing else.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::protocol::{Cost, ModelRef, ThinkingLevel, Usage};

mod search;

#[cfg(test)]
mod tests;

pub use search::{search, ModelHit};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthMethod {
    ApiKey,
    // camelCase would spell this `oAuth`; the spec's wire value is `oauth`.
    #[serde(rename = "oauth")]
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
    /// `pi`'s ladder, filtered to what this model actually offers (F8.2). Never empty: a
    /// model with no reasoning capability lists exactly `[off]`.
    pub thinking_levels: Vec<ThinkingLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released: Option<String>,
    #[serde(default)]
    pub deprecated: bool,
}

impl Model {
    /// What the picker should preselect when the user has expressed no preference: the
    /// middle of the ladder if the model offers it, otherwise the lowest rung it does.
    pub fn default_thinking_level(&self) -> ThinkingLevel {
        if self.thinking_levels.contains(&ThinkingLevel::Medium) {
            ThinkingLevel::Medium
        } else {
            self.thinking_levels
                .first()
                .copied()
                .unwrap_or(ThinkingLevel::Off)
        }
    }

    /// Snap a requested level onto this model's ladder: the highest offered rung that does
    /// not exceed the request, else the lowest offered. Used when a session or the settings
    /// document names a level the model does not support.
    pub fn clamp_thinking_level(&self, requested: ThinkingLevel) -> ThinkingLevel {
        if self.thinking_levels.contains(&requested) {
            return requested;
        }
        let want = level_rank(requested);
        self.thinking_levels
            .iter()
            .copied()
            .filter(|l| level_rank(*l) <= want)
            .max_by_key(|l| level_rank(*l))
            .or_else(|| self.thinking_levels.first().copied())
            .unwrap_or(ThinkingLevel::Off)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub auth: Vec<AuthMethod>,
    pub env_vars: Vec<String>,
    /// Free-form caveat rendered in the Providers tab — proxying, self-hosting, and so on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub models: Vec<Model>,
}

impl Provider {
    pub fn model(&self, model_id: &str) -> Option<&Model> {
        self.models.iter().find(|m| m.id == model_id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    /// Stamp on the hand-maintained data file; `pi-catalog` will supply its own.
    #[serde(default)]
    pub generated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub providers: Vec<Provider>,
}

impl Catalog {
    pub fn provider(&self, id: &str) -> Option<&Provider> {
        self.providers.iter().find(|p| p.id == id)
    }

    pub fn models(&self) -> impl Iterator<Item = (&Provider, &Model)> {
        self.providers
            .iter()
            .flat_map(|p| p.models.iter().map(move |m| (p, m)))
    }
}

const CATALOG_JSON: &str = include_str!("../../data/catalog.json");

static CATALOG: OnceLock<Catalog> = OnceLock::new();

/// The compiled-in catalog. Parsed once; the JSON is an asset of this crate, so a parse
/// failure is a build-time mistake and the `parses` test is what catches it.
pub fn load() -> &'static Catalog {
    CATALOG.get_or_init(|| {
        serde_json::from_str(CATALOG_JSON).expect("data/catalog.json is not a valid Catalog")
    })
}

/// Owned snapshot. `core.rs` routes `getCatalog` through this name and `stats` iterates it,
/// so it stays by value; use [`load`] or [`providers`] to avoid the clone.
pub fn builtin() -> Catalog {
    load().clone()
}

pub fn providers() -> &'static [Provider] {
    &load().providers
}

pub fn provider(id: &str) -> Option<&'static Provider> {
    load().provider(id)
}

/// Zero-copy resolution. Prefer this inside the core.
pub fn resolve_ref(model_ref: &ModelRef) -> Option<&'static Model> {
    load()
        .provider(&model_ref.provider_id)?
        .model(&model_ref.model_id)
}

/// Owned resolution, for callers that outlive a borrow of the catalog (and for the harness,
/// which needs pricing on another thread). Cloning ~200 bytes of static data is cheaper than
/// threading a lifetime through the boundary.
pub fn resolve(model_ref: &ModelRef) -> Option<Model> {
    resolve_ref(model_ref).cloned()
}

/// `"anthropic/claude-opus-5"`, optionally suffixed with `":high"` to pin the effort.
///
/// The model id may itself contain slashes (`openrouter/anthropic/claude-sonnet-4.5`), so
/// only the first segment is the provider. An unknown provider is rejected; an unknown model
/// id under a known provider is accepted, because local and proxied model lists are open.
pub fn parse_ref(s: &str) -> Option<ModelRef> {
    let s = s.trim();
    let (head, level) = match s.rsplit_once(':') {
        Some((head, tail)) => match parse_thinking_level(tail) {
            Some(level) => (head, Some(level)),
            // `qwen3:32b` and friends — the colon belongs to the model id.
            None => (s, None),
        },
        None => (s, None),
    };
    let (provider_id, model_id) = head.split_once('/')?;
    let (provider_id, model_id) = (provider_id.trim(), model_id.trim());
    if provider_id.is_empty() || model_id.is_empty() {
        return None;
    }
    let provider = load().provider(provider_id)?;
    let model = provider.model(model_id);
    let thinking_level = match (level, model) {
        (Some(level), Some(model)) => model.clamp_thinking_level(level),
        (Some(level), None) => level,
        (None, Some(model)) => model.default_thinking_level(),
        (None, None) => ThinkingLevel::Off,
    };
    Some(ModelRef {
        provider_id: provider_id.to_string(),
        model_id: model_id.to_string(),
        thinking_level,
    })
}

/// The inverse of [`parse_ref`], including the effort suffix.
pub fn format_ref(model_ref: &ModelRef) -> String {
    format!(
        "{}/{}:{}",
        model_ref.provider_id,
        model_ref.model_id,
        model_ref.thinking_level.as_str()
    )
}

/// The out-of-the-box model. Kept in sync with `app::default_model_ref`, which is what the
/// store stamps on a new session.
pub fn default_ref() -> ModelRef {
    let model_ref = ModelRef {
        provider_id: "anthropic".to_string(),
        model_id: "claude-opus-5".to_string(),
        thinking_level: ThinkingLevel::High,
    };
    debug_assert!(
        resolve_ref(&model_ref).is_some(),
        "default ref must resolve"
    );
    model_ref
}

/// Is this ref something the catalog knows how to bill and size? An unknown *model* under a
/// known provider is still usable (Ollama, OpenRouter), it just has no pricing.
pub fn is_known_provider(provider_id: &str) -> bool {
    load().provider(provider_id).is_some()
}

pub fn parse_thinking_level(s: &str) -> Option<ThinkingLevel> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "off" | "none" => ThinkingLevel::Off,
        "minimal" => ThinkingLevel::Minimal,
        "low" => ThinkingLevel::Low,
        "medium" => ThinkingLevel::Medium,
        "high" => ThinkingLevel::High,
        "xhigh" => ThinkingLevel::Xhigh,
        "max" => ThinkingLevel::Max,
        _ => return None,
    })
}

/// Position on `pi`'s ladder. Not on `ThinkingLevel` itself because `protocol` is frozen.
pub fn level_rank(level: ThinkingLevel) -> u8 {
    match level {
        ThinkingLevel::Off => 0,
        ThinkingLevel::Minimal => 1,
        ThinkingLevel::Low => 2,
        ThinkingLevel::Medium => 3,
        ThinkingLevel::High => 4,
        ThinkingLevel::Xhigh => 5,
        ThinkingLevel::Max => 6,
    }
}

/// Money for tokens. The single place pricing arithmetic happens, so the transcript footer,
/// the context popover and the Home dashboard cannot disagree.
///
/// `input` is assumed to exclude cache reads and writes, matching `pi`'s `Usage`.
pub fn price(model: &Model, usage: &Usage) -> Cost {
    let per_million = |tokens: u64, rate: f64| (tokens as f64) * rate / 1_000_000.0;
    let p = &model.pricing;
    let input = per_million(usage.input, p.input);
    let output = per_million(usage.output, p.output);
    let cache_read = per_million(usage.cache_read, p.cache_read);
    // A 1h cache entry costs 2x base input where a 5m entry costs 1.25x — hence 1.6x the
    // 5m write price. Providers without tiered caching report `None` and pay nothing extra.
    let cache_write = per_million(usage.cache_write, p.cache_write)
        + per_million(usage.cache_write_1h.unwrap_or(0), p.cache_write * 1.6);
    Cost {
        input,
        output,
        cache_read,
        cache_write,
        total: input + output + cache_read + cache_write,
    }
}
