//! Port of `.upstream/packages/ai/test/xai-oauth.test.ts`.
//! `wiremock` serves every endpoint; nothing here reaches xAI.

mod support;

use pi_auth::{OAuthCredential, OAuthFlow, OAuthHttp, XaiOAuth};
use pi_core::message::now_ms;
use pi_core::options::{AbortHandle, AbortSignal};
use serde_json::json;
use support::TestInteraction;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const REFRESH_SKEW_MS: i64 = 5 * 60 * 1000;

fn flow(server: &MockServer) -> XaiOAuth {
    XaiOAuth::new(OAuthHttp::default()).with_urls(
        format!("{}/oauth2/device/code", server.uri()),
        format!("{}/oauth2/token", server.uri()),
    )
}

async fn mount_device_code(server: &MockServer, body: serde_json::Value) {
    Mock::given(method("POST"))
        .and(path("/oauth2/device/code"))
        .and(header("accept", "application/json"))
        .and(body_string_contains(format!("client_id={CLIENT_ID}")))
        .and(body_string_contains("referrer=pi"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(server)
        .await;
}

fn device_code_body() -> serde_json::Value {
    json!({
        "device_code": "device-code-123",
        "user_code": "ABCD-1234",
        "verification_uri": "https://auth.x.ai/device",
        // Clamped to the poller's one-second floor, keeping the test quick.
        "interval": 1,
        "expires_in": 600,
    })
}

#[tokio::test]
async fn uses_the_device_grant_and_handles_pending_then_success() {
    let server = MockServer::start().await;
    mount_device_code(&server, device_code_body()).await;

    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
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
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access-token",
            "refresh_token": "refresh-token",
            "expires_in": 3600,
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (interaction, ctx) = TestInteraction::new().into_context();
    let credential = flow(&server).login(&ctx).await.unwrap();

    assert_eq!(credential.access, "access-token");
    assert_eq!(credential.refresh, "refresh-token");
    let expected = now_ms() + 3_600_000 - REFRESH_SKEW_MS;
    assert!((credential.expires - expected).abs() < 3_000);

    let recorded = interaction.recorded();
    assert_eq!(recorded.device_codes.len(), 1);
    assert_eq!(recorded.device_codes[0].user_code, "ABCD-1234");
    assert_eq!(
        recorded.device_codes[0].verification_uri,
        "https://auth.x.ai/device"
    );
}

#[tokio::test]
async fn handles_a_slow_down_response() {
    let server = MockServer::start().await;
    mount_device_code(&server, device_code_body()).await;

    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({ "error": "slow_down", "interval": 1 })),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "a", "refresh_token": "r", "expires_in": 3600,
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (_interaction, ctx) = TestInteraction::new().into_context();
    assert_eq!(flow(&server).login(&ctx).await.unwrap().access, "a");
}

#[tokio::test]
async fn prefers_verification_uri_complete_when_the_server_provides_it() {
    let server = MockServer::start().await;
    mount_device_code(
        &server,
        json!({
            "device_code": "d",
            "user_code": "ABCD-1234",
            "verification_uri": "https://auth.x.ai/device",
            "verification_uri_complete": "https://auth.x.ai/device?code=ABCD-1234",
            "interval": 1,
            "expires_in": 600,
        }),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "a", "refresh_token": "r", "expires_in": 3600,
        })))
        .mount(&server)
        .await;

    let (interaction, ctx) = TestInteraction::new().into_context();
    flow(&server).login(&ctx).await.unwrap();

    assert_eq!(
        interaction.recorded().device_codes[0].verification_uri,
        "https://auth.x.ai/device?code=ABCD-1234"
    );
}

#[tokio::test]
async fn rejects_a_non_https_verification_uri_before_it_reaches_the_host() {
    let server = MockServer::start().await;
    mount_device_code(
        &server,
        json!({
            "device_code": "d",
            "user_code": "u",
            "verification_uri": "https://auth.x.ai/device",
            "verification_uri_complete": "http://auth.x.ai/device",
            "interval": 1,
            "expires_in": 600,
        }),
    )
    .await;

    let (interaction, ctx) = TestInteraction::new().into_context();
    let error = flow(&server).login(&ctx).await.unwrap_err();

    assert!(error.message().contains("Untrusted verification URI"));
    assert!(
        interaction.recorded().device_codes.is_empty(),
        "an untrusted URI must never reach the host"
    );
}

#[tokio::test]
async fn a_denied_authorization_fails_the_login() {
    let server = MockServer::start().await;
    mount_device_code(&server, device_code_body()).await;
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "error": "access_denied" })))
        .mount(&server)
        .await;

    let (_interaction, ctx) = TestInteraction::new().into_context();
    let error = flow(&server).login(&ctx).await.unwrap_err();
    assert!(error.message().contains("denied"));
}

#[tokio::test]
async fn cancels_while_waiting_for_the_first_token_poll() {
    let server = MockServer::start().await;
    mount_device_code(&server, device_code_body()).await;
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(json!({ "error": "authorization_pending" })),
        )
        .mount(&server)
        .await;

    let (handle, signal) = AbortHandle::new();
    let (_interaction, ctx) = TestInteraction::new().into_context();
    let ctx = ctx.with_signal(signal);
    let xai = flow(&server);

    let login = xai.login(&ctx);
    let abort = async {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        handle.abort();
    };
    let (result, ()) = tokio::join!(login, abort);

    assert!(result.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn refreshes_tokens_and_preserves_an_unrotated_refresh_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=old-refresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "new-access",
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
    assert_eq!(credential.refresh, "old-refresh");
}

#[tokio::test]
async fn surfaces_the_upstream_error_code_and_description_on_refresh_failure() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "refresh token expired",
        })))
        .expect(1)
        .mount(&server)
        .await;

    let error = flow(&server)
        .refresh(
            &OAuthCredential::new("old", "old-refresh", 0),
            AbortSignal::never(),
        )
        .await
        .unwrap_err();

    assert_eq!(
        error.message(),
        "oauth error: xAI OAuth token refresh failed (HTTP 400): invalid_grant: refresh token expired"
    );
}

#[tokio::test]
async fn invalid_json_from_the_token_endpoint_is_reported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>nope</html>"))
        .mount(&server)
        .await;

    let error = flow(&server)
        .refresh(&OAuthCredential::new("a", "r", 0), AbortSignal::never())
        .await
        .unwrap_err();

    assert!(error.message().contains("invalid JSON"));
}
