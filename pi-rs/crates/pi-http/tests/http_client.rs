//! HTTP-level tests for [`HttpClient`], against a local `wiremock` server.
//!
//! Everything here binds to `127.0.0.1` on an ephemeral port. No test reaches a
//! real provider endpoint.

use std::time::Duration;

use futures_util::StreamExt;
use pi_core::options::AbortHandle;
use pi_http::client::{HttpClient, JsonRequest};
use pi_http::retry::RetryPolicy;
use pi_http::HttpError;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn fast_policy(max_attempts: u32) -> RetryPolicy {
    RetryPolicy {
        max_attempts,
        initial_delay_ms: 1,
        max_delay_ms: 5,
        ..Default::default()
    }
}

fn sse_response(body: &'static str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_raw(body, "text/event-stream")
}

async fn collect_sse(response: pi_http::client::SseResponse) -> Vec<pi_http::SseEvent> {
    response
        .body
        .map(|event| event.expect("stream error"))
        .collect::<Vec<_>>()
        .await
}

#[tokio::test]
async fn posts_json_with_merged_headers_and_reads_the_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "secret"))
        .and(header("content-type", "application/json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "id": "msg_1", "ok": true })),
        )
        .mount(&server)
        .await;

    let client = HttpClient::default();
    let request = JsonRequest::post(
        format!("{}/v1/messages", server.uri()),
        json!({ "model": "m" }),
    )
    .header("x-api-key", "secret");
    let response = client.post_json(request).await.expect("request succeeds");

    assert_eq!(response.status, 200);
    let body: serde_json::Value = response.json().expect("json body");
    assert_eq!(body["id"], "msg_1");
}

#[tokio::test]
async fn a_provider_error_body_becomes_a_status_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "3")
                .set_body_json(
                    json!({ "error": { "type": "rate_limit_error", "message": "slow down" } }),
                ),
        )
        .mount(&server)
        .await;

    let client = HttpClient::default();
    let error = client
        .post_json(JsonRequest::post(server.uri(), json!({})))
        .await
        .expect_err("429 is an error");

    match error {
        HttpError::Status {
            status,
            message,
            body,
            retry_after_ms,
        } => {
            assert_eq!(status, 429);
            assert_eq!(message, "slow down");
            assert_eq!(retry_after_ms, Some(3_000));
            assert_eq!(body.unwrap()["error"]["type"], "rate_limit_error");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn an_html_gateway_error_still_produces_a_usable_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(502)
                .set_body_string("<!DOCTYPE html>\n<html><body>502</body></html>"),
        )
        .mount(&server)
        .await;

    let error = HttpClient::default()
        .post_json(JsonRequest::post(server.uri(), json!({})))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("502 status code (html error page)"),
        "{error}"
    );
}

#[tokio::test]
async fn reads_an_sse_stream_and_maps_every_event() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_response(concat!(
            "event: message_start\ndata: {\"type\":\"message_start\"}\n\n",
            ": ping\n\n",
            "event: content_block_delta\ndata: {\"delta\":{\"text\":\"héllo 🙈\"}}\n\n",
            "data: [DONE]\n\n",
        )))
        .mount(&server)
        .await;

    let response = HttpClient::default()
        .post_sse(JsonRequest::post(server.uri(), json!({ "stream": true })))
        .await
        .expect("stream opens");
    assert_eq!(response.status, 200);

    let events = collect_sse(response).await;
    assert_eq!(
        events.len(),
        3,
        "the keepalive comment must not become an event"
    );
    assert_eq!(events[0].event, "message_start");
    assert_eq!(events[1].event, "content_block_delta");
    assert!(events[1].data.contains("héllo 🙈"));
    assert!(events[2].is_done_sentinel());
    assert_eq!(
        events[1].raw,
        "event: content_block_delta\ndata: {\"delta\":{\"text\":\"héllo 🙈\"}}"
    );
}

#[tokio::test]
async fn an_sse_endpoint_returning_an_error_never_yields_a_stream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({ "error": { "message": "prompt is too long" } })),
        )
        .mount(&server)
        .await;

    let error = HttpClient::default()
        .post_sse(JsonRequest::post(server.uri(), json!({})))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("prompt is too long"), "{error}");
}

#[tokio::test]
async fn a_stream_that_ends_without_a_blank_line_still_delivers_its_last_event() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_response("data: {\"a\":1}\n\ndata: {\"b\":2}\n"))
        .mount(&server)
        .await;

    let response = HttpClient::default()
        .post_sse(JsonRequest::post(server.uri(), json!({})))
        .await
        .unwrap();
    let events = collect_sse(response).await;
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].data, "{\"b\":2}");
}

#[tokio::test]
async fn retries_a_500_and_then_succeeds() {
    let server = MockServer::start().await;
    // wiremock matches the most recently mounted first when `up_to_n_times`
    // is exhausted, so mount the failures then the success.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("upstream exploded"))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .mount(&server)
        .await;

    let response = HttpClient::default()
        .post_json_retrying(JsonRequest::post(server.uri(), json!({})), &fast_policy(3))
        .await
        .expect("third attempt succeeds");
    assert_eq!(response.status, 200);
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn does_not_retry_a_400() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({ "error": { "message": "bad model" } })),
        )
        .mount(&server)
        .await;

    let error = HttpClient::default()
        .post_json_retrying(JsonRequest::post(server.uri(), json!({})), &fast_policy(3))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("bad model"), "{error}");
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "must not retry"
    );
}

#[tokio::test]
async fn honours_the_x_should_retry_header_over_the_status() {
    let server = MockServer::start().await;
    // 429 would normally be retried, but the provider says not to.
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("x-should-retry", "false")
                .set_body_json(json!({ "error": { "message": "quota exhausted" } })),
        )
        .mount(&server)
        .await;

    let error = HttpClient::default()
        .post_json_retrying(JsonRequest::post(server.uri(), json!({})), &fast_policy(3))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("quota exhausted"), "{error}");
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn retries_a_400_when_the_provider_asks_for_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(400)
                .insert_header("x-should-retry", "true")
                .set_body_string("transient validation glitch"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .mount(&server)
        .await;

    HttpClient::default()
        .post_json_retrying(JsonRequest::post(server.uri(), json!({})), &fast_policy(3))
        .await
        .expect("retried on the provider's instruction");
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_server_delay_above_the_cap_fails_without_waiting() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                // Nearly four days: parking the request would hang the caller.
                .insert_header("retry-after", "277403")
                .set_body_json(json!({ "error": { "message": "slow down" } })),
        )
        .mount(&server)
        .await;

    let policy = RetryPolicy {
        max_attempts: 3,
        max_server_delay_ms: 1_000,
        ..fast_policy(3)
    };
    let started = std::time::Instant::now();
    let error = HttpClient::default()
        .post_json_retrying(JsonRequest::post(server.uri(), json!({})), &policy)
        .await
        .unwrap_err();

    assert!(
        started.elapsed() < Duration::from_secs(5),
        "must not wait out the delay"
    );
    assert!(error.to_string().contains("slow down"), "{error}");
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn the_sse_connection_is_retried_but_the_stream_is_not() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503).set_body_string("overloaded"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_response("data: {\"a\":1}\n\n"))
        .mount(&server)
        .await;

    let response = HttpClient::default()
        .post_sse_retrying(JsonRequest::post(server.uri(), json!({})), &fast_policy(3))
        .await
        .expect("second attempt opens the stream");
    let events = collect_sse(response).await;
    assert_eq!(events.len(), 1);
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn an_abort_before_the_request_prevents_it_entirely() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;

    let (handle, signal) = AbortHandle::new();
    handle.abort();
    let error = HttpClient::default()
        .post_json(JsonRequest::post(server.uri(), json!({})).signal(Some(signal)))
        .await
        .unwrap_err();

    assert!(matches!(error, HttpError::Aborted));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn an_abort_mid_flight_cancels_the_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(30))
                .set_body_json(json!({})),
        )
        .mount(&server)
        .await;

    let (handle, signal) = AbortHandle::new();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        handle.abort();
    });

    let started = std::time::Instant::now();
    let error = HttpClient::default()
        .post_json(JsonRequest::post(server.uri(), json!({})).signal(Some(signal)))
        .await
        .unwrap_err();

    assert!(matches!(error, HttpError::Aborted));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "should not wait for the server"
    );
}

#[tokio::test]
async fn a_per_request_timeout_reports_the_configured_deadline() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(10))
                .set_body_json(json!({})),
        )
        .mount(&server)
        .await;

    let error = HttpClient::default()
        .post_json(JsonRequest::post(server.uri(), json!({})).timeout_ms(Some(150)))
        .await
        .unwrap_err();

    // The deadline must survive into the error: `Timeout(0)` is not actionable.
    assert!(matches!(error, HttpError::Timeout(150)), "{error:?}");
}

#[tokio::test]
async fn header_overrides_replace_and_remove_provider_defaults() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(|request: &Request| {
            let names: Vec<String> = request
                .headers
                .iter()
                .map(|(name, _)| name.to_string())
                .collect();
            let anthropic_version = request
                .headers
                .get("anthropic-version")
                .map(|v| v.to_str().unwrap_or_default().to_string());
            ResponseTemplate::new(200).set_body_json(json!({
                "names": names,
                "anthropicVersion": anthropic_version,
            }))
        })
        .mount(&server)
        .await;

    let overrides: pi_core::options::ProviderHeaders = [
        (
            "Anthropic-Version".to_string(),
            Some("2099-01-01".to_string()),
        ),
        ("x-api-key".to_string(), None),
    ]
    .into_iter()
    .collect();

    let request = JsonRequest::post(server.uri(), json!({}))
        .header("anthropic-version", "2023-06-01")
        .header("x-api-key", "secret")
        .with_overrides(&overrides);
    let response = HttpClient::default().post_json(request).await.unwrap();
    let body: serde_json::Value = response.json().unwrap();

    assert_eq!(body["anthropicVersion"], "2099-01-01");
    let names: Vec<String> = serde_json::from_value(body["names"].clone()).unwrap();
    assert!(
        !names.iter().any(|n| n == "x-api-key"),
        "a null override must remove the header: {names:?}"
    );
}

#[tokio::test]
async fn no_proxy_lets_a_scoped_client_reach_the_target_directly() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .mount(&server)
        .await;

    // A proxy that does not exist, excluded for this host by NO_PROXY. If the
    // exclusion were ignored the request would fail to connect.
    let env: pi_core::options::ProviderEnv = [
        ("HTTP_PROXY".to_string(), "http://127.0.0.1:1/".to_string()),
        ("NO_PROXY".to_string(), "127.0.0.1".to_string()),
    ]
    .into_iter()
    .collect();

    let client = HttpClient::shared_for_env(&env).expect("client builds");
    let response = client
        .post_json(JsonRequest::post(server.uri(), json!({})))
        .await
        .expect("NO_PROXY should bypass the dead proxy");
    assert_eq!(response.status, 200);
}

#[tokio::test]
async fn a_scoped_proxy_is_actually_used() {
    // The "proxy" is a mock server: an absolute-form request arriving here
    // proves the client routed through it rather than connecting directly.
    let proxy = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "viaProxy": true })))
        .mount(&proxy)
        .await;

    let env: pi_core::options::ProviderEnv = [("HTTP_PROXY".to_string(), proxy.uri())]
        .into_iter()
        .collect();
    let client = HttpClient::shared_for_env(&env).expect("client builds");

    let response = client
        .post_json(JsonRequest::post(
            "http://provider.invalid/v1/messages",
            json!({}),
        ))
        .await
        .expect("routed through the proxy");
    let body: serde_json::Value = response.json().unwrap();
    assert_eq!(body["viaProxy"], true);

    let received = proxy.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].url.path(), "/v1/messages");
}

#[tokio::test]
async fn an_unsupported_proxy_scheme_is_rejected_rather_than_bypassed() {
    let env: pi_core::options::ProviderEnv = [(
        "HTTPS_PROXY".to_string(),
        "socks5://127.0.0.1:1080".to_string(),
    )]
    .into_iter()
    .collect();
    let error = HttpClient::shared_for_env(&env).expect_err("SOCKS is unsupported");
    assert!(
        error
            .to_string()
            .contains(pi_http::UNSUPPORTED_PROXY_PROTOCOL_MESSAGE),
        "{error}"
    );
}

#[tokio::test]
async fn a_connection_failure_is_a_transport_error() {
    // Port 1 on loopback: nothing listens, so the connection is refused.
    let error = HttpClient::default()
        .post_json(JsonRequest::post(
            "http://127.0.0.1:1/v1/messages",
            json!({}),
        ))
        .await
        .unwrap_err();
    assert!(matches!(error, HttpError::Transport(_)), "{error:?}");
    // The message-level classifier must recognise it so a turn can be retried.
    assert!(
        pi_http::is_retryable_error_message(&error.to_string()),
        "{error}"
    );
}
