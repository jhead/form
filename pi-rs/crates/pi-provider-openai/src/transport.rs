//! Shared HTTP plumbing for the four adapters.
//!
//! Everything network-facing goes through `pi-http`: this module only owns
//! header composition, the retry wrapper, and the abort-aware SSE pump.

use std::collections::BTreeMap;

use futures_util::StreamExt;
use pi_core::options::{AbortSignal, ProviderHeaders, ProviderResponse, RequestOptions};
use pi_core::{AiError, Model};
use pi_http::client::{JsonRequest, OwnedHeaders};
use pi_http::{merge_headers, retry_with_backoff, HttpClient, RetryPolicy, SseEvent, SseResponse};
use serde_json::Value;

/// Case-insensitive check for a non-blank header value.
pub fn has_header(headers: &ProviderHeaders, name: &str) -> bool {
    headers.iter().any(|(key, value)| {
        key.eq_ignore_ascii_case(name) && value.as_ref().is_some_and(|v| !v.trim().is_empty())
    })
}

/// Port of `getClientApiKey`.
///
/// A caller-supplied `authorization` (or Cloudflare gateway) header stands in
/// for the key, because the SDK requires *something* to construct the client.
pub fn client_api_key(
    provider: &str,
    api_key: Option<&str>,
    headers: &ProviderHeaders,
) -> Result<String, AiError> {
    if let Some(key) = api_key.filter(|k| !k.is_empty()) {
        return Ok(key.to_string());
    }
    if has_header(headers, "authorization") || has_header(headers, "cf-aig-authorization") {
        return Ok("unused".to_string());
    }
    Err(AiError::auth(format!(
        "No API key for provider: {provider}"
    )))
}

/// Start from `model.headers`, which every adapter layers its own defaults onto.
pub fn model_headers(model: &Model) -> ProviderHeaders {
    model
        .headers
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|(k, v)| (k, Some(v)))
        .collect()
}

/// `Object.assign(headers, more)` with case-insensitive replacement.
pub fn assign_header(headers: &mut ProviderHeaders, name: &str, value: Option<String>) {
    let existing: Vec<String> = headers
        .keys()
        .filter(|k| k.eq_ignore_ascii_case(name))
        .cloned()
        .collect();
    for key in existing {
        headers.remove(&key);
    }
    headers.insert(name.to_string(), value);
}

/// Merge caller header overrides last so they win over adapter defaults.
pub fn assign_all(headers: &mut ProviderHeaders, overrides: &ProviderHeaders) {
    for (name, value) in overrides {
        assign_header(headers, name, value.clone());
    }
}

/// Compose the wire headers: adapter defaults, then everything in `overrides`
/// (`None` deletes, matching upstream's `null`).
pub fn finalize_headers(defaults: OwnedHeaders, overrides: &ProviderHeaders) -> OwnedHeaders {
    merge_headers(defaults, overrides)
}

/// The base header set every OpenAI-shaped JSON+SSE request needs.
pub fn base_json_sse_headers(api_key: &str) -> OwnedHeaders {
    let mut headers: OwnedHeaders = BTreeMap::new();
    headers.insert("authorization".into(), format!("Bearer {api_key}"));
    headers.insert("content-type".into(), "application/json".into());
    headers.insert("accept".into(), "text/event-stream".into());
    headers
}

fn retry_policy(options: &RequestOptions) -> RetryPolicy {
    let mut policy = RetryPolicy::default();
    // Upstream defaults `maxRetries` to the provider-retry helper's default of 2
    // extra attempts; `RetryPolicy::max_attempts` counts the first try too.
    if let Some(max_retries) = options.max_retries {
        policy.max_attempts = max_retries.saturating_add(1).max(1);
    }
    if let Some(max_delay) = options.max_retry_delay_ms {
        policy.max_server_delay_ms = max_delay;
    }
    policy
}

/// POST a JSON body and open the SSE response, with retry and the `onResponse`
/// hook. Errors come back as [`AiError`]; adapters encode them in the stream.
pub async fn post_sse_with_retry(
    http: &HttpClient,
    request: JsonRequest,
    model: &Model,
    options: &RequestOptions,
) -> Result<SseResponse, AiError> {
    let policy = retry_policy(options);
    let signal = options.signal.clone();
    let response = retry_with_backoff(&policy, signal.as_ref(), |_attempt| {
        let request = request.clone();
        async move { http.post_sse(request).await }
    })
    .await
    .map_err(AiError::from)?;

    if let Some(on_response) = &options.on_response {
        on_response(
            &ProviderResponse {
                status: response.status,
                headers: response.headers.clone(),
            },
            model,
        );
    }
    Ok(response)
}

/// Apply the `onPayload` hook, which may replace the body wholesale.
pub fn apply_on_payload(body: Value, model: &Model, options: &RequestOptions) -> Value {
    match &options.on_payload {
        Some(hook) => hook(&body, model).unwrap_or(body),
        None => body,
    }
}

/// One item from the abort-aware SSE pump.
pub enum SsePump {
    Event(SseEvent),
    Aborted,
    Failed(AiError),
    Done,
}

/// Read the next SSE event, racing the abort signal.
pub async fn next_sse(response: &mut SseResponse, signal: &Option<AbortSignal>) -> SsePump {
    match signal {
        Some(signal) if !signal.is_aborted() => {
            tokio::select! {
                biased;
                _ = signal.aborted() => SsePump::Aborted,
                item = response.body.next() => classify(item),
            }
        }
        Some(_) => SsePump::Aborted,
        None => classify(response.body.next().await),
    }
}

fn classify(item: Option<Result<SseEvent, pi_http::HttpError>>) -> SsePump {
    match item {
        Some(Ok(event)) => SsePump::Event(event),
        Some(Err(err)) => SsePump::Failed(AiError::from(err)),
        None => SsePump::Done,
    }
}

/// Build a `JsonRequest` carrying the caller's timeout and abort signal.
pub fn json_request(
    url: String,
    body: Value,
    headers: OwnedHeaders,
    options: &RequestOptions,
) -> JsonRequest {
    let mut request = JsonRequest::post(url, body);
    request.headers = headers;
    request.timeout_ms = options.timeout_ms;
    request.signal = options.signal.clone();
    request
}

/// Join a provider base URL with a path segment the way the OpenAI SDK does.
pub fn join_url(base_url: &str, path: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    format!("{base}/{path}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_authorization_header_substitutes_for_a_key() {
        let mut headers = ProviderHeaders::new();
        headers.insert("Authorization".into(), Some("Bearer xyz".into()));
        assert_eq!(client_api_key("p", None, &headers).unwrap(), "unused");
    }

    #[test]
    fn missing_key_is_an_auth_error() {
        let err = client_api_key("acme", None, &ProviderHeaders::new()).unwrap_err();
        assert_eq!(err.code(), "auth");
        assert!(err.message().contains("No API key for provider: acme"));
    }

    #[test]
    fn header_assignment_is_case_insensitive() {
        let mut headers = ProviderHeaders::new();
        headers.insert("X-Session-Id".into(), Some("a".into()));
        assign_header(&mut headers, "x-session-id", Some("b".into()));
        assert_eq!(headers.len(), 1);
        assert_eq!(headers["x-session-id"], Some("b".into()));
    }

    #[test]
    fn urls_join_without_duplicate_slashes() {
        assert_eq!(
            join_url("https://api.openai.com/v1/", "/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
    }
}
