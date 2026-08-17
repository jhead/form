//! The HTTP client every provider adapter uses.
//!
//! Wraps `reqwest` with the behaviour upstream gets from `fetch` plus its
//! wrappers: per-request headers, timeouts, abort signals, proxy env vars, and
//! SSE response bodies.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures_core::Stream;
use pi_core::options::{AbortSignal, ProviderEnv, ProviderHeaders};
use serde_json::Value;

use crate::error_body::extract_error;
use crate::node_http_proxy::{
    has_proxy_overrides, resolve_http_proxy_url_for_target, PROXY_ENV_KEYS,
};
use crate::retry::{retry_attempts, AttemptFailure, RetryPolicy};
use crate::sse::{sse_stream, SseEvent};
use crate::HttpError;

/// Plain header map with case-insensitive lookup semantics handled by reqwest.
pub type OwnedHeaders = BTreeMap<String, String>;

#[derive(Debug, Clone)]
pub struct HttpClientConfig {
    pub timeout_ms: Option<u64>,
    pub connect_timeout_ms: Option<u64>,
    /// Honour `HTTPS_PROXY` / `HTTP_PROXY` / `NO_PROXY` (and the provider-scoped
    /// overrides passed through `RequestOptions::env`).
    pub use_proxy_env: bool,
    pub user_agent: String,
}

impl Default for HttpClientConfig {
    fn default() -> Self {
        Self {
            timeout_ms: None,
            connect_timeout_ms: Some(30_000),
            use_proxy_env: true,
            user_agent: default_user_agent(),
        }
    }
}

/// `pi/<version> (rust)` — port of `utils/pi-user-agent.ts`.
pub fn default_user_agent() -> String {
    format!("pi-rs/{} (rust)", env!("CARGO_PKG_VERSION"))
}

/// Shared HTTP client. Cheap to clone; holds one connection pool.
#[derive(Debug, Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
}

impl Default for HttpClient {
    fn default() -> Self {
        Self::new(HttpClientConfig::default()).expect("default http client builds")
    }
}

impl HttpClient {
    pub fn new(config: HttpClientConfig) -> Result<Self, HttpError> {
        let mut builder = reqwest::Client::builder().user_agent(config.user_agent.clone());
        if let Some(ms) = config.connect_timeout_ms {
            builder = builder.connect_timeout(Duration::from_millis(ms));
        }
        if let Some(ms) = config.timeout_ms {
            builder = builder.timeout(Duration::from_millis(ms));
        }
        if !config.use_proxy_env {
            builder = builder.no_proxy();
        }
        builder
            .build()
            .map(|inner| Self { inner })
            .map_err(|e| HttpError::Transport(e.to_string()))
    }

    /// A process-wide default client.
    pub fn shared() -> Arc<HttpClient> {
        static SHARED: std::sync::OnceLock<Arc<HttpClient>> = std::sync::OnceLock::new();
        SHARED
            .get_or_init(|| Arc::new(HttpClient::default()))
            .clone()
    }

    /// A client whose proxy resolution honours provider-scoped `env` overrides
    /// on top of the process environment.
    ///
    /// `reqwest` fixes proxy configuration at client-build time and only reads
    /// the process environment, so per-provider overrides (from
    /// [`RequestOptions::env`](pi_core::options::RequestOptions)) need their own
    /// client. A custom resolver runs
    /// [`crate::node_http_proxy::resolve_http_proxy_url_for_target`]
    /// per request URL, which also gives `NO_PROXY` its per-host semantics.
    ///
    /// An unsupported proxy scheme (SOCKS, PAC) fails here rather than being
    /// silently bypassed.
    pub fn with_proxy_env(
        config: HttpClientConfig,
        env: &ProviderEnv,
    ) -> Result<HttpClient, HttpError> {
        if !config.use_proxy_env {
            return HttpClient::new(config);
        }
        // Surface a misconfigured proxy now instead of on the first request,
        // where the resolver has nowhere to report it.
        resolve_http_proxy_url_for_target("https://example.invalid/", env)
            .map_err(|e| HttpError::InvalidRequest(e.to_string()))?;

        let scoped = env.clone();
        let proxy = reqwest::Proxy::custom(move |url| {
            resolve_http_proxy_url_for_target(url.as_str(), &scoped)
                .ok()
                .flatten()
        });

        let mut builder = reqwest::Client::builder()
            .user_agent(config.user_agent.clone())
            // Our resolver is the single source of truth; letting reqwest also
            // read the process env would double-apply NO_PROXY.
            .no_proxy()
            .proxy(proxy);
        if let Some(ms) = config.connect_timeout_ms {
            builder = builder.connect_timeout(Duration::from_millis(ms));
        }
        if let Some(ms) = config.timeout_ms {
            builder = builder.timeout(Duration::from_millis(ms));
        }
        builder
            .build()
            .map(|inner| Self { inner })
            .map_err(|e| HttpError::Transport(e.to_string()))
    }

    /// [`with_proxy_env`](Self::with_proxy_env), cached by the proxy-relevant
    /// entries of `env` so repeated requests share a connection pool.
    ///
    /// Returns the shared default client when `env` carries no proxy overrides,
    /// which is the common case.
    pub fn shared_for_env(env: &ProviderEnv) -> Result<Arc<HttpClient>, HttpError> {
        if !has_proxy_overrides(env) {
            return Ok(HttpClient::shared());
        }
        static CACHE: std::sync::OnceLock<std::sync::Mutex<BTreeMap<String, Arc<HttpClient>>>> =
            std::sync::OnceLock::new();

        let key = PROXY_ENV_KEYS
            .iter()
            .filter_map(|name| env.get(*name).map(|value| format!("{name}={value}")))
            .collect::<Vec<_>>()
            .join("\u{1}");

        let cache = CACHE.get_or_init(|| std::sync::Mutex::new(BTreeMap::new()));
        let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(client) = cache.get(&key) {
            return Ok(client.clone());
        }
        let client = Arc::new(HttpClient::with_proxy_env(
            HttpClientConfig::default(),
            env,
        )?);
        cache.insert(key, client.clone());
        Ok(client)
    }

    pub fn raw(&self) -> &reqwest::Client {
        &self.inner
    }

    /// POST JSON and read a JSON response. Non-2xx becomes `HttpError::Status`
    /// with the provider's error body attached.
    pub async fn post_json(&self, req: JsonRequest) -> Result<HttpResponse, HttpError> {
        self.post_json_reporting(req)
            .await
            .map_err(|failure| failure.error)
    }

    /// GET a JSON response.
    pub async fn get_json(&self, req: JsonRequest) -> Result<HttpResponse, HttpError> {
        let mut req = req;
        req.method = Method::Get;
        self.post_json_reporting(req)
            .await
            .map_err(|failure| failure.error)
    }

    /// POST JSON and read a `text/event-stream` response.
    pub async fn post_sse(&self, req: JsonRequest) -> Result<SseResponse, HttpError> {
        self.post_sse_reporting(req)
            .await
            .map_err(|failure| failure.error)
    }

    /// [`post_json`](Self::post_json) with automatic retry.
    ///
    /// Retryability uses the response headers (`x-should-retry`) and the
    /// server's `retry-after`, which the plain variant cannot see because
    /// `HttpError` does not carry headers.
    pub async fn post_json_retrying(
        &self,
        req: JsonRequest,
        policy: &RetryPolicy,
    ) -> Result<HttpResponse, HttpError> {
        let signal = req.signal.clone();
        retry_attempts(policy, signal.as_ref(), |_| {
            self.post_json_reporting(req.clone())
        })
        .await
    }

    /// [`post_sse`](Self::post_sse) with automatic retry.
    ///
    /// Only the *connection* is retried. Once the stream is handed back, a
    /// mid-stream failure is the adapter's business, since replaying it would
    /// duplicate already-emitted events.
    pub async fn post_sse_retrying(
        &self,
        req: JsonRequest,
        policy: &RetryPolicy,
    ) -> Result<SseResponse, HttpError> {
        let signal = req.signal.clone();
        retry_attempts(policy, signal.as_ref(), |_| {
            self.post_sse_reporting(req.clone())
        })
        .await
    }

    async fn post_json_reporting(&self, req: JsonRequest) -> Result<HttpResponse, AttemptFailure> {
        let response = self.send(&req).await.map_err(AttemptFailure::new)?;
        let status = response.status().as_u16();
        let headers = collect_headers(response.headers());
        let body = response.text().await.map_err(|e| {
            AttemptFailure::with_headers(HttpError::Transport(e.to_string()), headers.clone())
        })?;
        if !(200..300).contains(&status) {
            return Err(AttemptFailure::with_headers(
                extract_error(status, &headers, &body),
                headers,
            ));
        }
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }

    async fn post_sse_reporting(&self, req: JsonRequest) -> Result<SseResponse, AttemptFailure> {
        let response = self.send(&req).await.map_err(AttemptFailure::new)?;
        let status = response.status().as_u16();
        let headers = collect_headers(response.headers());
        if !(200..300).contains(&status) {
            let body = response.text().await.unwrap_or_default();
            return Err(AttemptFailure::with_headers(
                extract_error(status, &headers, &body),
                headers,
            ));
        }
        Ok(SseResponse {
            status,
            headers,
            body: Box::pin(sse_stream(response.bytes_stream())),
        })
    }

    async fn send(&self, req: &JsonRequest) -> Result<reqwest::Response, HttpError> {
        let builder = req.build_request(&self.inner)?;
        let timeout_ms = req.timeout_ms;
        let fut = builder.send();
        match &req.signal {
            Some(signal) if !signal.is_aborted() => {
                tokio::select! {
                    biased;
                    _ = signal.aborted() => Err(HttpError::Aborted),
                    result = fut => result.map_err(|e| map_reqwest_error(e, timeout_ms)),
                }
            }
            Some(_) => Err(HttpError::Aborted),
            None => fut.await.map_err(|e| map_reqwest_error(e, timeout_ms)),
        }
    }
}

fn map_reqwest_error(err: reqwest::Error, timeout_ms: Option<u64>) -> HttpError {
    if err.is_timeout() {
        // The configured deadline when there is one, so the error text is
        // actionable rather than reporting `0ms`.
        return HttpError::Timeout(timeout_ms.unwrap_or(0));
    }
    HttpError::Transport(error_chain(&err))
}

/// Flatten an error and its `source()` chain into one line.
///
/// `reqwest::Error`'s own `Display` stops at "error sending request for url
/// (…)" and leaves the actual cause in the chain. That matters beyond
/// readability: [`crate::retry::is_retryable_error_message`] classifies a failed
/// turn from its text, and without the cause a refused connection or a DNS
/// failure looks like an unrecognised error and is never retried.
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(current) = source {
        let text = current.to_string();
        // Chains often repeat the parent's text; do not print it twice.
        if !message.contains(&text) {
            message.push_str(": ");
            message.push_str(&text);
        }
        source = current.source();
    }
    message
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Delete,
}

/// A JSON request with the knobs adapters need.
#[derive(Debug, Clone)]
pub struct JsonRequest {
    pub method: Method,
    pub url: String,
    pub body: Option<Value>,
    /// Final headers. Build these with [`crate::merge_headers`].
    pub headers: OwnedHeaders,
    pub timeout_ms: Option<u64>,
    pub signal: Option<AbortSignal>,
}

impl JsonRequest {
    pub fn post(url: impl Into<String>, body: Value) -> Self {
        Self {
            method: Method::Post,
            url: url.into(),
            body: Some(body),
            headers: BTreeMap::new(),
            timeout_ms: None,
            signal: None,
        }
    }

    pub fn get(url: impl Into<String>) -> Self {
        Self {
            method: Method::Get,
            url: url.into(),
            body: None,
            headers: BTreeMap::new(),
            timeout_ms: None,
            signal: None,
        }
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    /// Apply caller header overrides; a `None` value removes a default header.
    pub fn with_overrides(mut self, overrides: &ProviderHeaders) -> Self {
        self.headers = crate::merge_headers(self.headers, overrides);
        self
    }

    pub fn signal(mut self, signal: Option<AbortSignal>) -> Self {
        self.signal = signal;
        self
    }

    pub fn timeout_ms(mut self, ms: Option<u64>) -> Self {
        self.timeout_ms = ms;
        self
    }

    fn build_request(
        &self,
        client: &reqwest::Client,
    ) -> Result<reqwest::RequestBuilder, HttpError> {
        let mut builder = match self.method {
            Method::Get => client.get(&self.url),
            Method::Post => client.post(&self.url),
            Method::Delete => client.delete(&self.url),
        };
        for (name, value) in &self.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = &self.body {
            builder = builder.json(body);
        }
        if let Some(ms) = self.timeout_ms {
            builder = builder.timeout(Duration::from_millis(ms));
        }
        Ok(builder)
    }
}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: OwnedHeaders,
    pub body: String,
}

impl HttpResponse {
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, HttpError> {
        serde_json::from_str(&self.body)
            .map_err(|e| HttpError::Transport(format!("invalid JSON response: {e}")))
    }
}

/// A streaming SSE response.
pub struct SseResponse {
    pub status: u16,
    pub headers: OwnedHeaders,
    pub body: std::pin::Pin<Box<dyn Stream<Item = Result<SseEvent, HttpError>> + Send>>,
}

impl std::fmt::Debug for SseResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SseResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

pub(crate) fn collect_headers(headers: &reqwest::header::HeaderMap) -> OwnedHeaders {
    headers
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|v| (k.as_str().to_lowercase(), v.to_string()))
        })
        .collect()
}
