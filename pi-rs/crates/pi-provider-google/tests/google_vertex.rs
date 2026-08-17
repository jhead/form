//! `google-vertex` adapter: credential resolution, URL construction and the
//! shared streaming machinery.

mod common;

use std::sync::Arc;

use common::*;
use pretty_assertions::assert_eq;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use pi_core::message::{Message, StopReason, UserMessage};
use pi_core::options::{ProviderEnv, StreamOptions};
use pi_core::tool::Context;
use pi_core::{AiError, ApiClient};
use pi_provider_google::token_source::{
    GoogleAccessToken, GoogleTokenRequest, GoogleTokenSource, GoogleTokenSourceRef,
};
use pi_provider_google::{GoogleStreamOptionsExt, GoogleVertexClient, StaticTokenSource};

const MODEL_ID: &str = "gemini-2.5-flash";

fn context() -> Context {
    Context::new(vec![Message::User(UserMessage::text("hello"))])
}

/// A custom `model.baseUrl` is `ResourceScope::COLLECTION`, so the path is
/// `/v1/publishers/google/models/{model}:streamGenerateContent`.
fn collection_path() -> String {
    format!("/v1/publishers/google/models/{MODEL_ID}:streamGenerateContent")
}

async fn mock_vertex(auth_header: (&str, &str)) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(collection_path()))
        .and(query_param("alt", "sse"))
        .and(header(auth_header.0, auth_header.1))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(fixture("vertex_text_stream.sse"), "text/event-stream"),
        )
        .mount(&server)
        .await;
    server
}

fn static_tokens(token: &str) -> GoogleTokenSourceRef {
    Arc::new(StaticTokenSource::new(token))
}

#[tokio::test]
async fn adc_credentials_send_a_bearer_token_and_stream() {
    let server = mock_vertex(("Authorization", "Bearer ya29.test-token")).await;
    let model = vertex_model(&server.uri(), MODEL_ID);
    let client = GoogleVertexClient::with_http_client(http_client())
        .with_token_source(static_tokens("ya29.test-token"));

    let options = StreamOptions::default()
        .with_vertex_project("test-project")
        .with_vertex_location("us-central1");
    let stream = client.stream(&model, &context(), &options).await.unwrap();
    let (events, described) = collect_events(stream).await;

    assert_eq!(
        described,
        vec![
            "start",
            "text_start#0",
            "text_delta#0 \"ok\"",
            "text_end#0 \"ok\"",
            "done Stop",
        ]
    );
    let message = terminal(&events);
    assert_eq!(message.api, "google-vertex");
    assert_eq!(message.provider, "google-vertex");
    assert_eq!(message.response_id.as_deref(), Some("vertex-response-id"));
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.usage.total_tokens, 2);
}

#[tokio::test]
async fn a_real_api_key_uses_the_express_endpoint_and_skips_the_token_source() {
    let server = mock_vertex((
        "x-goog-api-key",
        "AIzaSyExampleRealisticLookingApiKey123456",
    ))
    .await;
    let model = vertex_model(&server.uri(), MODEL_ID);
    let client = GoogleVertexClient::with_http_client(http_client())
        .with_token_source(Arc::new(FailingTokenSource));

    let mut options = StreamOptions::default();
    options.request.api_key = Some("AIzaSyExampleRealisticLookingApiKey123456".into());
    // No project/location: the express endpoint does not need them.
    let stream = client.stream(&model, &context(), &options).await.unwrap();
    let (events, described) = collect_events(stream).await;

    assert_eq!(described.last().unwrap(), "done Stop");
    assert_eq!(terminal(&events).stop_reason, StopReason::Stop);
}

#[tokio::test]
async fn placeholder_api_keys_fall_back_to_the_token_source() {
    let server = mock_vertex(("Authorization", "Bearer adc-token")).await;
    let model = vertex_model(&server.uri(), MODEL_ID);
    let client = GoogleVertexClient::with_http_client(http_client())
        .with_token_source(static_tokens("adc-token"));

    for marker in ["<authenticated>", "gcp-vertex-credentials"] {
        let mut options = StreamOptions::default()
            .with_vertex_project("test-project")
            .with_vertex_location("us-central1");
        options.request.api_key = Some(marker.into());
        let stream = client.stream(&model, &context(), &options).await.unwrap();
        let (events, _) = collect_events(stream).await;
        assert_eq!(
            terminal(&events).stop_reason,
            StopReason::Stop,
            "marker {marker} should have used ADC"
        );
    }
}

#[tokio::test]
async fn project_and_location_come_from_the_scoped_env() {
    let server = mock_vertex(("Authorization", "Bearer adc-token")).await;
    let model = vertex_model(&server.uri(), MODEL_ID);
    let client = GoogleVertexClient::with_http_client(http_client())
        .with_token_source(static_tokens("adc-token"));

    let mut options = StreamOptions::default();
    options.request.env = ProviderEnv::from([
        (
            "GOOGLE_CLOUD_PROJECT".to_string(),
            "env-project".to_string(),
        ),
        (
            "GOOGLE_CLOUD_LOCATION".to_string(),
            "us-central1".to_string(),
        ),
    ]);
    let stream = client.stream(&model, &context(), &options).await.unwrap();
    let (events, _) = collect_events(stream).await;
    assert_eq!(terminal(&events).stop_reason, StopReason::Stop);
}

#[tokio::test]
async fn a_missing_project_is_an_error_event() {
    let server = mock_vertex(("Authorization", "Bearer adc-token")).await;
    let model = vertex_model(&server.uri(), MODEL_ID);
    let client = GoogleVertexClient::with_http_client(http_client())
        .with_token_source(static_tokens("adc-token"));

    // An empty scoped env cannot suppress the process env, so only assert the
    // error when the ambient environment has no project either.
    if std::env::var("GOOGLE_CLOUD_PROJECT").is_ok() || std::env::var("GCLOUD_PROJECT").is_ok() {
        return;
    }
    let stream = client
        .stream(&model, &context(), &StreamOptions::default())
        .await
        .unwrap();
    let (events, described) = collect_events(stream).await;

    assert_eq!(described.len(), 1);
    assert!(described[0].contains("Vertex AI requires a project ID"));
    assert_eq!(terminal(&events).stop_reason, StopReason::Error);
}

#[tokio::test]
async fn a_token_source_failure_becomes_an_error_event() {
    let server = mock_vertex(("Authorization", "Bearer never")).await;
    let model = vertex_model(&server.uri(), MODEL_ID);
    let client = GoogleVertexClient::with_http_client(http_client())
        .with_token_source(Arc::new(FailingTokenSource));

    let options = StreamOptions::default()
        .with_vertex_project("p")
        .with_vertex_location("us-central1");
    let stream = client.stream(&model, &context(), &options).await.unwrap();
    let (events, described) = collect_events(stream).await;

    assert_eq!(described.len(), 1);
    assert!(described[0].contains("no Google Cloud credentials"));
    assert_eq!(terminal(&events).stop_reason, StopReason::Error);
}

#[tokio::test]
async fn vertex_sends_the_same_payload_shape_as_gemini() {
    let server = mock_vertex(("Authorization", "Bearer adc-token")).await;
    let model = vertex_model(&server.uri(), MODEL_ID);
    let client = GoogleVertexClient::with_http_client(http_client())
        .with_token_source(static_tokens("adc-token"));

    let options = StreamOptions::default()
        .with_vertex_project("test-project")
        .with_vertex_location("us-central1");
    let stream = client.stream(&model, &context(), &options).await.unwrap();
    collect_events(stream).await;

    assert_eq!(
        recorded_body(&server).await,
        serde_json::json!({
            "contents": [{"role": "user", "parts": [{"text": "hello"}]}],
            "generationConfig": {}
        })
    );
}

struct FailingTokenSource;

#[async_trait::async_trait]
impl GoogleTokenSource for FailingTokenSource {
    fn id(&self) -> &str {
        "failing"
    }

    async fn token(&self, _request: &GoogleTokenRequest) -> Result<GoogleAccessToken, AiError> {
        Err(AiError::auth("no Google Cloud credentials available"))
    }
}
