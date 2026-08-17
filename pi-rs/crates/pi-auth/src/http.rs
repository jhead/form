//! Thin request helper for OAuth endpoints.
//!
//! Built on [`pi_http::HttpClient`] for the connection pool, proxy handling and
//! user agent, but deliberately *not* on `JsonRequest`/`post_json`: OAuth
//! endpoints carry protocol state in non-2xx bodies (`authorization_pending`,
//! `slow_down`, `invalid_grant`) and send form-encoded bodies, both of which
//! `post_json` would flatten into an error with the body discarded.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use pi_core::options::AbortSignal;
use pi_http::HttpClient;
use serde_json::{Map, Value};

use crate::error::AuthError;

/// A response read to completion as text, whatever the status.
#[derive(Debug, Clone)]
pub struct HttpText {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

impl HttpText {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Parse the body as a JSON object; `None` if it is not one.
    pub fn json_object(&self) -> Option<Map<String, Value>> {
        match serde_json::from_str::<Value>(&self.body) {
            Ok(Value::Object(map)) => Some(map),
            _ => None,
        }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_lowercase()).map(String::as_str)
    }
}

/// Read a string field from a JSON object body.
pub(crate) fn str_field(body: &Map<String, Value>, field: &str) -> Option<String> {
    body.get(field)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// Read a number field, tolerating the string encodings some providers use
/// (OpenAI's device-auth endpoint returns `interval` as `"5"`).
pub(crate) fn num_field(body: &Map<String, Value>, field: &str) -> Option<f64> {
    match body.get(field) {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

#[derive(Clone)]
pub struct OAuthHttp {
    client: Arc<HttpClient>,
}

impl std::fmt::Debug for OAuthHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthHttp").finish_non_exhaustive()
    }
}

impl Default for OAuthHttp {
    fn default() -> Self {
        Self {
            client: HttpClient::shared(),
        }
    }
}

impl OAuthHttp {
    pub fn new(client: Arc<HttpClient>) -> Self {
        Self { client }
    }

    pub async fn post_form(
        &self,
        url: &str,
        fields: &[(&str, &str)],
        headers: &[(&str, &str)],
        signal: &AbortSignal,
        timeout: Option<Duration>,
    ) -> Result<HttpText, AuthError> {
        let builder = self
            .client
            .raw()
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .form(fields);
        self.send(builder, headers, signal, timeout).await
    }

    pub async fn post_json(
        &self,
        url: &str,
        body: &Value,
        headers: &[(&str, &str)],
        signal: &AbortSignal,
        timeout: Option<Duration>,
    ) -> Result<HttpText, AuthError> {
        let builder = self
            .client
            .raw()
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(serde_json::to_string(body).unwrap_or_else(|_| "{}".into()));
        self.send(builder, headers, signal, timeout).await
    }

    pub async fn get(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        signal: &AbortSignal,
        timeout: Option<Duration>,
    ) -> Result<HttpText, AuthError> {
        let builder = self
            .client
            .raw()
            .get(url)
            .header("Accept", "application/json");
        self.send(builder, headers, signal, timeout).await
    }

    async fn send(
        &self,
        mut builder: reqwest::RequestBuilder,
        headers: &[(&str, &str)],
        signal: &AbortSignal,
        timeout: Option<Duration>,
    ) -> Result<HttpText, AuthError> {
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        if let Some(timeout) = timeout {
            builder = builder.timeout(timeout);
        }

        if signal.is_aborted() {
            return Err(AuthError::Cancelled);
        }

        let request = async {
            let response = builder.send().await.map_err(|e| {
                if e.is_timeout() {
                    AuthError::timed_out(e.to_string())
                } else {
                    AuthError::transport(e.to_string())
                }
            })?;
            let status = response.status().as_u16();
            let headers = response
                .headers()
                .iter()
                .filter_map(|(k, v)| {
                    v.to_str()
                        .ok()
                        .map(|v| (k.as_str().to_lowercase(), v.to_string()))
                })
                .collect();
            let body = response
                .text()
                .await
                .map_err(|e| AuthError::transport(e.to_string()))?;
            Ok(HttpText {
                status,
                headers,
                body,
            })
        };

        tokio::select! {
            biased;
            _ = signal.aborted() => Err(AuthError::Cancelled),
            result = request => result,
        }
    }
}
