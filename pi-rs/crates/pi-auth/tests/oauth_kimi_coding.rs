//! Port of `.upstream/packages/ai/test/kimi-coding-oauth.test.ts`.
//! `wiremock` serves every endpoint; nothing here reaches Kimi.

mod support;

use pi_auth::{KimiCodingOAuth, OAuthCredential, OAuthFlow, OAuthHttp};
use pi_core::message::now_ms;
use pi_core::options::{AbortHandle, AbortSignal};
use serde_json::json;
use support::TestInteraction;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";

fn flow(server: &MockServer) -> KimiCodingOAuth {
    KimiCodingOAuth::new(OAuthHttp::default()).with_oauth_host(server.uri())
}

fn device_authorization_body() -> serde_json::Value {
    json!({
        "user_code": "ABCD-1234",
        "device_code": "device-code-123",
        "verification_uri": "https://www.kimi.com/code",
        "verification_uri_complete": "https://www.kimi.com/code?user_code=ABCD-1234",
        // The poller clamps to a one-second floor, which keeps the test quick.
        "interval": 1,
        "expires_in": 600,
    })
}

async fn mount_device_authorization(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/oauth/device_authorization"))
        .and(header("content-type", "application/x-www-form-urlencoded"))
        .and(header("accept", "application/json"))
        .and(body_string_contains(format!("client_id={CLIENT_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(device_authorization_body()))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn logs_in_with_the_device_authorization_flow() {
    let server = MockServer::start().await;
    mount_device_authorization(&server).await;

    // First poll pending, second one succeeds.
    Mock::given(method("POST"))
        .and(path("/api/oauth/token"))
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
        .and(path("/api/oauth/token"))
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
    assert!(credential.expires >= now_ms() + 3_600_000 - 2_000);

    // The host is shown the completed verification URI, not the bare one.
    let recorded = interaction.recorded();
    assert_eq!(recorded.device_codes.len(), 1);
    let device_code = &recorded.device_codes[0];
    assert_eq!(device_code.user_code, "ABCD-1234");
    assert_eq!(
        device_code.verification_uri,
        "https://www.kimi.com/code?user_code=ABCD-1234"
    );
    assert_eq!(device_code.interval_seconds, Some(1));
    assert_eq!(device_code.expires_in_seconds, Some(600));
    assert!(recorded.prompts.is_empty(), "Kimi login must not prompt");
}

#[tokio::test]
async fn fails_when_the_device_code_expires() {
    let server = MockServer::start().await;
    mount_device_authorization(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "error": "expired_token" })))
        .mount(&server)
        .await;

    let (_interaction, ctx) = TestInteraction::new().into_context();
    let error = flow(&server).login(&ctx).await.unwrap_err();

    assert_eq!(error.code(), "oauth");
    assert!(error.message().contains("expired"));
}

#[tokio::test]
async fn fails_when_the_user_denies_the_login() {
    let server = MockServer::start().await;
    mount_device_authorization(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "error": "access_denied" })))
        .mount(&server)
        .await;

    let (_interaction, ctx) = TestInteraction::new().into_context();
    let error = flow(&server).login(&ctx).await.unwrap_err();

    assert!(error.message().contains("denied"));
}

#[tokio::test]
async fn a_cancelled_login_stops_polling() {
    let server = MockServer::start().await;
    mount_device_authorization(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/oauth/token"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(json!({ "error": "authorization_pending" })),
        )
        .mount(&server)
        .await;

    let (handle, signal) = AbortHandle::new();
    let (_interaction, ctx) = TestInteraction::new().into_context();
    let ctx = ctx.with_signal(signal);

    let kimi = flow(&server);
    let login = kimi.login(&ctx);
    let abort = async {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        handle.abort();
    };
    let (result, ()) = tokio::join!(login, abort);

    assert!(result.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn refreshes_tokens_and_returns_a_bearer_header_for_requests() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=old-refresh"))
        .and(body_string_contains(format!("client_id={CLIENT_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "new-access",
            "refresh_token": "new-refresh",
            "expires_in": 3600,
        })))
        .expect(1)
        .mount(&server)
        .await;

    let before = now_ms();
    let flow = flow(&server);
    let credential = flow
        .refresh(
            &OAuthCredential::new("old-access", "old-refresh", before),
            AbortSignal::never(),
        )
        .await
        .unwrap();

    assert_eq!(credential.access, "new-access");
    assert_eq!(credential.refresh, "new-refresh");
    assert!(credential.expires >= before + 3_600_000 - 1_000);

    let auth = flow.to_auth(&credential).await.unwrap();
    assert_eq!(
        auth.headers.get("Authorization").and_then(Option::as_deref),
        Some("Bearer new-access")
    );
}

#[tokio::test]
async fn retries_a_refresh_after_a_429() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/oauth/token"))
        .respond_with(
            ResponseTemplate::new(429).set_body_json(json!({ "error": "temporarily_unavailable" })),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "a", "refresh_token": "r", "expires_in": 60,
        })))
        .expect(1)
        .mount(&server)
        .await;

    let credential = flow(&server)
        .refresh(&OAuthCredential::new("old", "old", 0), AbortSignal::never())
        .await
        .unwrap();

    assert_eq!(credential.access, "a");
}

#[tokio::test]
async fn does_not_retry_an_invalid_grant() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "error": "invalid_grant" })))
        // A dead credential must fail on the first answer, not after retries.
        .expect(1)
        .mount(&server)
        .await;

    let error = flow(&server)
        .refresh(&OAuthCredential::new("old", "old", 0), AbortSignal::never())
        .await
        .unwrap_err();

    assert!(error.message().contains("unauthorized"));
}

#[tokio::test]
async fn honors_the_kimi_oauth_host_override() {
    // The pinned host stands in for KIMI_CODE_OAUTH_HOST, which the flow reads
    // from the process env; pinning keeps the test free of global env mutation.
    let server = MockServer::start().await;
    let flow =
        KimiCodingOAuth::new(OAuthHttp::default()).with_oauth_host(format!("{}/", server.uri()));

    Mock::given(method("POST"))
        .and(path("/api/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "a", "refresh_token": "r", "expires_in": 60,
        })))
        .expect(1)
        .mount(&server)
        .await;

    flow.refresh(&OAuthCredential::new("old", "old", 0), AbortSignal::never())
        .await
        .unwrap();
}
