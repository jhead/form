//! The generated model catalog (every provider's models) plus the provider
//! registry that resolves `provider/model` strings to a [`pi_core::Model`] and
//! an [`pi_core::ApiClient`].
//!
//! Port of `packages/ai/src/{models,models-store,model-catalog}.ts` and
//! `packages/ai/src/providers/*.ts`.
//!
//! # Layout
//!
//! - [`model_catalog`] — the ~1.3k generated models, embedded from `data/*.json`
//!   at compile time. See `README.md` for how to refresh them.
//! - [`providers`] — the built-in [`Provider`] descriptors (id, name, base URL,
//!   auth metadata, model list, api bindings).
//! - [`registry`] — [`ModelRegistry`], the runtime collection: lookup, listing,
//!   filtering, reference resolution, and runtime api-adapter registration.
//! - [`models_store`] — persisted catalogs for dynamic providers.
//! - [`models`] — thinking-level and cost helpers over a single model.
//!
//! # Decoupling from the provider crates
//!
//! `pi-catalog` deliberately has **no** dependency on `pi-provider-*`. A
//! provider descriptor names the api ids its models use; the matching
//! `Arc<dyn ApiClient>` adapters are supplied at runtime:
//!
//! ```no_run
//! use std::sync::Arc;
//! use pi_catalog::ModelRegistry;
//! # fn example(anthropic: pi_core::ApiClientRef) -> Option<()> {
//! let registry = ModelRegistry::with_builtins();
//! registry.register_api(anthropic); // keyed by `ApiClient::api()`
//!
//! let model = registry.find_model("anthropic/claude-sonnet-4-5")?;
//! let client = registry.client_for_model(&model).ok()?;
//! # let _ = client;
//! # Some(())
//! # }
//! ```

pub mod error;
pub mod model_catalog;
pub mod models;
pub mod models_store;
pub mod providers;
pub mod registry;

pub use error::CatalogError;
pub use model_catalog::{
    all_builtin_models, builtin_model, builtin_model_count, builtin_model_data_generated_at,
    builtin_model_data_schema_version, builtin_models, builtin_provider_ids,
};
pub use models::{
    calculate_cost, clamp_thinking_level, get_supported_thinking_levels, models_are_equal,
    rates_for_usage, supports_thinking, EXTENDED_THINKING_LEVELS,
};
pub use models_store::{InMemoryModelsStore, ModelsStore, ModelsStoreEntry};
pub use providers::{
    builtin_provider, builtin_providers, ApiKeyAuthInfo, OAuthAuthInfo, Provider, ProviderAuth,
};
pub use registry::{ModelFilter, ModelRegistry};
