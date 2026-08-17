//! Port of `.upstream/packages/ai/test/openrouter-oauth.test.ts`.
//! `wiremock` serves every endpoint; nothing here reaches OpenRouter.
//!
//! The loopback-callback cases exercise the opt-in server, so they double as
//! the coverage for `LoginContext::with_local_callback_server`.

mod support;

use pi_auth::{OAuthCredential, OAuthFlow, OAuthHttp, OpenRouterOAuth};
use pi_core::options::AbortSignal;
use serde_json::json;
use support::TestInteraction;
use wiremock::matchers::{body_json_string, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn flow(server: &MockServer) -> OpenRouterOAuth {
    OpenRouterOAuth::new(OAuthHttp::default()).with_urls(
        "https://openrouter.ai/auth",
        format!("{}/api/v1/auth/keys", server.uri()),
    )
}

async fn mount_keys(server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/keys"))
        .respond_with(response)
        .mount(server)
        .await;
}

fn exchange_body(requests: &[Request]) -> serde_json::Value {
    serde_json::from_slice(&requests[0].body).expect("json body")
}

#[tokio::test]
async fn mints_a_permanent_key_from_a_pasted_redirect_url() {
    let server = MockServer::start().await;
    mount_keys(
        &server,
        ResponseTemplate::new(200).set_body_json(json!({ "key": "sk-or-minted" })),
    )
    .await;

    let (interaction, ctx) = TestInteraction::new()
        .with_manual_answer(|_| Ok("http://127.0.0.1:9999/oauth/callback?code=the-code".into()))
        .into_context();

    let credential = flow(&server).login(&ctx).await.unwrap();

    assert_eq!(credential.access, "sk-or-minted");
    // No refresh token, and an expiry that never triggers a refresh.
    assert_eq!(credential.refresh, "");
    assert_eq!(credential.expires, 9_007_199_254_740_991);

    let body = exchange_body(&server.received_requests().await.unwrap());
    assert_eq!(body["code"], "the-code");
    assert_eq!(body["code_challenge_method"], "S256");
    let verifier = body["code_verifier"].as_str().expect("verifier");
    let recorded = interaction.recorded();
    assert_eq!(
        recorded.query_param("code_challenge").as_deref(),
        Some(pi_auth::oauth::pkce_challenge(verifier).as_str())
    );
}

#[tokio::test]
async fn accepts_a_bare_authorization_code_from_the_manual_prompt() {
    let server = MockServer::start().await;
    mount_keys(
        &server,
        ResponseTemplate::new(200).set_body_json(json!({ "key": "sk-or-bare" })),
    )
    .await;

    let (_interaction, ctx) = TestInteraction::new()
        .with_manual_answer(|_| Ok("bare-code".into()))
        .into_context();

    let credential = flow(&server).login(&ctx).await.unwrap();
    assert_eq!(credential.access, "sk-or-bare");

    let body = exchange_body(&server.received_requests().await.unwrap());
    assert_eq!(body["code"], "bare-code");
}

#[tokio::test]
async fn rejects_empty_manual_input_without_exchanging_a_code() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/keys"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let (_interaction, ctx) = TestInteraction::new()
        .with_manual_answer(|_| Ok("".into()))
        .into_context();

    let error = flow(&server).login(&ctx).await.unwrap_err();
    assert!(error.message().contains("Missing authorization code"));
}

#[tokio::test]
async fn fails_login_when_the_manual_prompt_is_cancelled() {
    let server = MockServer::start().await;
    // No manual answer configured: the prompt is refused.
    let (_interaction, ctx) = TestInteraction::new().into_context();

    let error = flow(&server).login(&ctx).await.unwrap_err();
    assert_eq!(error.code(), "interaction");
}

#[tokio::test]
async fn reports_a_token_exchange_failure_with_the_server_detail() {
    let server = MockServer::start().await;
    mount_keys(
        &server,
        ResponseTemplate::new(400).set_body_json(json!({ "error": { "message": "bad code" } })),
    )
    .await;

    let (_interaction, ctx) = TestInteraction::new()
        .with_manual_answer(|_| Ok("the-code".into()))
        .into_context();

    let error = flow(&server).login(&ctx).await.unwrap_err();
    assert!(error
        .message()
        .contains("OpenRouter OAuth key exchange failed (HTTP 400): bad code"));
}

#[tokio::test]
async fn rejects_a_successful_response_that_does_not_contain_a_key() {
    let server = MockServer::start().await;
    mount_keys(
        &server,
        ResponseTemplate::new(200).set_body_json(json!({ "not_a_key": "value" })),
    )
    .await;

    let (_interaction, ctx) = TestInteraction::new()
        .with_manual_answer(|_| Ok("the-code".into()))
        .into_context();

    let error = flow(&server).login(&ctx).await.unwrap_err();
    assert!(error.message().contains("carries no \"key\""));
}

#[tokio::test]
async fn invalid_json_on_a_successful_response_is_reported() {
    let server = MockServer::start().await;
    mount_keys(
        &server,
        ResponseTemplate::new(200).set_body_string("<html>"),
    )
    .await;

    let (_interaction, ctx) = TestInteraction::new()
        .with_manual_answer(|_| Ok("the-code".into()))
        .into_context();

    let error = flow(&server).login(&ctx).await.unwrap_err();
    assert!(error.message().contains("invalid JSON"));
}

#[tokio::test]
async fn rejects_before_starting_when_login_is_already_cancelled() {
    let server = MockServer::start().await;
    let (handle, signal) = pi_core::options::AbortHandle::new();
    handle.abort();

    let (interaction, ctx) = TestInteraction::new()
        .with_manual_answer(|_| Ok("the-code".into()))
        .into_context();
    let ctx = ctx.with_signal(signal);

    let error = flow(&server).login(&ctx).await.unwrap_err();
    assert!(error.is_cancelled());
    assert!(interaction.recorded().auth_urls.is_empty());
}

/// With the loopback server enabled, a browser redirect wins the race against
/// the manual prompt and mints the key.
#[tokio::test]
async fn runs_pkce_on_a_one_shot_loopback_callback() {
    let server = MockServer::start().await;
    mount_keys(
        &server,
        ResponseTemplate::new(200).set_body_json(json!({ "key": "sk-or-callback" })),
    )
    .await;

    // The manual prompt never answers, so only the callback can finish login.
    let (interaction, ctx) = TestInteraction::new()
        .with_manual_never_answered()
        .into_context();
    let ctx = ctx.with_local_callback_server(true);

    let flow = flow(&server);
    let login = flow.login(&ctx);

    let drive_browser = async {
        // Wait for the flow to publish the authorize URL carrying callback_url.
        let callback_url = loop {
            if let Some(url) = interaction.recorded().query_param("callback_url") {
                break url;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        };
        let response = reqwest::get(format!("{callback_url}?code=browser-code"))
            .await
            .expect("callback request");
        assert_eq!(response.status().as_u16(), 200);
    };

    let (credential, ()) = tokio::join!(login, drive_browser);
    let credential = credential.unwrap();
    assert_eq!(credential.access, "sk-or-callback");

    let body = exchange_body(&server.received_requests().await.unwrap());
    assert_eq!(body["code"], "browser-code");
}

#[tokio::test]
async fn refresh_is_a_no_op_for_the_permanent_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/keys"))
        .and(body_json_string("{}"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let credential = OAuthCredential::new("sk-or-permanent", "", 9_007_199_254_740_991);
    let refreshed = flow(&server)
        .refresh(&credential, AbortSignal::never())
        .await
        .unwrap();

    assert_eq!(refreshed, credential);
}
