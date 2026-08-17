//! Provider descriptors for `pi-catalog` to register at runtime.
//!
//! Port of `providers/google.ts` and `providers/google-vertex.ts`, minus the
//! two things that do not belong here: the generated model lists (they live in
//! `pi-catalog`) and the interactive `login` flows (they live in `pi-auth`).
//! What is left is the static metadata plus a factory for the `ApiClient`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use pi_core::api::ApiClientRef;
use pi_core::model::Api;

use crate::google_generative_ai::GoogleGenerativeAiClient;
use crate::google_vertex::GoogleVertexClient;

/// How a provider's credential is discovered without user interaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuthDescriptor {
    /// Human-readable credential name, e.g. `Gemini API key`.
    pub name: String,
    /// Env vars checked, in order, for an API key.
    #[serde(default)]
    pub api_key_env: Vec<String>,
    /// Other env vars the adapter reads (project, location, credentials path).
    #[serde(default)]
    pub extra_env: Vec<String>,
    /// True when the provider can authenticate from ambient Google Cloud
    /// credentials (ADC file or metadata server) with no API key at all.
    #[serde(default)]
    pub supports_google_credentials: bool,
}

/// Static provider metadata. Deliberately data-only so `pi-catalog` can own
/// registration and model lists without depending on this crate's internals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDescriptor {
    pub id: String,
    pub name: String,
    /// Absent for Vertex, whose endpoint is derived from project and location.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub api: Api,
    pub auth: ProviderAuthDescriptor,
}

/// `googleProvider()`.
pub fn google_provider_descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        id: "google".to_string(),
        name: "Google".to_string(),
        base_url: Some("https://generativelanguage.googleapis.com/v1beta".to_string()),
        api: Api::GoogleGenerativeAi,
        auth: ProviderAuthDescriptor {
            name: "Gemini API key".to_string(),
            api_key_env: vec!["GEMINI_API_KEY".to_string()],
            extra_env: Vec::new(),
            supports_google_credentials: false,
        },
    }
}

/// `googleVertexProvider()`. Vertex accepts an explicit API key or ambient
/// Google Cloud credentials, which additionally need project and location.
pub fn google_vertex_provider_descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        id: "google-vertex".to_string(),
        name: "Google Vertex AI".to_string(),
        base_url: None,
        api: Api::GoogleVertex,
        auth: ProviderAuthDescriptor {
            name: "Google Cloud credentials".to_string(),
            api_key_env: vec!["GOOGLE_CLOUD_API_KEY".to_string()],
            extra_env: vec![
                "GOOGLE_CLOUD_PROJECT".to_string(),
                "GCLOUD_PROJECT".to_string(),
                "GOOGLE_CLOUD_LOCATION".to_string(),
                "GOOGLE_APPLICATION_CREDENTIALS".to_string(),
            ],
            supports_google_credentials: true,
        },
    }
}

/// Both descriptors, for a registry that wants to enumerate them.
pub fn google_provider_descriptors() -> Vec<ProviderDescriptor> {
    vec![
        google_provider_descriptor(),
        google_vertex_provider_descriptor(),
    ]
}

/// The `ApiClient` for `google-generative-ai`.
pub fn google_generative_ai_client() -> ApiClientRef {
    Arc::new(GoogleGenerativeAiClient::new())
}

/// The `ApiClient` for `google-vertex`, using the ambient token chain.
pub fn google_vertex_client() -> ApiClientRef {
    Arc::new(GoogleVertexClient::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_match_upstream_metadata() {
        let google = google_provider_descriptor();
        assert_eq!(google.id, "google");
        assert_eq!(google.api, Api::GoogleGenerativeAi);
        assert_eq!(
            google.base_url.as_deref(),
            Some("https://generativelanguage.googleapis.com/v1beta")
        );
        assert_eq!(google.auth.api_key_env, vec!["GEMINI_API_KEY"]);

        let vertex = google_vertex_provider_descriptor();
        assert_eq!(vertex.id, "google-vertex");
        assert_eq!(vertex.api, Api::GoogleVertex);
        assert!(vertex.base_url.is_none());
        assert!(vertex.auth.supports_google_credentials);
        assert!(vertex
            .auth
            .extra_env
            .contains(&"GOOGLE_APPLICATION_CREDENTIALS".to_string()));
    }

    #[test]
    fn descriptors_round_trip_as_json() {
        let json = serde_json::to_value(google_vertex_provider_descriptor()).unwrap();
        assert_eq!(json["api"], "google-vertex");
        assert_eq!(json["auth"]["supportsGoogleCredentials"], true);
        let back: ProviderDescriptor = serde_json::from_value(json).unwrap();
        assert_eq!(back, google_vertex_provider_descriptor());
    }

    #[test]
    fn clients_report_their_api_ids() {
        assert_eq!(google_generative_ai_client().api(), "google-generative-ai");
        assert_eq!(google_vertex_client().api(), "google-vertex");
    }
}
