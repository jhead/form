//! The Vertex credential paths this crate owns: environment tokens, ADC files
//! (`authorized_user` and `service_account`) and the GCE metadata server.
//!
//! The key in `fixtures/test_service_account_key.pem` is a throwaway generated
//! for these tests and is not a credential for anything.

mod common;

use common::http_client;
use serde_json::Value;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use pi_core::options::ProviderEnv;
use pi_provider_google::token_source::{
    AdcFileTokenSource, CachingTokenSource, GoogleTokenRequest, GoogleTokenSource,
    MetadataServerTokenSource,
};

fn write_credentials(dir: &tempfile::TempDir, contents: Value) -> std::path::PathBuf {
    let path = dir.path().join("credentials.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&contents).unwrap()).unwrap();
    path
}

async fn token_endpoint(response: Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .mount(&server)
        .await;
    server
}

async fn single_request_body(server: &MockServer) -> Value {
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    serde_json::from_slice(&requests[0].body).unwrap()
}

#[tokio::test]
async fn authorized_user_credentials_refresh_into_an_access_token() {
    let server = token_endpoint(serde_json::json!({
        "access_token": "ya29.refreshed",
        "expires_in": 3599,
        "token_type": "Bearer"
    }))
    .await;
    let dir = tempfile::tempdir().unwrap();
    let path = write_credentials(
        &dir,
        serde_json::json!({
            "type": "authorized_user",
            "client_id": "cid.apps.googleusercontent.com",
            "client_secret": "secret",
            "refresh_token": "1//refresh"
        }),
    );

    let source = AdcFileTokenSource::new(http_client())
        .with_path(&path)
        .with_token_uri(format!("{}/token", server.uri()));
    let token = source
        .token(&GoogleTokenRequest::cloud_platform(
            ProviderEnv::new(),
            None,
        ))
        .await
        .unwrap();

    assert_eq!(token.header_value(), "Bearer ya29.refreshed");
    assert!(token.expires_at_ms.is_some());
    assert_eq!(
        single_request_body(&server).await,
        serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": "cid.apps.googleusercontent.com",
            "client_secret": "secret",
            "refresh_token": "1//refresh"
        })
    );
}

#[tokio::test]
async fn service_account_credentials_exchange_a_signed_jwt() {
    let server = token_endpoint(serde_json::json!({
        "access_token": "ya29.from-jwt",
        "expires_in": 3600
    }))
    .await;
    let dir = tempfile::tempdir().unwrap();
    let private_key = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/test_service_account_key.pem"),
    )
    .unwrap();
    let path = write_credentials(
        &dir,
        serde_json::json!({
            "type": "service_account",
            "client_email": "svc@example.iam.gserviceaccount.com",
            "private_key": private_key,
            "token_uri": "https://oauth2.googleapis.com/token"
        }),
    );

    let source = AdcFileTokenSource::new(http_client())
        .with_path(&path)
        .with_token_uri(format!("{}/token", server.uri()));
    let token = source
        .token(&GoogleTokenRequest::cloud_platform(
            ProviderEnv::new(),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(token.header_value(), "Bearer ya29.from-jwt");

    let body = single_request_body(&server).await;
    assert_eq!(
        body["grant_type"],
        "urn:ietf:params:oauth:grant-type:jwt-bearer"
    );
    let assertion = body["assertion"].as_str().unwrap();
    let segments: Vec<&str> = assertion.split('.').collect();
    assert_eq!(segments.len(), 3, "assertion must be a three-part JWT");

    let decode = |segment: &str| -> Value {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(segment)
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    };
    assert_eq!(
        decode(segments[0]),
        serde_json::json!({"alg": "RS256", "typ": "JWT"})
    );
    let claims = decode(segments[1]);
    assert_eq!(claims["iss"], "svc@example.iam.gserviceaccount.com");
    assert_eq!(
        claims["scope"],
        "https://www.googleapis.com/auth/cloud-platform"
    );
    // The audience is the credential's own token_uri, not the test transport.
    assert_eq!(claims["aud"], "https://oauth2.googleapis.com/token");
    assert_eq!(
        claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap(),
        3600
    );
    // RS256 over a 2048-bit key is 256 bytes, base64url-encoded without padding.
    assert_eq!(segments[2].len(), 342);
}

#[tokio::test]
async fn unsupported_credential_types_are_reported_not_guessed() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_credentials(
        &dir,
        serde_json::json!({"type": "external_account", "audience": "//iam.googleapis.com/x"}),
    );
    let err = AdcFileTokenSource::new(http_client())
        .with_path(&path)
        .token(&GoogleTokenRequest::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "unsupported");
    assert!(err.message().contains("external_account"));
}

#[tokio::test]
async fn a_missing_credentials_file_is_an_auth_error() {
    let err = AdcFileTokenSource::new(http_client())
        .with_path("/nonexistent/pi-google-credentials.json")
        .token(&GoogleTokenRequest::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "auth");
    assert!(err.message().contains("cannot read Google credentials"));
}

#[tokio::test]
async fn the_metadata_server_mints_a_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/computeMetadata/v1/instance/service-accounts/default/token",
        ))
        .and(header("Metadata-Flavor", "Google"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "ya29.metadata",
            "expires_in": 3600,
            "token_type": "Bearer"
        })))
        .mount(&server)
        .await;

    let source = MetadataServerTokenSource::new(http_client()).with_base_url(server.uri());
    let token = source
        .token(&GoogleTokenRequest::cloud_platform(
            ProviderEnv::new(),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(token.header_value(), "Bearer ya29.metadata");
}

#[tokio::test]
async fn gce_metadata_host_overrides_the_default_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/computeMetadata/v1/instance/service-accounts/default/token",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"access_token": "ya29.env-host"})),
        )
        .mount(&server)
        .await;

    let mut env = ProviderEnv::new();
    // The documented variable is a bare host; a full URL is accepted too.
    env.insert("GCE_METADATA_HOST".into(), server.uri());
    let token = MetadataServerTokenSource::new(http_client())
        .token(&GoogleTokenRequest::cloud_platform(env, None))
        .await
        .unwrap();
    assert_eq!(token.token, "ya29.env-host");
}

#[tokio::test]
async fn tokens_with_an_expiry_are_cached() {
    let server = token_endpoint(serde_json::json!({
        "access_token": "ya29.cached",
        "expires_in": 3600
    }))
    .await;
    let dir = tempfile::tempdir().unwrap();
    let path = write_credentials(
        &dir,
        serde_json::json!({
            "type": "authorized_user",
            "client_id": "cid",
            "client_secret": "secret",
            "refresh_token": "1//refresh"
        }),
    );
    let inner = AdcFileTokenSource::new(http_client())
        .with_path(&path)
        .with_token_uri(format!("{}/token", server.uri()));
    let caching = CachingTokenSource::new(std::sync::Arc::new(inner));

    for _ in 0..3 {
        let token = caching
            .token(&GoogleTokenRequest::cloud_platform(
                ProviderEnv::new(),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(token.token, "ya29.cached");
    }
    // Only the first call reached the token endpoint.
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}
