//! Request options. Port of `ProviderRequestOptions` / `StreamOptions` /
//! `SimpleStreamOptions` from `packages/ai/src/types.ts`.
//!
//! Upstream passes callbacks (`onPayload`, `onResponse`) and an `AbortSignal`.
//! The Rust port keeps those as `Arc<dyn Fn ...>` hooks plus a
//! [`tokio_util`-style cancellation token](AbortSignal) implemented on
//! `tokio::sync::watch` so the type stays dependency-light and `Send + Sync`.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::model::{CacheRetention, Model, ThinkingBudgets, ThinkingLevel, Transport};

/// Cooperative cancellation, the port of `AbortSignal`.
#[derive(Clone, Debug)]
pub struct AbortSignal {
    rx: tokio::sync::watch::Receiver<bool>,
}

/// Owning half of an [`AbortSignal`].
#[derive(Debug)]
pub struct AbortHandle {
    tx: tokio::sync::watch::Sender<bool>,
}

impl AbortHandle {
    pub fn new() -> (AbortHandle, AbortSignal) {
        let (tx, rx) = tokio::sync::watch::channel(false);
        (AbortHandle { tx }, AbortSignal { rx })
    }

    pub fn abort(&self) {
        let _ = self.tx.send(true);
    }
}

impl AbortSignal {
    /// A signal that is never aborted.
    pub fn never() -> Self {
        let (_tx, rx) = tokio::sync::watch::channel(false);
        // Keep the sender alive for the life of the receiver.
        std::mem::forget(_tx);
        AbortSignal { rx }
    }

    pub fn is_aborted(&self) -> bool {
        *self.rx.borrow()
    }

    /// Resolves as soon as the signal is aborted.
    pub async fn aborted(&self) {
        let mut rx = self.rx.clone();
        loop {
            if *rx.borrow() {
                return;
            }
            if rx.changed().await.is_err() {
                // Sender dropped without aborting: never fires.
                std::future::pending::<()>().await;
            }
        }
    }
}

impl Default for AbortSignal {
    fn default() -> Self {
        Self::never()
    }
}

/// Header overrides. A `None` value suppresses a provider default header.
pub type ProviderHeaders = BTreeMap<String, Option<String>>;
/// Provider-scoped environment overrides; these win over the process env.
pub type ProviderEnv = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderResponse {
    pub status: u16,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

/// Called before the payload is sent; return `Some(value)` to replace it.
pub type OnPayload = Arc<dyn Fn(&Value, &Model) -> Option<Value> + Send + Sync>;
/// Called after HTTP response headers are received.
pub type OnResponse = Arc<dyn Fn(&ProviderResponse, &Model) + Send + Sync>;

/// Auth, transport and lifecycle options shared by all provider requests.
#[derive(Clone, Default)]
pub struct RequestOptions {
    pub signal: Option<AbortSignal>,
    pub api_key: Option<String>,
    pub env: ProviderEnv,
    pub headers: ProviderHeaders,
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    /// Cap on a server-requested retry delay. Default 60_000; 0 disables the cap.
    pub max_retry_delay_ms: Option<u64>,
    pub on_payload: Option<OnPayload>,
    pub on_response: Option<OnResponse>,
}

impl std::fmt::Debug for RequestOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestOptions")
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .field("headers", &self.headers)
            .field("timeout_ms", &self.timeout_ms)
            .field("max_retries", &self.max_retries)
            .field("max_retry_delay_ms", &self.max_retry_delay_ms)
            .finish_non_exhaustive()
    }
}

impl RequestOptions {
    pub fn signal(&self) -> AbortSignal {
        self.signal.clone().unwrap_or_default()
    }

    pub fn is_aborted(&self) -> bool {
        self.signal.as_ref().is_some_and(|s| s.is_aborted())
    }
}

/// Options for a streaming completion.
#[derive(Clone, Debug, Default)]
pub struct StreamOptions {
    pub request: RequestOptions,
    pub temperature: Option<f32>,
    /// Extra sampling params merged into the body after named fields, so these win.
    /// Only OpenAI-compatible adapters apply this.
    pub sampling_params: Option<Map<String, Value>>,
    pub max_tokens: Option<u64>,
    pub transport: Option<Transport>,
    pub cache_retention: Option<CacheRetention>,
    pub session_id: Option<String>,
    pub websocket_connect_timeout_ms: Option<u64>,
    /// Provider-specific metadata; providers take what they understand.
    pub metadata: Option<Map<String, Value>>,
    /// Provider-specific options that have no cross-provider meaning
    /// (the `ApiOptionsMap` escape hatch). Adapters read their own keys.
    pub provider_options: Map<String, Value>,
}

impl StreamOptions {
    pub fn signal(&self) -> AbortSignal {
        self.request.signal()
    }

    /// Read a provider-specific option by key.
    pub fn provider_option<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.provider_options
            .get(key)
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
    }

    pub fn with_provider_option(mut self, key: impl Into<String>, value: Value) -> Self {
        self.provider_options.insert(key.into(), value);
        self
    }
}

/// Deferred/async window requested from capable providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeferredWindow {
    #[serde(rename = "15m")]
    FifteenMinutes,
    #[serde(rename = "1h")]
    OneHour,
    #[serde(rename = "24h")]
    TwentyFourHours,
}

/// Options for `stream_simple`: unified reasoning control on top of [`StreamOptions`].
#[derive(Clone, Debug, Default)]
pub struct SimpleStreamOptions {
    pub stream: StreamOptions,
    pub reasoning: Option<ThinkingLevel>,
    pub deferred: Option<Deferred>,
    pub thinking_budgets: Option<ThinkingBudgets>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deferred {
    Enabled,
    Window(DeferredWindow),
}

impl SimpleStreamOptions {
    pub fn signal(&self) -> AbortSignal {
        self.stream.signal()
    }
}

/// Options for fetching a deferred response.
#[derive(Clone, Debug, Default)]
pub struct DeferredFetchOptions {
    pub request: RequestOptions,
    /// Max provider long-poll duration in ms. Default 0 = one status check.
    pub wait_ms: Option<u64>,
}
