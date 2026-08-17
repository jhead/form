//! Port of `.upstream/packages/ai/test/radius-oauth.test.ts`.
//! `wiremock` stands in for the gateway; nothing here reaches a real Radius.

mod support;

use pi_auth::{OAuthCredential, OAuthFlow, OAuthHttp, RadiusOAuth};
use pi_core::message::now_ms;
use pi_core::options::AbortSignal;
use serde_json::json;
use support::TestInteraction;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN_EXPIRY_SKEW_MS: i64 = 60_000;

fn flow(server: &MockServer) -> RadiusOAuth {
    RadiusOAuth::new(OAuthHttp::default(), "Radius", server.uri())
}

async fn mount_device_authorization(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/v1/oauth/device"))
        .and(body_string_contains("client_id=pi-gateway"))
        .and(body_string_contains("scope=gateway+offline_access"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "device-code-123",
            "user_code": "ABCD-1234",
            "verification_uri": "https://gateway.example.com/device",
            // Clamped to the poller's one-second floor.
            "interval": 1,
            "expires_in": 600,
        })))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn uses_gateway_endpoints_directly_for_device_login() {
    let server = MockServer::start().await;
    // Device login must not hit the discovery endpoint.
    Mock::given(method("GET"))
        .and(path("/v1/oauth"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    mount_device_authorization(&server).await;

    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
        ))
        .and(body_string_contains("device_code=device-code-123"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(json!({ "error": "authorization_pending" })),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access-token",
            "refresh_token": "refresh-token",
            "expires_in": 3600,
            "scope": "gateway",
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (interaction, ctx) = TestInteraction::new()
        .with_select("device-code")
        .into_context();
    let credential = flow(&server).login(&ctx).await.unwrap();

    assert_eq!(credential.access, "access-token");
    assert_eq!(credential.refresh, "refresh-token");
    assert_eq!(credential.extra_str("scope"), Some("gateway"));
    let expected = now_ms() + 3_600_000 - TOKEN_EXPIRY_SKEW_MS;
    assert!((credential.expires - expected).abs() < 3_000);

    let recorded = interaction.recorded();
    assert_eq!(recorded.device_codes.len(), 1);
    assert_eq!(recorded.device_codes[0].user_code, "ABCD-1234");
    assert_eq!(
        recorded.prompts,
        vec!["select: Sign in to Radius:".to_string()]
    );
}

#[tokio::test]
async fn a_denied_device_authorization_fails_the_login() {
    let server = MockServer::start().await;
    mount_device_authorization(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "error": "access_denied" })))
        .mount(&server)
        .await;

    let (_interaction, ctx) = TestInteraction::new()
        .with_select("device-code")
        .into_context();
    let error = flow(&server).login(&ctx).await.unwrap_err();

    assert!(error.message().contains("denied"));
}

#[tokio::test]
async fn an_expired_device_code_fails_the_login() {
    let server = MockServer::start().await;
    mount_device_authorization(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "error": "expired_token" })))
        .mount(&server)
        .await;

    let (_interaction, ctx) = TestInteraction::new()
        .with_select("device-code")
        .into_context();
    let error = flow(&server).login(&ctx).await.unwrap_err();

    assert!(error.message().contains("expired"));
}

#[tokio::test]
async fn a_device_authorization_response_missing_fields_is_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/device"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "device_code": "d", "expires_in": 600 })),
        )
        .mount(&server)
        .await;

    let (_interaction, ctx) = TestInteraction::new()
        .with_select("device-code")
        .into_context();
    let error = flow(&server).login(&ctx).await.unwrap_err();

    assert!(error.message().contains("missing required fields"));
}

#[tokio::test]
async fn refreshes_directly_through_the_gateway_without_discovery() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/oauth"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("client_id=pi-gateway"))
        .and(body_string_contains("refresh_token=old-refresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "new-access",
            "refresh_token": "new-refresh",
            "expires_in": 3600,
        })))
        .expect(1)
        .mount(&server)
        .await;

    let credential = flow(&server)
        .refresh(
            &OAuthCredential::new("old-access", "old-refresh", 0),
            AbortSignal::never(),
        )
        .await
        .unwrap();

    assert_eq!(credential.access, "new-access");
    assert_eq!(credential.refresh, "new-refresh");
}

#[tokio::test]
async fn a_refresh_failure_surfaces_the_gateway_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "refresh token revoked",
        })))
        .mount(&server)
        .await;

    let error = flow(&server)
        .refresh(&OAuthCredential::new("a", "r", 0), AbortSignal::never())
        .await
        .unwrap_err();

    assert!(error
        .message()
        .contains("Radius OAuth token request failed: invalid_grant: refresh token revoked"));
}

#[tokio::test]
async fn discovers_only_the_interactive_browser_authorization_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/oauth"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authorizationEndpoint": "https://gateway.example.com/authorize",
        })))
        .expect(1)
        .mount(&server)
        .await;

    // Browser login without an opted-in callback server reports that clearly
    // rather than silently hanging; the discovery call still happens first.
    let (interaction, ctx) = TestInteraction::new().with_select("browser").into_context();
    let error = flow(&server).login(&ctx).await.unwrap_err();

    assert_eq!(error.code(), "interaction");
    assert!(error.message().contains("local callback server"));
    let recorded = interaction.recorded();
    assert_eq!(recorded.auth_urls.len(), 1);
    assert!(recorded
        .auth_url()
        .starts_with("https://gateway.example.com/authorize?"));
    assert_eq!(
        recorded.query_param("client_id").as_deref(),
        Some("pi-gateway")
    );
    assert_eq!(
        recorded.query_param("redirect_uri").as_deref(),
        Some("http://127.0.0.1:1456/oauth/callback")
    );
    assert_eq!(recorded.query_param("handoff").as_deref(), Some("url"));
    assert_eq!(
        recorded.query_param("code_challenge_method").as_deref(),
        Some("S256")
    );
}

#[tokio::test]
async fn an_invalid_discovery_document_is_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/oauth"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "unexpected": true })))
        .mount(&server)
        .await;

    let (_interaction, ctx) = TestInteraction::new().with_select("browser").into_context();
    let error = flow(&server).login(&ctx).await.unwrap_err();

    assert!(error.message().contains("Invalid Radius OAuth config"));
}

#[tokio::test]
async fn an_unknown_sign_in_method_is_rejected() {
    let server = MockServer::start().await;
    let (_interaction, ctx) = TestInteraction::new()
        .with_select("smoke-signals")
        .into_context();

    let error = flow(&server).login(&ctx).await.unwrap_err();
    assert!(error.message().contains("Unknown Radius sign-in method"));
}
