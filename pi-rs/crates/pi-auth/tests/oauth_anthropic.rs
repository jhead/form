//! Port of `.upstream/packages/ai/test/anthropic-oauth.test.ts`.
//! Every endpoint is a `wiremock` mock; nothing here reaches Anthropic.

mod support;

use pi_auth::{AnthropicOAuth, OAuthCredential, OAuthFlow, OAuthHttp};
use pi_core::message::now_ms;
use pi_core::options::AbortSignal;
use serde_json::{json, Value};
use support::TestInteraction;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const REDIRECT_URI: &str = "http://localhost:53692/callback";

fn flow(server: &MockServer) -> AnthropicOAuth {
    AnthropicOAuth::new(OAuthHttp::default())
        .with_urls(AUTHORIZE_URL, format!("{}/v1/oauth/token", server.uri()))
}

async fn token_endpoint(server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(header("content-type", "application/json"))
        .respond_with(response)
        .expect(1)
        .mount(server)
        .await;
}

fn request_body(server_requests: &[Request]) -> Value {
    serde_json::from_slice(&server_requests[0].body).expect("json body")
}

#[tokio::test]
async fn login_keeps_the_localhost_redirect_uri_for_manual_callback_login() {
    let server = MockServer::start().await;
    token_endpoint(
        &server,
        ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access-token",
            "refresh_token": "refresh-token",
            "expires_in": 3600,
        })),
    )
    .await;

    // The host pastes the redirect URL back, echoing the state from the auth URL.
    let (interaction, ctx) = TestInteraction::new()
        .with_manual_answer(|recorded| {
            let state = recorded.query_param("state").expect("state in auth url");
            let redirect_uri = recorded
                .query_param("redirect_uri")
                .expect("redirect_uri in auth url");
            Ok(format!("{redirect_uri}?code=manual-code&state={state}"))
        })
        .into_context();

    let credential = flow(&server).login(&ctx).await.unwrap();

    assert_eq!(credential.access, "access-token");
    assert_eq!(credential.refresh, "refresh-token");

    let body = request_body(&server.received_requests().await.unwrap());
    assert_eq!(body["grant_type"], "authorization_code");
    assert_eq!(body["code"], "manual-code");
    assert_eq!(body["redirect_uri"], REDIRECT_URI);
    assert_eq!(body["client_id"], CLIENT_ID);
    assert_eq!(
        body["code_verifier"],
        recorded_verifier(&interaction.recorded())
    );
}

/// The verifier doubles as the state parameter in this flow.
fn recorded_verifier(recorded: &support::Recorded) -> String {
    recorded.query_param("state").expect("state in auth url")
}

#[tokio::test]
async fn the_authorization_url_carries_the_pkce_challenge_and_scopes() {
    let server = MockServer::start().await;
    token_endpoint(
        &server,
        ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "a", "refresh_token": "r", "expires_in": 3600,
        })),
    )
    .await;

    let (interaction, ctx) = TestInteraction::new()
        .with_manual_answer(|_| Ok("the-code".into()))
        .into_context();
    flow(&server).login(&ctx).await.unwrap();

    let recorded = interaction.recorded();
    assert!(recorded.auth_url().starts_with(AUTHORIZE_URL));
    assert_eq!(recorded.query_param("code").as_deref(), Some("true"));
    assert_eq!(
        recorded.query_param("response_type").as_deref(),
        Some("code")
    );
    assert_eq!(
        recorded.query_param("client_id").as_deref(),
        Some(CLIENT_ID)
    );
    assert_eq!(
        recorded.query_param("redirect_uri").as_deref(),
        Some(REDIRECT_URI)
    );
    assert_eq!(
        recorded.query_param("code_challenge_method").as_deref(),
        Some("S256")
    );
    assert_eq!(
        recorded.query_param("scope").as_deref(),
        Some("org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload")
    );
    let challenge = recorded.query_param("code_challenge").expect("challenge");
    let verifier = recorded.query_param("state").expect("state");
    assert_eq!(challenge, pi_auth::oauth::pkce_challenge(&verifier));

    // The host was asked to open the browser and offered a paste-back prompt.
    assert_eq!(recorded.auth_urls.len(), 1);
    assert!(recorded
        .prompts
        .iter()
        .any(|p| p.starts_with("manual_code:")));
}

#[tokio::test]
async fn refresh_omits_scope_and_returns_a_typed_credential() {
    let server = MockServer::start().await;
    token_endpoint(
        &server,
        ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "new-access-token",
            "refresh_token": "new-refresh-token",
            "expires_in": 3600,
        })),
    )
    .await;

    let credential = flow(&server)
        .refresh(
            &OAuthCredential::new("old-access-token", "refresh-token", 0),
            AbortSignal::never(),
        )
        .await
        .unwrap();

    assert_eq!(credential.access, "new-access-token");
    assert_eq!(credential.refresh, "new-refresh-token");
    // One hour minus the five-minute refresh skew.
    let expected = now_ms() + 3_600_000 - 5 * 60_000;
    assert!((credential.expires - expected).abs() < 2_000);

    let body = request_body(&server.received_requests().await.unwrap());
    assert_eq!(body["grant_type"], "refresh_token");
    assert_eq!(body["refresh_token"], "refresh-token");
    assert_eq!(body["client_id"], CLIENT_ID);
    assert!(body.get("scope").is_none(), "refresh must not send scope");
}

#[tokio::test]
async fn a_state_mismatch_in_the_pasted_url_fails_the_login() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let (_interaction, ctx) = TestInteraction::new()
        .with_manual_answer(|_| Ok("http://localhost:53692/callback?code=c&state=forged".into()))
        .into_context();

    let error = flow(&server).login(&ctx).await.unwrap_err();
    assert_eq!(error.code(), "oauth");
    assert!(error.message().contains("state mismatch"));
}

#[tokio::test]
async fn an_empty_paste_back_fails_without_exchanging_a_code() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let (_interaction, ctx) = TestInteraction::new()
        .with_manual_answer(|_| Ok("   ".into()))
        .into_context();

    let error = flow(&server).login(&ctx).await.unwrap_err();
    assert!(error.message().contains("Missing authorization code"));
}

#[tokio::test]
async fn a_failed_token_exchange_reports_the_status_and_body() {
    let server = MockServer::start().await;
    token_endpoint(
        &server,
        ResponseTemplate::new(400).set_body_json(json!({ "error": "invalid_grant" })),
    )
    .await;

    let (_interaction, ctx) = TestInteraction::new()
        .with_manual_answer(|_| Ok("the-code".into()))
        .into_context();

    let error = flow(&server).login(&ctx).await.unwrap_err();
    assert_eq!(error.code(), "oauth");
    assert!(error.message().contains("status=400"));
    assert!(error.message().contains("invalid_grant"));
}

#[tokio::test]
async fn a_headless_host_cannot_complete_the_browser_flow() {
    let server = MockServer::start().await;
    let ctx = pi_auth::LoginContext::headless();

    let error = flow(&server).login(&ctx).await.unwrap_err();
    assert_eq!(error.code(), "interaction");
}
