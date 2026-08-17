//! Port of `.upstream/packages/ai/test/github-copilot-oauth.test.ts`.
//! `wiremock` serves every endpoint; nothing here reaches GitHub.

mod support;

use pi_auth::{GitHubCopilotOAuth, OAuthCredential, OAuthFlow, OAuthHttp};
use pi_core::options::AbortSignal;
use serde_json::json;
use support::TestInteraction;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";

/// One base URL stands in for `github.com`, `api.github.com` and the Copilot
/// proxy endpoint the token would otherwise name.
fn flow(server: &MockServer) -> GitHubCopilotOAuth {
    GitHubCopilotOAuth::new(OAuthHttp::default()).with_endpoint_override(server.uri())
}

async fn mount_device_code(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/login/device/code"))
        .and(header("user-agent", "GitHubCopilotChat/0.35.0"))
        .and(body_string_contains(format!("client_id={CLIENT_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "device-code-123",
            "user_code": "ABCD-1234",
            "verification_uri": "https://github.com/login/device",
            // Clamped to the poller's one-second floor.
            "interval": 1,
            "expires_in": 900,
        })))
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_copilot_token(server: &MockServer, token: &str) {
    Mock::given(method("GET"))
        .and(path("/copilot_internal/v2/token"))
        .and(header("copilot-integration-id", "vscode-chat"))
        .and(header("editor-version", "vscode/1.107.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token": token,
            "expires_at": 9_999_999_999i64,
        })))
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_models(server: &MockServer, body: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("x-github-api-version", "2026-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn reports_device_code_details_to_the_host_and_completes_the_login() {
    let server = MockServer::start().await;
    mount_device_code(&server).await;

    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .and(body_string_contains("device_code=device-code-123"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "error": "authorization_pending" })),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "gh-token",
            "token_type": "bearer",
        })))
        .expect(1)
        .mount(&server)
        .await;
    mount_copilot_token(&server, "copilot-token").await;
    mount_models(
        &server,
        json!({ "data": [{ "id": "gpt-5", "model_picker_enabled": true }] }),
    )
    .await;

    // Blank input means github.com rather than an enterprise domain.
    let (interaction, ctx) = TestInteraction::new().with_text("").into_context();
    let credential = flow(&server).login(&ctx).await.unwrap();

    assert_eq!(credential.access, "copilot-token");
    assert_eq!(credential.refresh, "gh-token");
    assert_eq!(credential.extra_str("enterpriseUrl"), None);
    assert_eq!(
        credential.extra.get("availableModelIds"),
        Some(&json!(["gpt-5"]))
    );

    let recorded = interaction.recorded();
    assert_eq!(recorded.device_codes.len(), 1);
    assert_eq!(recorded.device_codes[0].user_code, "ABCD-1234");
    assert_eq!(
        recorded.device_codes[0].verification_uri,
        "https://github.com/login/device"
    );
    assert_eq!(recorded.device_codes[0].interval_seconds, Some(1));
    assert_eq!(recorded.device_codes[0].expires_in_seconds, Some(900));
}

#[tokio::test]
async fn rejects_a_non_http_verification_uri_before_it_reaches_the_host() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/login/device/code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "d",
            "user_code": "u",
            "verification_uri": "file:///etc/passwd",
            "expires_in": 900,
        })))
        .mount(&server)
        .await;

    let (interaction, ctx) = TestInteraction::new().with_text("").into_context();
    let error = flow(&server).login(&ctx).await.unwrap_err();

    assert!(error.message().contains("Untrusted verification_uri"));
    assert!(interaction.recorded().device_codes.is_empty());
}

#[tokio::test]
async fn an_invalid_enterprise_domain_is_rejected_before_any_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/login/device/code"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let (_interaction, ctx) = TestInteraction::new()
        .with_text("not a domain")
        .into_context();
    let error = flow(&server).login(&ctx).await.unwrap_err();

    assert!(error
        .message()
        .contains("Invalid GitHub Enterprise URL/domain"));
}

#[tokio::test]
async fn a_device_flow_failure_surfaces_its_description() {
    let server = MockServer::start().await;
    mount_device_code(&server).await;
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": "access_denied",
            "error_description": "the user denied the request",
        })))
        .mount(&server)
        .await;

    let (_interaction, ctx) = TestInteraction::new().with_text("").into_context();
    let error = flow(&server).login(&ctx).await.unwrap_err();

    assert!(error
        .message()
        .contains("Device flow failed: access_denied"));
    assert!(error.message().contains("the user denied the request"));
}

#[tokio::test]
async fn refresh_exchanges_the_github_token_and_preserves_the_enterprise_domain() {
    let server = MockServer::start().await;
    mount_copilot_token(&server, "new-token").await;
    mount_models(&server, json!({ "data": [] })).await;

    let refreshed = flow(&server)
        .refresh(
            &OAuthCredential::new("old", "gh-token", 0)
                .with_extra("enterpriseUrl", json!("company.ghe.com")),
            AbortSignal::never(),
        )
        .await
        .unwrap();

    assert_eq!(refreshed.access, "new-token");
    assert_eq!(refreshed.refresh, "gh-token");
    assert_eq!(
        refreshed.extra_str("enterpriseUrl"),
        Some("company.ghe.com")
    );
    // `expires_at` is seconds; the credential stores milliseconds less the skew.
    assert_eq!(refreshed.expires, 9_999_999_999_000 - 5 * 60 * 1000);
}

#[tokio::test]
async fn filters_models_to_the_authenticated_account_picker_catalog() {
    let server = MockServer::start().await;
    mount_copilot_token(&server, "copilot-token").await;
    mount_models(
        &server,
        json!({
            "data": [
                { "id": "picked", "model_picker_enabled": true },
                { "id": "not-picked", "model_picker_enabled": false, "policy": { "state": "enabled" } },
                { "id": "no-tools", "model_picker_enabled": true,
                  "capabilities": { "supports": { "tool_calls": false } } },
            ]
        }),
    )
    .await;

    let refreshed = flow(&server)
        .refresh(
            &OAuthCredential::new("old", "gh-token", 0),
            AbortSignal::never(),
        )
        .await
        .unwrap();

    assert_eq!(
        refreshed.extra.get("availableModelIds"),
        Some(&json!(["picked"]))
    );
}

#[tokio::test]
async fn retries_the_models_request_once_after_a_429() {
    let server = MockServer::start().await;
    mount_copilot_token(&server, "copilot-token").await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({ "data": [{ "id": "gpt-5", "model_picker_enabled": true }] }),
            ),
        )
        .expect(1)
        .mount(&server)
        .await;

    let refreshed = flow(&server)
        .refresh(
            &OAuthCredential::new("old", "gh-token", 0),
            AbortSignal::never(),
        )
        .await
        .unwrap();

    assert_eq!(
        refreshed.extra.get("availableModelIds"),
        Some(&json!(["gpt-5"]))
    );
}

#[tokio::test]
async fn a_rejected_copilot_token_exchange_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/copilot_internal/v2/token"))
        .respond_with(ResponseTemplate::new(401).set_body_string("Bad credentials"))
        .mount(&server)
        .await;

    let error = flow(&server)
        .refresh(
            &OAuthCredential::new("old", "dead-token", 0),
            AbortSignal::never(),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code(), "oauth");
    assert!(error.message().contains("401"));
}

#[tokio::test]
async fn model_policies_are_accepted_during_login_when_the_host_supplies_ids() {
    let server = MockServer::start().await;
    mount_device_code(&server).await;
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "access_token": "gh-token" })),
        )
        .mount(&server)
        .await;
    mount_copilot_token(&server, "copilot-token").await;
    for model_id in ["claude-sonnet-4", "grok-code"] {
        Mock::given(method("POST"))
            .and(path(format!("/models/{model_id}/policy")))
            .and(header("openai-intent", "chat-policy"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "state": "enabled" })))
            .expect(1)
            .mount(&server)
            .await;
    }
    mount_models(&server, json!({ "data": [] })).await;

    let (_interaction, ctx) = TestInteraction::new().with_text("").into_context();
    flow(&server)
        .with_policy_model_ids(["claude-sonnet-4", "grok-code"])
        .login(&ctx)
        .await
        .unwrap();
    // The per-model `expect(1)` mocks assert the sequential policy calls.
}

#[tokio::test]
async fn a_failing_policy_call_does_not_fail_the_login() {
    let server = MockServer::start().await;
    mount_device_code(&server).await;
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "access_token": "gh-token" })),
        )
        .mount(&server)
        .await;
    mount_copilot_token(&server, "copilot-token").await;
    Mock::given(method("POST"))
        .and(path("/models/claude-sonnet-4/policy"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    mount_models(&server, json!({ "data": [] })).await;

    let (_interaction, ctx) = TestInteraction::new().with_text("").into_context();
    let credential = flow(&server)
        .with_policy_model_ids(["claude-sonnet-4"])
        .login(&ctx)
        .await
        .unwrap();

    assert_eq!(credential.access, "copilot-token");
}
