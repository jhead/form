//! Port of `.upstream/packages/ai/test/openai-codex-oauth.test.ts`.
//! `wiremock` serves every endpoint; nothing here reaches OpenAI.

mod support;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use pi_auth::{OAuthCredential, OAuthFlow, OAuthHttp, OpenAICodexOAuth};
use pi_core::message::now_ms;
use pi_core::options::AbortSignal;
use serde_json::json;
use support::TestInteraction;
use wiremock::matchers::{body_json, body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

fn flow(server: &MockServer) -> OpenAICodexOAuth {
    OpenAICodexOAuth::new(OAuthHttp::default()).with_auth_base_url(server.uri())
}

/// A JWT-shaped access token carrying the ChatGPT account id.
fn access_token(account_id: &str) -> String {
    let header = STANDARD.encode(json!({ "alg": "none" }).to_string());
    let payload = STANDARD.encode(
        json!({ "https://api.openai.com/auth": { "chatgpt_account_id": account_id } }).to_string(),
    );
    format!("{header}.{payload}.signature")
}

fn device_auth_pending() -> ResponseTemplate {
    ResponseTemplate::new(403).set_body_json(json!({
        "error": {
            "message": "Device authorization is pending. Please try again.",
            "type": "invalid_request_error",
            "code": "deviceauth_authorization_pending",
        }
    }))
}

async fn mount_user_code(server: &MockServer, interval: &str) {
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .and(header("content-type", "application/json"))
        .and(body_json(json!({ "client_id": CLIENT_ID })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_auth_id": "device-auth-id",
            "user_code": "ABCD-1234",
            // The endpoint really does return this as a string.
            "interval": interval,
        })))
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_token_exchange(server: &MockServer, token: &str) {
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(header("content-type", "application/x-www-form-urlencoded"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=oauth-code"))
        .and(body_string_contains("code_verifier=device-code-verifier"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": token,
            "refresh_token": "refresh-token",
            "expires_in": 3600,
        })))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn logs_in_with_the_device_code_flow() {
    let server = MockServer::start().await;
    let token = access_token("account-123");
    mount_user_code(&server, "1").await;

    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .and(body_json(json!({
            "device_auth_id": "device-auth-id",
            "user_code": "ABCD-1234",
        })))
        .respond_with(device_auth_pending())
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authorization_code": "oauth-code",
            "code_challenge": "device-code-challenge",
            "code_verifier": "device-code-verifier",
        })))
        .expect(1)
        .mount(&server)
        .await;
    mount_token_exchange(&server, &token).await;

    let (interaction, ctx) = TestInteraction::new()
        .with_select("device_code")
        .into_context();
    let credential = flow(&server).login(&ctx).await.unwrap();

    assert_eq!(credential.access, token);
    assert_eq!(credential.refresh, "refresh-token");
    assert_eq!(credential.extra_str("accountId"), Some("account-123"));
    assert!(credential.expires >= now_ms() + 3_600_000 - 5_000);

    let recorded = interaction.recorded();
    assert_eq!(recorded.device_codes.len(), 1);
    assert_eq!(recorded.device_codes[0].user_code, "ABCD-1234");
    assert_eq!(
        recorded.device_codes[0].verification_uri,
        format!("{}/codex/device", server.uri())
    );
    assert_eq!(recorded.device_codes[0].expires_in_seconds, Some(900));
}

#[tokio::test]
async fn offers_browser_login_first_and_honors_the_selected_method() {
    let server = MockServer::start().await;
    let token = access_token("account-456");
    mount_user_code(&server, "1").await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authorization_code": "oauth-code",
            "code_verifier": "device-code-verifier",
        })))
        .mount(&server)
        .await;
    mount_token_exchange(&server, &token).await;

    let (interaction, ctx) = TestInteraction::new()
        .with_select("device_code")
        .into_context();
    flow(&server).login(&ctx).await.unwrap();

    let recorded = interaction.recorded();
    assert_eq!(
        recorded.prompts,
        vec!["select: Select OpenAI Codex login method:".to_string()]
    );
    // Choosing device-code must not start the browser flow.
    assert!(recorded.auth_urls.is_empty());
}

#[tokio::test]
async fn cancels_when_login_method_selection_is_cancelled() {
    let server = MockServer::start().await;
    // A `TestInteraction` with no canned select answer refuses the prompt.
    let (_interaction, ctx) = TestInteraction::new().into_context();

    let error = flow(&server).login(&ctx).await.unwrap_err();
    assert_eq!(error.code(), "interaction");
}

#[tokio::test]
async fn treats_device_auth_403_and_404_responses_as_pending() {
    let server = MockServer::start().await;
    let token = access_token("account-403-404");
    mount_user_code(&server, "1").await;

    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_json(json!({ "error": "access_denied", "error_description": "denied" })),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(ResponseTemplate::new(404).set_body_string("not ready"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authorization_code": "oauth-code",
            "code_verifier": "device-code-verifier",
        })))
        .expect(1)
        .mount(&server)
        .await;
    mount_token_exchange(&server, &token).await;

    let (_interaction, ctx) = TestInteraction::new()
        .with_select("device_code")
        .into_context();
    let credential = flow(&server).login(&ctx).await.unwrap();

    assert_eq!(credential.extra_str("accountId"), Some("account-403-404"));
}

#[tokio::test]
async fn includes_the_response_body_in_device_auth_poll_failures() {
    let server = MockServer::start().await;
    mount_user_code(&server, "1").await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "error": "server_error",
            "error_description": "try again later",
        })))
        .mount(&server)
        .await;

    let (_interaction, ctx) = TestInteraction::new()
        .with_select("device_code")
        .into_context();
    let error = flow(&server).login(&ctx).await.unwrap_err();

    assert!(
        error
            .message()
            .contains("OpenAI Codex device auth failed with status 500"),
        "unexpected message: {}",
        error.message()
    );
    assert!(error.message().contains("try again later"));
}

#[tokio::test]
async fn a_device_code_endpoint_404_explains_that_the_flow_is_disabled() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let (_interaction, ctx) = TestInteraction::new()
        .with_select("device_code")
        .into_context();
    let error = flow(&server).login(&ctx).await.unwrap_err();

    assert!(error.message().contains("not enabled for this server"));
}

#[tokio::test]
async fn refresh_reports_a_rejected_token_without_writing_to_stderr() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {
                "message": "Could not validate your token. Please try signing in again.",
                "type": "invalid_request_error",
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let error = flow(&server)
        .refresh(
            &OAuthCredential::new("invalid-access-token", "invalid-refresh-token", 0),
            AbortSignal::never(),
        )
        .await
        .unwrap_err();

    assert!(error
        .message()
        .contains("OpenAI Codex token refresh failed (401)"));
    assert!(error.message().contains("Could not validate your token"));
}

#[tokio::test]
async fn refresh_returns_a_credential_carrying_the_account_id() {
    let server = MockServer::start().await;
    let token = access_token("account-789");
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=old-refresh"))
        .and(body_string_contains(format!("client_id={CLIENT_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": token,
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

    assert_eq!(credential.access, token);
    assert_eq!(credential.refresh, "new-refresh");
    assert_eq!(credential.extra_str("accountId"), Some("account-789"));
}

#[tokio::test]
async fn the_browser_flow_emits_an_authorization_url_and_exchanges_the_pasted_code() {
    let server = MockServer::start().await;
    let token = access_token("account-browser");
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=pasted-code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": token,
            "refresh_token": "refresh-token",
            "expires_in": 3600,
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (interaction, ctx) = TestInteraction::new()
        .with_select("browser")
        .with_manual_answer(|recorded| {
            let state = recorded.query_param("state").expect("state in auth url");
            Ok(format!(
                "http://localhost:1455/auth/callback?code=pasted-code&state={state}"
            ))
        })
        .into_context();

    let credential = flow(&server).login(&ctx).await.unwrap();
    assert_eq!(credential.extra_str("accountId"), Some("account-browser"));

    let recorded = interaction.recorded();
    assert_eq!(
        recorded.query_param("client_id").as_deref(),
        Some(CLIENT_ID)
    );
    assert_eq!(
        recorded.query_param("redirect_uri").as_deref(),
        Some("http://localhost:1455/auth/callback")
    );
    assert_eq!(recorded.query_param("originator").as_deref(), Some("pi"));
    assert_eq!(
        recorded.query_param("codex_cli_simplified_flow").as_deref(),
        Some("true")
    );
    let challenge = recorded.query_param("code_challenge").expect("challenge");
    assert!(!challenge.is_empty());
}

#[tokio::test]
async fn a_browser_state_mismatch_fails_the_login() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let (_interaction, ctx) = TestInteraction::new()
        .with_select("browser")
        .with_manual_answer(
            |_| Ok("http://localhost:1455/auth/callback?code=c&state=forged".into()),
        )
        .into_context();

    let error = flow(&server).login(&ctx).await.unwrap_err();
    assert!(error.message().contains("State mismatch"));
}

#[tokio::test]
async fn an_unknown_login_method_is_rejected() {
    let server = MockServer::start().await;
    let (_interaction, ctx) = TestInteraction::new()
        .with_select("carrier-pigeon")
        .into_context();

    let error = flow(&server).login(&ctx).await.unwrap_err();
    assert!(error
        .message()
        .contains("Unknown OpenAI Codex login method"));
}

/// Guards the `to_auth` contract the resolver depends on.
#[tokio::test]
async fn to_auth_derives_the_api_key_from_the_access_token() {
    let server = MockServer::start().await;
    let auth = flow(&server)
        .to_auth(&OAuthCredential::new("token", "r", 0))
        .await
        .unwrap();
    assert_eq!(auth.api_key.as_deref(), Some("token"));
    assert_eq!(auth.base_url, None);
}
