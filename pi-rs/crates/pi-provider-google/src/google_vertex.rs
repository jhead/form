//! Port of `api/google-vertex.ts`.
//!
//! URL construction reproduces `@google/genai`'s `ApiClient.constructUrl` for
//! `vertexai: true`, because upstream configures the SDK rather than the wire:
//!
//! | credential | base | resource path |
//! |---|---|---|
//! | API key (express) | `https://aiplatform.googleapis.com` | `publishers/google/models/{model}` |
//! | ADC, `global` | `https://aiplatform.googleapis.com` | `projects/{p}/locations/global/publishers/google/models/{model}` |
//! | ADC, `us`/`eu` | `https://aiplatform.{loc}.rep.googleapis.com` | as above |
//! | ADC, regional | `https://{loc}-aiplatform.googleapis.com` | as above |
//! | custom `model.baseUrl` | that URL, `ResourceScope.COLLECTION` | `publishers/google/models/{model}` |

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;

use pi_core::model::{Model, ModelThinkingLevel, ThinkingBudgets};
use pi_core::options::{ProviderEnv, SimpleStreamOptions, StreamOptions};
use pi_core::tool::Context;
use pi_core::{AiError, ApiClient, AssistantMessageEventStream};
use pi_http::client::HttpClient;

use crate::google_shared::{is_gemini3_flash_model, is_gemini3_pro_model, ClampedThinkingLevel};
use crate::options::{GoogleOptions, GoogleStreamOptionsExt, GoogleThinking};
use crate::params::build_request_body;
use crate::stream::{start_stream, GoogleHttpRequest};
use crate::token_source::{default_token_source, GoogleTokenRequest, GoogleTokenSourceRef};
use crate::wire::GoogleThinkingLevel;
use pi_provider_common::simple_options::build_base_options;

pub const API: &str = "google-vertex";
const STREAM_ENDED_MESSAGE: &str = "Google Vertex stream ended without a finish reason";
const API_VERSION: &str = "v1";
/// A stored credential can carry this instead of a real key when the user
/// picked ADC; it must not be sent as an API key.
const GCP_VERTEX_CREDENTIALS_MARKER: &str = "gcp-vertex-credentials";
/// Vertex serves `us` and `eu` from residency endpoints.
const MULTI_REGIONAL_LOCATIONS: [&str; 2] = ["us", "eu"];

/// The `google-vertex` adapter.
#[derive(Clone)]
pub struct GoogleVertexClient {
    http: Arc<HttpClient>,
    tokens: GoogleTokenSourceRef,
}

impl Default for GoogleVertexClient {
    fn default() -> Self {
        Self::new()
    }
}

impl GoogleVertexClient {
    pub fn new() -> Self {
        let http = HttpClient::shared();
        Self {
            tokens: default_token_source(http.clone()),
            http,
        }
    }

    pub fn with_http_client(http: Arc<HttpClient>) -> Self {
        Self {
            tokens: default_token_source(http.clone()),
            http,
        }
    }

    /// Swap the credential provider — this is the seam `pi-auth` plugs into.
    pub fn with_token_source(mut self, tokens: GoogleTokenSourceRef) -> Self {
        self.tokens = tokens;
        self
    }

    async fn build(
        model: &Model,
        context: &Context,
        options: &StreamOptions,
        tokens: &GoogleTokenSourceRef,
    ) -> Result<GoogleHttpRequest, AiError> {
        let google = GoogleOptions::from_stream_options(options);
        let env = &options.request.env;
        let api_key = resolve_api_key(options.request.api_key.as_deref());
        let custom_base_url = resolve_custom_base_url(&model.base_url);

        let mut headers: BTreeMap<String, String> = BTreeMap::new();
        headers.insert("Content-Type".into(), "application/json".into());
        if let Some(model_headers) = &model.headers {
            headers.extend(model_headers.clone());
        }

        let url = match &api_key {
            Some(key) => {
                headers.insert("x-goog-api-key".into(), key.clone());
                build_url(custom_base_url.as_deref(), None)?
            }
            None => {
                let project = resolve_project(google.project.as_deref(), env)?;
                let location = resolve_location(google.location.as_deref(), env)?;
                let token = tokens
                    .token(&GoogleTokenRequest::cloud_platform(
                        env.clone(),
                        options.request.signal.clone(),
                    ))
                    .await?;
                headers.insert("Authorization".into(), token.header_value());
                build_url(
                    custom_base_url.as_deref(),
                    Some((project.as_str(), location.as_str())),
                )?
            }
        };
        let url = format!("{url}/{}:streamGenerateContent?alt=sse", model.id);

        let body = build_request_body(model, context, options, &google, false)?;
        Ok(GoogleHttpRequest {
            url,
            headers: pi_http::merge_headers(headers, &options.request.headers),
            body: serde_json::to_value(body)
                .map_err(|err| AiError::protocol(format!("cannot serialize request: {err}")))?,
        })
    }
}

/// Everything up to (but excluding) the model resource segment.
fn build_url(
    custom_base_url: Option<&str>,
    project_location: Option<(&str, &str)>,
) -> Result<String, AiError> {
    let (base, api_version) = match custom_base_url {
        // `ResourceScope::COLLECTION`: the caller's URL already covers project
        // and location, so nothing is prepended.
        Some(base) => (
            base.trim_end_matches('/').to_string(),
            if base_url_includes_api_version(base) {
                ""
            } else {
                API_VERSION
            },
        ),
        None => {
            let host = match project_location {
                None => "https://aiplatform.googleapis.com".to_string(),
                Some((_, "global")) => "https://aiplatform.googleapis.com".to_string(),
                Some((_, location)) if MULTI_REGIONAL_LOCATIONS.contains(&location) => {
                    format!("https://aiplatform.{location}.rep.googleapis.com")
                }
                Some((_, location)) => format!("https://{location}-aiplatform.googleapis.com"),
            };
            (host, API_VERSION)
        }
    };

    let mut url = base;
    if !api_version.is_empty() {
        url.push('/');
        url.push_str(api_version);
    }
    // The API-key (express) endpoint and custom collection base URLs never take
    // the project/location prefix.
    if custom_base_url.is_none() {
        if let Some((project, location)) = project_location {
            url.push_str(&format!("/projects/{project}/locations/{location}"));
        }
    }
    url.push_str("/publishers/google/models");
    Ok(url)
}

/// `/^v\d+(?:beta\d*)?$/` on any path segment.
fn base_url_includes_api_version(base_url: &str) -> bool {
    let path = base_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base_url);
    let path = path.split(['?', '#']).next().unwrap_or(path);
    path.split('/').any(|segment| {
        let Some(rest) = segment.strip_prefix('v') else {
            return false;
        };
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            return false;
        }
        let tail = &rest[digits..];
        tail.is_empty()
            || tail
                .strip_prefix("beta")
                .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()))
    })
}

/// The catalog stores `https://{location}-aiplatform.googleapis.com` as a
/// template; that placeholder is not a real custom base URL.
fn resolve_custom_base_url(base_url: &str) -> Option<String> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() || trimmed.contains("{location}") {
        return None;
    }
    Some(trimmed.to_string())
}

/// `/^<[^>]+>$/` — the auth layer's "configured but no literal key" placeholder.
fn is_placeholder_api_key(api_key: &str) -> bool {
    api_key.len() > 2
        && api_key.starts_with('<')
        && api_key.ends_with('>')
        && !api_key[1..api_key.len() - 1].contains('>')
}

fn resolve_api_key(api_key: Option<&str>) -> Option<String> {
    let api_key = api_key?.trim();
    if api_key.is_empty()
        || api_key == GCP_VERTEX_CREDENTIALS_MARKER
        || is_placeholder_api_key(api_key)
    {
        return None;
    }
    Some(api_key.to_string())
}

fn provider_env(name: &str, env: &ProviderEnv) -> Option<String> {
    env.get(name)
        .cloned()
        .or_else(|| std::env::var(name).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn resolve_project(explicit: Option<&str>, env: &ProviderEnv) -> Result<String, AiError> {
    explicit
        .map(str::to_string)
        .or_else(|| provider_env("GOOGLE_CLOUD_PROJECT", env))
        .or_else(|| provider_env("GCLOUD_PROJECT", env))
        .ok_or_else(|| {
            AiError::invalid_request(
                "Vertex AI requires a project ID. Set GOOGLE_CLOUD_PROJECT/GCLOUD_PROJECT or pass project in options.",
            )
        })
}

fn resolve_location(explicit: Option<&str>, env: &ProviderEnv) -> Result<String, AiError> {
    explicit
        .map(str::to_string)
        .or_else(|| provider_env("GOOGLE_CLOUD_LOCATION", env))
        .ok_or_else(|| {
            AiError::invalid_request(
                "Vertex AI requires a location. Set GOOGLE_CLOUD_LOCATION or pass location in options.",
            )
        })
}

#[async_trait]
impl ApiClient for GoogleVertexClient {
    fn api(&self) -> &str {
        API
    }

    async fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: &StreamOptions,
    ) -> Result<AssistantMessageEventStream, AiError> {
        let (model, options) = (model.clone(), options.clone());
        let build = {
            let (model, context, options, tokens) = (
                model.clone(),
                context.clone(),
                options.clone(),
                self.tokens.clone(),
            );
            Box::pin(async move { Self::build(&model, &context, &options, &tokens).await })
        };
        Ok(start_stream(
            API,
            STREAM_ENDED_MESSAGE,
            self.http.clone(),
            model,
            options,
            build,
        ))
    }

    async fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, AiError> {
        let stream_options = build_base_options(model, context, options, None);
        let thinking = resolve_thinking(model, options);
        self.stream(
            model,
            context,
            &stream_options.with_google_thinking(thinking),
        )
        .await
    }
}

/// Port of the Vertex `streamSimple` thinking branch. Unlike the Gemini copy it
/// has no Gemma 4 special case.
fn resolve_thinking(model: &Model, options: &SimpleStreamOptions) -> GoogleThinking {
    let Some(reasoning) = options.reasoning else {
        return GoogleThinking::disabled();
    };
    let clamped = model.clamp_thinking_level(ModelThinkingLevel::from(reasoning));
    let effort = ClampedThinkingLevel::from_model_level(clamped);

    if is_gemini3_pro_model(&model.id) || is_gemini3_flash_model(&model.id) {
        return GoogleThinking::with_level(gemini3_thinking_level(effort, &model.id));
    }
    GoogleThinking::with_budget(google_budget(
        &model.id,
        effort,
        options.thinking_budgets.as_ref(),
    ))
}

fn gemini3_thinking_level(effort: ClampedThinkingLevel, model_id: &str) -> GoogleThinkingLevel {
    if is_gemini3_pro_model(model_id) {
        return match effort {
            ClampedThinkingLevel::Minimal | ClampedThinkingLevel::Low => GoogleThinkingLevel::Low,
            ClampedThinkingLevel::Medium | ClampedThinkingLevel::High => GoogleThinkingLevel::High,
        };
    }
    match effort {
        ClampedThinkingLevel::Minimal => GoogleThinkingLevel::Minimal,
        ClampedThinkingLevel::Low => GoogleThinkingLevel::Low,
        ClampedThinkingLevel::Medium => GoogleThinkingLevel::Medium,
        ClampedThinkingLevel::High => GoogleThinkingLevel::High,
    }
}

/// Port of the Vertex `getGoogleBudget` — no Flash-Lite table upstream.
fn google_budget(
    model_id: &str,
    effort: ClampedThinkingLevel,
    custom: Option<&ThinkingBudgets>,
) -> i64 {
    if let Some(budget) = custom.and_then(|budgets| effort.budget_from(budgets)) {
        return budget as i64;
    }
    let table: [i64; 4] = if model_id.contains("2.5-pro") {
        [128, 2048, 8192, 32768]
    } else if model_id.contains("2.5-flash") {
        [128, 2048, 8192, 24576]
    } else {
        return -1;
    };
    match effort {
        ClampedThinkingLevel::Minimal => table[0],
        ClampedThinkingLevel::Low => table[1],
        ClampedThinkingLevel::Medium => table[2],
        ClampedThinkingLevel::High => table[3],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::model::{Api, ThinkingLevel};

    fn model(id: &str) -> Model {
        let mut m = Model::new(
            id,
            Api::GoogleVertex,
            "google-vertex",
            "https://{location}-aiplatform.googleapis.com",
        );
        m.reasoning = true;
        m
    }

    // --- google-vertex-api-key-resolution.test.ts ---

    #[test]
    fn placeholders_and_markers_fall_back_to_adc() {
        assert_eq!(resolve_api_key(Some("<authenticated>")), None);
        assert_eq!(resolve_api_key(Some("gcp-vertex-credentials")), None);
        assert_eq!(resolve_api_key(Some("  ")), None);
        assert_eq!(resolve_api_key(None), None);
        assert_eq!(
            resolve_api_key(Some("AIzaSyExampleRealisticLookingApiKey123456")).as_deref(),
            Some("AIzaSyExampleRealisticLookingApiKey123456")
        );
    }

    #[test]
    fn generated_base_url_placeholder_is_not_a_custom_base_url() {
        assert_eq!(
            resolve_custom_base_url("https://{location}-aiplatform.googleapis.com"),
            None
        );
        assert_eq!(resolve_custom_base_url("  "), None);
        assert_eq!(
            resolve_custom_base_url("https://proxy.example.com").as_deref(),
            Some("https://proxy.example.com")
        );
    }

    #[test]
    fn adc_url_includes_project_and_location() {
        assert_eq!(
            build_url(None, Some(("test-project", "us-central1"))).unwrap(),
            "https://us-central1-aiplatform.googleapis.com/v1/projects/test-project/locations/us-central1/publishers/google/models"
        );
    }

    #[test]
    fn global_and_multi_regional_locations_use_dedicated_hosts() {
        assert!(build_url(None, Some(("p", "global")))
            .unwrap()
            .starts_with("https://aiplatform.googleapis.com/v1/projects/p/locations/global/"));
        assert!(build_url(None, Some(("p", "eu")))
            .unwrap()
            .starts_with("https://aiplatform.eu.rep.googleapis.com/v1/"));
    }

    #[test]
    fn api_key_uses_the_express_endpoint_without_project_or_location() {
        assert_eq!(
            build_url(None, None).unwrap(),
            "https://aiplatform.googleapis.com/v1/publishers/google/models"
        );
    }

    #[test]
    fn custom_base_url_is_collection_scoped() {
        assert_eq!(
            build_url(
                Some("https://proxy.example.com"),
                Some(("p", "us-central1"))
            )
            .unwrap(),
            "https://proxy.example.com/v1/publishers/google/models"
        );
    }

    #[test]
    fn custom_base_url_with_a_version_does_not_get_another_one() {
        assert_eq!(
            build_url(
                Some("https://proxy.example.com/v1/projects/test-project/locations/global"),
                Some(("test-project", "us-central1"))
            )
            .unwrap(),
            "https://proxy.example.com/v1/projects/test-project/locations/global/publishers/google/models"
        );
        assert!(base_url_includes_api_version("https://x/v1beta1/y"));
        assert!(base_url_includes_api_version("https://x/v1/y"));
        assert!(!base_url_includes_api_version("https://x/vertex/y"));
        assert!(!base_url_includes_api_version("https://proxy.example.com"));
    }

    #[test]
    fn project_and_location_are_required_for_adc() {
        let env = ProviderEnv::from([("GOOGLE_CLOUD_PROJECT".to_string(), "p".to_string())]);
        assert_eq!(resolve_project(None, &env).unwrap(), "p");
        let err = resolve_location(None, &ProviderEnv::new()).unwrap_err();
        assert!(err.message().contains("Vertex AI requires a location"));
    }

    #[test]
    fn scoped_env_beats_the_process_env() {
        let env = ProviderEnv::from([("GCLOUD_PROJECT".to_string(), "scoped".to_string())]);
        assert_eq!(resolve_project(None, &env).unwrap(), "scoped");
        assert_eq!(resolve_project(Some("explicit"), &env).unwrap(), "explicit");
    }

    #[test]
    fn vertex_thinking_has_no_gemma_branch() {
        let options = SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::Low),
            ..Default::default()
        };
        assert_eq!(
            resolve_thinking(&model("gemma-4-27b"), &options),
            GoogleThinking::with_budget(-1)
        );
        assert_eq!(
            resolve_thinking(&model("gemini-3-pro-preview"), &options),
            GoogleThinking::with_level(GoogleThinkingLevel::Low)
        );
        // No Flash-Lite table: 2.5-flash-lite falls into the 2.5-flash budgets.
        assert_eq!(
            resolve_thinking(
                &model("gemini-2.5-flash-lite"),
                &SimpleStreamOptions {
                    reasoning: Some(ThinkingLevel::Minimal),
                    ..Default::default()
                }
            ),
            GoogleThinking::with_budget(128)
        );
    }
}
