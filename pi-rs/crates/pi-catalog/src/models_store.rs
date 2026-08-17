//! Persisted per-provider model catalogs.
//!
//! Port of `packages/ai/src/models-store.ts`. Dynamic providers (Radius, and
//! any custom provider that lists models over the network) cache their catalog
//! here so a cold start can restore it without network access.

use std::collections::HashMap;

use async_trait::async_trait;
use parking_lot::Mutex;
use pi_core::Model;
use serde::{Deserialize, Serialize};

use crate::error::CatalogError;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsStoreEntry {
    pub models: Vec<Model>,
    /// Unix ms from the remote catalog's `Last-Modified` header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<i64>,
    /// Unix ms of the last completed remote check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<i64>,
    /// Opaque validator from the remote catalog's `ETag`, stored verbatim
    /// (quotes included) and echoed back as `If-None-Match`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
}

/// Persistent model catalogs keyed by provider id.
///
/// Object-safe and `Send + Sync` so hosts can supply their own backing store
/// across the FFI boundary.
#[async_trait]
pub trait ModelsStore: Send + Sync + 'static {
    async fn read(&self, provider_id: &str) -> Result<Option<ModelsStoreEntry>, CatalogError>;
    async fn write(&self, provider_id: &str, entry: ModelsStoreEntry) -> Result<(), CatalogError>;
    async fn delete(&self, provider_id: &str) -> Result<(), CatalogError>;
}

/// Non-persistent default, matching upstream's `InMemoryModelsStore`.
#[derive(Debug, Default)]
pub struct InMemoryModelsStore {
    entries: Mutex<HashMap<String, ModelsStoreEntry>>,
}

impl InMemoryModelsStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ModelsStore for InMemoryModelsStore {
    async fn read(&self, provider_id: &str) -> Result<Option<ModelsStoreEntry>, CatalogError> {
        Ok(self.entries.lock().get(provider_id).cloned())
    }

    async fn write(&self, provider_id: &str, entry: ModelsStoreEntry) -> Result<(), CatalogError> {
        self.entries.lock().insert(provider_id.to_string(), entry);
        Ok(())
    }

    async fn delete(&self, provider_id: &str) -> Result<(), CatalogError> {
        self.entries.lock().remove(provider_id);
        Ok(())
    }
}
