//! The runtime provider/model registry.
//!
//! Port of the `Models` collection in `packages/ai/src/models.ts`, minus auth
//! resolution (that is `pi-auth`'s, W4) and minus the stream plumbing that
//! upstream folds into the same object.
//!
//! The critical difference from upstream: providers here declare *api ids*, and
//! the concrete `ApiClient` implementations are registered separately at
//! runtime via [`ModelRegistry::register_api`]. That inverts the dependency —
//! `pi-provider-*` crates depend on `pi-catalog`, never the reverse — and keeps
//! the registry constructible from Swift, which cannot supply Rust closures.
//!
//! The registry is internally synchronized so it can be shared as
//! `Arc<ModelRegistry>` and mutated from any thread; every method takes `&self`.

use std::collections::HashMap;

use indexmap::IndexMap;
use parking_lot::RwLock;
use pi_core::{Api, ApiClientRef, Model, ModelThinkingLevel, StreamFn};
use serde::{Deserialize, Serialize};

use crate::error::CatalogError;
use crate::model_catalog::builtin_models;
use crate::models::get_supported_thinking_levels;
use crate::models_store::{ModelsStore, ModelsStoreEntry};
use crate::providers::{builtin_providers, Provider};

/// Declarative model filter. Every `Some` field must match; `None` ignores that
/// dimension. Mirrors the ad-hoc predicates upstream applies over
/// `Models.getModels()`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<Api>,
    /// Match `Model::reasoning`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    /// Match image input support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_images: Option<bool>,
    /// Keep only models that accept this thinking level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ModelThinkingLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_max_tokens: Option<u64>,
    /// Ceiling on input $/Mtok. Useful for "cheap model" selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_input_cost: Option<f64>,
    /// Case-insensitive substring match on the model id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_contains: Option<String>,
    /// Keep only models whose api has a registered adapter — i.e. models this
    /// process can actually run.
    #[serde(default)]
    pub runnable_only: bool,
}

impl ModelFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    pub fn api(mut self, api: Api) -> Self {
        self.api = Some(api);
        self
    }

    pub fn reasoning(mut self, reasoning: bool) -> Self {
        self.reasoning = Some(reasoning);
        self
    }

    pub fn supports_images(mut self, supports: bool) -> Self {
        self.supports_images = Some(supports);
        self
    }

    pub fn thinking_level(mut self, level: ModelThinkingLevel) -> Self {
        self.thinking_level = Some(level);
        self
    }

    pub fn min_context_window(mut self, tokens: u64) -> Self {
        self.min_context_window = Some(tokens);
        self
    }

    pub fn min_max_tokens(mut self, tokens: u64) -> Self {
        self.min_max_tokens = Some(tokens);
        self
    }

    /// Ceiling on input $/Mtok.
    pub fn max_input_cost(mut self, dollars_per_mtok: f64) -> Self {
        self.max_input_cost = Some(dollars_per_mtok);
        self
    }

    /// Case-insensitive substring match on the model id.
    pub fn id_contains(mut self, needle: impl Into<String>) -> Self {
        self.id_contains = Some(needle.into());
        self
    }

    pub fn runnable_only(mut self, runnable: bool) -> Self {
        self.runnable_only = runnable;
        self
    }

    fn matches(&self, model: &Model, has_api: impl Fn(&Api) -> bool) -> bool {
        if let Some(provider) = &self.provider {
            if &model.provider != provider {
                return false;
            }
        }
        if let Some(api) = &self.api {
            if &model.api != api {
                return false;
            }
        }
        if let Some(reasoning) = self.reasoning {
            if model.reasoning != reasoning {
                return false;
            }
        }
        if let Some(images) = self.supports_images {
            if model.supports_images() != images {
                return false;
            }
        }
        if let Some(level) = self.thinking_level {
            if !get_supported_thinking_levels(model).contains(&level) {
                return false;
            }
        }
        if let Some(min) = self.min_context_window {
            if model.context_window < min {
                return false;
            }
        }
        if let Some(min) = self.min_max_tokens {
            if model.max_tokens < min {
                return false;
            }
        }
        if let Some(max) = self.max_input_cost {
            if model.cost.rates.input > max {
                return false;
            }
        }
        if let Some(needle) = &self.id_contains {
            if !model.id.to_lowercase().contains(&needle.to_lowercase()) {
                return false;
            }
        }
        if self.runnable_only && !has_api(&model.api) {
            return false;
        }
        true
    }
}

#[derive(Default)]
struct Inner {
    /// Insertion-ordered, like upstream's `Map`. Replacing a provider keeps its
    /// position, which listing order depends on.
    providers: IndexMap<String, Provider>,
    /// Api id -> adapter.
    apis: HashMap<String, ApiClientRef>,
}

/// Runtime collection of providers, models and api adapters.
#[derive(Default)]
pub struct ModelRegistry {
    inner: RwLock<Inner>,
}

impl std::fmt::Debug for ModelRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.read();
        f.debug_struct("ModelRegistry")
            .field("providers", &inner.providers.len())
            .field(
                "models",
                &inner
                    .providers
                    .values()
                    .map(|p| p.models.len())
                    .sum::<usize>(),
            )
            .field("apis", &inner.apis.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ModelRegistry {
    /// Empty registry: no providers, no adapters.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every built-in provider with its embedded catalog, in upstream order.
    /// Equivalent to upstream's `builtinModels()`, minus adapters — register
    /// those with [`ModelRegistry::register_api`].
    pub fn with_builtins() -> Self {
        let registry = Self::new();
        for provider in builtin_providers() {
            registry.set_provider(provider);
        }
        registry
    }

    // ---- api adapters -------------------------------------------------

    /// Register an adapter under its own [`pi_core::ApiClient::api`] id.
    /// Replaces any adapter already registered for that id.
    pub fn register_api(&self, client: ApiClientRef) {
        let api = client.api().to_string();
        self.inner.write().apis.insert(api, client);
    }

    /// Register an adapter under an explicit api id. Use this to serve a custom
    /// api id (`Api::Custom`) with an existing adapter, e.g. an OpenAI-compatible
    /// gateway.
    pub fn register_api_as(&self, api: &str, client: ApiClientRef) {
        self.inner.write().apis.insert(api.to_string(), client);
    }

    pub fn unregister_api(&self, api: &str) -> bool {
        self.inner.write().apis.remove(api).is_some()
    }

    /// Api ids with a registered adapter.
    pub fn api_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.inner.read().apis.keys().cloned().collect();
        ids.sort();
        ids
    }

    pub fn api_client(&self, api: &Api) -> Option<ApiClientRef> {
        self.inner.read().apis.get(api.as_str()).cloned()
    }

    /// The adapter that can serve this model.
    ///
    /// Upstream turns a missing implementation into a stream error rather than
    /// a throw; the caller here decides, since `CatalogError` converts into the
    /// `AiError` an error event carries.
    pub fn client_for_model(&self, model: &Model) -> Result<ApiClientRef, CatalogError> {
        let inner = self.inner.read();
        if !inner.providers.contains_key(&model.provider) {
            return Err(CatalogError::UnknownProvider {
                provider: model.provider.clone(),
            });
        }
        inner.apis.get(model.api.as_str()).cloned().ok_or_else(|| {
            CatalogError::NoApiImplementation {
                provider: model.provider.clone(),
                api: model.api.to_string(),
            }
        })
    }

    /// The model's adapter wrapped as a [`StreamFn`] for the agent loop.
    pub fn stream_fn_for_model(&self, model: &Model) -> Result<StreamFn, CatalogError> {
        Ok(pi_core::stream_fn_from_client(
            self.client_for_model(model)?,
        ))
    }

    // ---- providers ----------------------------------------------------

    /// Upsert by `provider.id`. An existing provider keeps its listing position.
    pub fn set_provider(&self, provider: Provider) {
        self.inner
            .write()
            .providers
            .insert(provider.id.clone(), provider);
    }

    pub fn delete_provider(&self, id: &str) -> bool {
        // shift_remove, not swap_remove: listing order must stay stable.
        self.inner.write().providers.shift_remove(id).is_some()
    }

    pub fn clear_providers(&self) {
        self.inner.write().providers.clear();
    }

    pub fn providers(&self) -> Vec<Provider> {
        self.inner.read().providers.values().cloned().collect()
    }

    pub fn provider(&self, id: &str) -> Option<Provider> {
        self.inner.read().providers.get(id).cloned()
    }

    pub fn provider_ids(&self) -> Vec<String> {
        self.inner.read().providers.keys().cloned().collect()
    }

    pub fn provider_count(&self) -> usize {
        self.inner.read().providers.len()
    }

    // ---- models -------------------------------------------------------

    /// Every model from every provider, in provider then catalog order.
    pub fn models(&self) -> Vec<Model> {
        self.inner
            .read()
            .providers
            .values()
            .flat_map(|provider| provider.models.iter().cloned())
            .collect()
    }

    /// One provider's models. Empty for an unknown provider, matching
    /// upstream's best-effort `getModels(provider)`.
    pub fn provider_models(&self, provider: &str) -> Vec<Model> {
        self.inner
            .read()
            .providers
            .get(provider)
            .map(|p| p.models.clone())
            .unwrap_or_default()
    }

    pub fn model_count(&self) -> usize {
        self.inner
            .read()
            .providers
            .values()
            .map(|p| p.models.len())
            .sum()
    }

    /// Exact lookup by provider and model id.
    pub fn get_model(&self, provider: &str, id: &str) -> Option<Model> {
        self.inner
            .read()
            .providers
            .get(provider)?
            .models
            .iter()
            .find(|model| model.id == id)
            .cloned()
    }

    /// Like [`ModelRegistry::get_model`] but distinguishes an unknown provider
    /// from an unknown model.
    pub fn require_model(&self, provider: &str, id: &str) -> Result<Model, CatalogError> {
        let inner = self.inner.read();
        let entry = inner
            .providers
            .get(provider)
            .ok_or_else(|| CatalogError::UnknownProvider {
                provider: provider.to_string(),
            })?;
        entry
            .models
            .iter()
            .find(|model| model.id == id)
            .cloned()
            .ok_or_else(|| CatalogError::UnknownModel {
                provider: provider.to_string(),
                model: id.to_string(),
            })
    }

    /// Replace one provider's model list. This is how a dynamic provider
    /// publishes a refreshed catalog (upstream: `RefreshModelsContext.publish`).
    pub fn set_provider_models(
        &self,
        provider: &str,
        models: Vec<Model>,
    ) -> Result<(), CatalogError> {
        let mut inner = self.inner.write();
        let entry =
            inner
                .providers
                .get_mut(provider)
                .ok_or_else(|| CatalogError::UnknownProvider {
                    provider: provider.to_string(),
                })?;
        entry.models = models;
        Ok(())
    }

    /// Upsert a single model into its own `model.provider`, replacing any model
    /// with the same id. This is the runtime hook for user-defined models.
    pub fn set_model(&self, model: Model) -> Result<(), CatalogError> {
        let mut inner = self.inner.write();
        let entry = inner.providers.get_mut(&model.provider).ok_or_else(|| {
            CatalogError::UnknownProvider {
                provider: model.provider.clone(),
            }
        })?;
        match entry.models.iter_mut().find(|m| m.id == model.id) {
            Some(existing) => *existing = model,
            None => entry.models.push(model),
        }
        Ok(())
    }

    pub fn remove_model(&self, provider: &str, id: &str) -> bool {
        let mut inner = self.inner.write();
        let Some(entry) = inner.providers.get_mut(provider) else {
            return false;
        };
        let before = entry.models.len();
        entry.models.retain(|model| model.id != id);
        entry.models.len() != before
    }

    /// Restore a provider's catalog to the embedded built-in data. Useful after
    /// a dynamic refresh needs to be rolled back.
    pub fn reset_provider_to_builtin(&self, provider: &str) -> Result<(), CatalogError> {
        self.set_provider_models(provider, builtin_models(provider))
    }

    // ---- listing / filtering ------------------------------------------

    /// Models matching `filter`, in registry order.
    pub fn list_models(&self, filter: &ModelFilter) -> Vec<Model> {
        let inner = self.inner.read();
        let has_api = |api: &Api| inner.apis.contains_key(api.as_str());
        inner
            .providers
            .values()
            .flat_map(|provider| provider.models.iter())
            .filter(|model| filter.matches(model, has_api))
            .cloned()
            .collect()
    }

    /// Models this process can actually run: their api has a registered adapter.
    pub fn runnable_models(&self) -> Vec<Model> {
        self.list_models(&ModelFilter::new().runnable_only(true))
    }

    /// Providers with at least one runnable model.
    pub fn runnable_providers(&self) -> Vec<Provider> {
        let inner = self.inner.read();
        inner
            .providers
            .values()
            .filter(|provider| {
                provider
                    .apis
                    .iter()
                    .any(|api| inner.apis.contains_key(api.as_str()))
            })
            .cloned()
            .collect()
    }

    // ---- reference resolution -----------------------------------------

    /// Resolve `"provider/model"` or a bare model id to a model.
    ///
    /// Port of `findExactModelReferenceMatch`
    /// (`packages/coding-agent/src/core/model-resolver.ts`). Matching is
    /// case-insensitive and proceeds in three stages, because model ids may
    /// themselves contain slashes (`openrouter/anthropic/claude-opus-4.6`):
    ///
    /// 1. the whole reference against canonical `provider/id`;
    /// 2. a split at the first `/` into provider + model id;
    /// 3. the whole reference as a bare model id.
    ///
    /// An ambiguous match at any stage stops the search rather than guessing,
    /// matching upstream, which returns `undefined` in that case.
    pub fn find_model(&self, reference: &str) -> Option<Model> {
        self.resolve_model(reference).ok()
    }

    /// [`ModelRegistry::find_model`] with a diagnostic error instead of `None`.
    pub fn resolve_model(&self, reference: &str) -> Result<Model, CatalogError> {
        let trimmed = reference.trim();
        if trimmed.is_empty() {
            return Err(CatalogError::UnresolvedModel {
                message: "Empty model reference".to_string(),
            });
        }
        let normalized = trimmed.to_lowercase();
        let inner = self.inner.read();
        let all = || inner.providers.values().flat_map(|p| p.models.iter());

        let canonical: Vec<&Model> = all()
            .filter(|m| format!("{}/{}", m.provider, m.id).to_lowercase() == normalized)
            .collect();
        if let Some(model) = single(&canonical) {
            return Ok(model.clone());
        }
        if canonical.len() > 1 {
            return Err(ambiguous(trimmed, &canonical));
        }

        if let Some((provider, model_id)) = trimmed.split_once('/') {
            let (provider, model_id) = (provider.trim(), model_id.trim());
            if !provider.is_empty() && !model_id.is_empty() {
                let matches: Vec<&Model> = all()
                    .filter(|m| {
                        m.provider.eq_ignore_ascii_case(provider)
                            && m.id.eq_ignore_ascii_case(model_id)
                    })
                    .collect();
                if let Some(model) = single(&matches) {
                    return Ok(model.clone());
                }
                if matches.len() > 1 {
                    return Err(ambiguous(trimmed, &matches));
                }
            }
        }

        let by_id: Vec<&Model> = all()
            .filter(|m| m.id.to_lowercase() == normalized)
            .collect();
        if let Some(model) = single(&by_id) {
            return Ok(model.clone());
        }
        if by_id.len() > 1 {
            return Err(ambiguous(trimmed, &by_id));
        }

        Err(CatalogError::UnresolvedModel {
            message: format!("No model matches \"{trimmed}\""),
        })
    }

    // ---- persisted catalogs -------------------------------------------

    /// Load a provider's cached catalog from `store` and install it. Returns
    /// whether anything was restored. No network access — this is the
    /// offline/cold-start path.
    pub async fn restore_provider_models(
        &self,
        store: &dyn ModelsStore,
        provider: &str,
    ) -> Result<bool, CatalogError> {
        let Some(entry) = store.read(provider).await? else {
            return Ok(false);
        };
        // Guard against a store entry that leaked another provider's models.
        let models: Vec<Model> = entry
            .models
            .into_iter()
            .filter(|model| model.provider == provider)
            .collect();
        if models.is_empty() {
            return Ok(false);
        }
        self.set_provider_models(provider, models)?;
        Ok(true)
    }

    /// Install a freshly fetched catalog and persist it.
    pub async fn publish_provider_models(
        &self,
        store: &dyn ModelsStore,
        provider: &str,
        models: Vec<Model>,
        checked_at: Option<i64>,
    ) -> Result<(), CatalogError> {
        self.set_provider_models(provider, models.clone())?;
        store
            .write(
                provider,
                ModelsStoreEntry {
                    models,
                    checked_at,
                    ..Default::default()
                },
            )
            .await
    }
}

fn single<'a>(matches: &[&'a Model]) -> Option<&'a Model> {
    match matches {
        [only] => Some(only),
        _ => None,
    }
}

fn ambiguous(reference: &str, matches: &[&Model]) -> CatalogError {
    let mut candidates: Vec<String> = matches
        .iter()
        .map(|m| format!("{}/{}", m.provider, m.id))
        .collect();
    candidates.sort();
    CatalogError::UnresolvedModel {
        message: format!(
            "Model reference \"{reference}\" is ambiguous: {}",
            candidates.join(", ")
        ),
    }
}
