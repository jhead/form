//! Adapter-specific request options.
//!
//! Upstream declares one `interface XOptions extends StreamOptions` per adapter.
//! The FFI rules here forbid generic/extensible public structs, so those extra
//! knobs travel in [`StreamOptions::provider_options`] under the keys below —
//! spelled exactly as the TypeScript field names so a JSON bridge round-trips.

use pi_core::model::ThinkingBudgets;
use pi_core::options::StreamOptions;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderOptionKey {
    /// `toolChoice` — passed through verbatim to the provider.
    ToolChoice,
    /// `reasoningEffort` — one of minimal/low/medium/high/xhigh/max.
    ReasoningEffort,
    /// `reasoningSummary` — auto/detailed/concise (Responses family).
    ReasoningSummary,
    /// `thinkingBudgets` — per-level token budgets.
    ThinkingBudgets,
    /// `serviceTier` — flex/priority/default.
    ServiceTier,
    /// `textVerbosity` — Codex only.
    TextVerbosity,
    /// Azure deployment/endpoint overrides.
    AzureApiVersion,
    AzureResourceName,
    AzureBaseUrl,
    AzureDeploymentName,
}

impl ProviderOptionKey {
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderOptionKey::ToolChoice => "toolChoice",
            ProviderOptionKey::ReasoningEffort => "reasoningEffort",
            ProviderOptionKey::ReasoningSummary => "reasoningSummary",
            ProviderOptionKey::ThinkingBudgets => "thinkingBudgets",
            ProviderOptionKey::ServiceTier => "serviceTier",
            ProviderOptionKey::TextVerbosity => "textVerbosity",
            ProviderOptionKey::AzureApiVersion => "azureApiVersion",
            ProviderOptionKey::AzureResourceName => "azureResourceName",
            ProviderOptionKey::AzureBaseUrl => "azureBaseUrl",
            ProviderOptionKey::AzureDeploymentName => "azureDeploymentName",
        }
    }
}

pub fn provider_opt_value(options: &StreamOptions, key: ProviderOptionKey) -> Option<Value> {
    options
        .provider_options
        .get(key.as_str())
        .filter(|v| !v.is_null())
        .cloned()
}

pub fn provider_opt_str(options: &StreamOptions, key: ProviderOptionKey) -> Option<String> {
    options
        .provider_options
        .get(key.as_str())
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Whether the key is present at all, including an explicit `null` — several
/// Responses branches distinguish "absent" from "explicitly null".
pub fn provider_opt_present(options: &StreamOptions, key: ProviderOptionKey) -> bool {
    options.provider_options.contains_key(key.as_str())
}

pub fn thinking_budgets_from(options: &StreamOptions) -> Option<ThinkingBudgets> {
    let value = options
        .provider_options
        .get(ProviderOptionKey::ThinkingBudgets.as_str())?;
    serde_json::from_value(value.clone()).ok()
}

/// Set a provider option, returning the options for chaining in tests.
pub fn with_provider_option(
    mut options: StreamOptions,
    key: ProviderOptionKey,
    value: Value,
) -> StreamOptions {
    options
        .provider_options
        .insert(key.as_str().to_string(), value);
    options
}
