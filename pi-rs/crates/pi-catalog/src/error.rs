//! Catalog/registry errors.
//!
//! Port of `ModelsError` / `ModelsErrorCode` from `packages/ai/src/auth/resolve.ts`,
//! narrowed to the codes this crate can raise (auth resolution itself lives in
//! `pi-auth`). Flat and code-tagged per the workspace FFI rules.

use pi_core::AiError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum CatalogError {
    /// No provider registered under this id.
    #[error("Unknown provider: {provider}")]
    UnknownProvider { provider: String },

    /// The provider exists but has no model with this id.
    #[error("Unknown model: {provider}/{model}")]
    UnknownModel { provider: String, model: String },

    /// A model reference matched nothing, or matched more than one model.
    #[error("{message}")]
    UnresolvedModel { message: String },

    /// No `ApiClient` has been registered for the model's api id.
    #[error("Provider {provider} has no API implementation for \"{api}\"")]
    NoApiImplementation { provider: String, api: String },

    /// Persisted model-catalog storage failed.
    #[error("Model source failed for {provider}: {message}")]
    ModelSource { provider: String, message: String },
}

impl CatalogError {
    /// Stable machine-readable code. Do not change these strings: FFI callers
    /// and telemetry attributes depend on them.
    pub fn code(&self) -> &'static str {
        match self {
            CatalogError::UnknownProvider { .. } => "provider",
            CatalogError::UnknownModel { .. } => "model",
            CatalogError::UnresolvedModel { .. } => "unresolved_model",
            CatalogError::NoApiImplementation { .. } => "stream",
            CatalogError::ModelSource { .. } => "model_source",
        }
    }

    pub fn message(&self) -> String {
        self.to_string()
    }
}

impl From<CatalogError> for AiError {
    fn from(error: CatalogError) -> Self {
        match &error {
            // Upstream surfaces a missing api implementation as a stream error
            // rather than a caller mistake, so it maps to `Unsupported`.
            CatalogError::NoApiImplementation { .. } => AiError::unsupported(error.to_string()),
            CatalogError::ModelSource { .. } => AiError::other(error.to_string()),
            _ => AiError::invalid_request(error.to_string()),
        }
    }
}
