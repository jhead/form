//! Provider descriptors.
//!
//! Port of the provider half of `packages/ai/src/providers/{mistral,radius}.ts`
//! and `createProvider` from `models.ts`, reduced to data.
//!
//! `pi-catalog` owns provider resolution and must not depend on the adapter
//! crates, so each adapter here exposes a plain, serde-derivable
//! [`ProviderDescriptor`] plus the [`ApiClientRef`] that serves it. The catalog
//! registers the pair at runtime.

use pi_core::api::ApiClientRef;
use pi_core::model::Model;
use serde::{Deserialize, Serialize};

/// Static metadata about a provider: everything a registry needs except the
/// live client and the credential store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDescriptor {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// The API binding, matching [`pi_core::api::ApiClient::api`].
    pub api: String,
    /// Environment variables checked for an API key, in priority order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub api_key_env: Vec<String>,
    /// Human-readable credential label, e.g. `"Mistral API key"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_label: Option<String>,
    /// Models known statically. Dynamic providers ship an empty list and let
    /// the catalog fill it in.
    #[serde(default)]
    pub models: Vec<Model>,
}

impl ProviderDescriptor {
    pub fn new(id: impl Into<String>, name: impl Into<String>, api: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            base_url: None,
            api: api.into(),
            api_key_env: Vec::new(),
            api_key_label: None,
            models: Vec::new(),
        }
    }

    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    pub fn api_key(mut self, label: impl Into<String>, env: &[&str]) -> Self {
        self.api_key_label = Some(label.into());
        self.api_key_env = env.iter().map(|name| name.to_string()).collect();
        self
    }

    pub fn models(mut self, models: Vec<Model>) -> Self {
        self.models = models;
        self
    }
}

/// A descriptor together with the adapter that serves it.
#[derive(Clone)]
pub struct ProviderRegistration {
    pub descriptor: ProviderDescriptor,
    pub client: ApiClientRef,
}

impl std::fmt::Debug for ProviderRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistration")
            .field("descriptor", &self.descriptor)
            .field("client", &self.client.api())
            .finish()
    }
}
