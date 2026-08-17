//! Retry policy and retryability classification.
//! Port of `packages/ai/src/utils/{retry,provider-retry}.ts`.
//!
//! There are two independent classifiers upstream, and both are needed:
//!
//! - **Transport level** (`provider-retry.ts`): decide from the HTTP status and
//!   response headers. Mirrors the pinned OpenAI/Anthropic SDK policy, including
//!   the `x-should-retry` override those SDKs send. See
//!   [`is_retryable_provider_error`].
//! - **Message level** (`retry.ts`): decide from the *text* of a failed
//!   assistant turn, because by then the status is long gone and the only signal
//!   left is provider wording. See [`is_retryable_error_message`].
//!
//! Backoff matches upstream exactly: `min(0.5 * 2^attempt, 8)` seconds with
//! jitter in `[0.75, 1.0)`. A server-supplied delay always wins over the
//! computed one, and a server delay above the cap fails immediately rather than
//! silently parking the request for minutes — upstream's
//! `validateServerRetryDelayMs`, motivated by providers that ask for hours.

use std::future::Future;
use std::time::Duration;

use once_cell::sync::Lazy;
use pi_core::message::{AssistantMessage, StopReason};
use pi_core::options::AbortSignal;
use rand::Rng;
use regex::Regex;

use crate::abort_signals::sleep_unless_aborted;
use crate::client::OwnedHeaders;
use crate::error_body::retry_after_ms;
use crate::HttpError;

/// Default cap on a server-requested delay. Upstream's `DEFAULT_MAX_RETRY_DELAY_MS`.
pub const DEFAULT_MAX_RETRY_DELAY_MS: u64 = 60_000;

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
    pub multiplier: f64,
    /// Cap on a server-requested delay. Exceeding it fails immediately so a
    /// higher layer can surface the wait to the user. 0 disables the cap.
    pub max_server_delay_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        // Upstream: `Math.min(0.5 * 2 ** retryIndex, 8) * 1000`.
        Self {
            max_attempts: 3,
            initial_delay_ms: 500,
            max_delay_ms: 8_000,
            multiplier: 2.0,
            max_server_delay_ms: DEFAULT_MAX_RETRY_DELAY_MS,
        }
    }
}

impl RetryPolicy {
    pub fn with_max_attempts(mut self, attempts: u32) -> Self {
        self.max_attempts = attempts.max(1);
        self
    }

    pub fn with_max_server_delay_ms(mut self, ms: u64) -> Self {
        self.max_server_delay_ms = ms;
        self
    }

    /// Backoff for `attempt` (0-based), jittered.
    ///
    /// Upstream applies `delay * (1 - random() * 0.25)`: jitter only ever
    /// *shortens* the wait, so the cap is a true upper bound.
    pub fn delay_for(&self, attempt: u32) -> Duration {
        self.delay_for_with_jitter(attempt, rand::thread_rng().gen::<f64>())
    }

    /// [`delay_for`](Self::delay_for) with the jitter draw supplied, so the
    /// curve can be asserted without sampling.
    pub fn delay_for_with_jitter(&self, attempt: u32, jitter_unit: f64) -> Duration {
        let base = (self.initial_delay_ms as f64) * self.multiplier.powi(attempt as i32);
        let capped = base.min(self.max_delay_ms as f64).max(0.0);
        let jittered = capped * (1.0 - jitter_unit.clamp(0.0, 1.0) * 0.25);
        Duration::from_millis(jittered.max(0.0) as u64)
    }

    /// Whether `delay_ms` exceeds the server-delay cap.
    pub fn exceeds_server_delay_cap(&self, delay_ms: u64) -> bool {
        self.max_server_delay_ms > 0 && delay_ms > self.max_server_delay_ms
    }

    /// Upstream's `validateServerRetryDelayMs` error text, which the
    /// message-level classifier then matches on via the `retry delay` pattern.
    pub fn server_delay_rejection(&self, delay_ms: u64, provider_message: &str) -> HttpError {
        HttpError::Transport(format!(
            "Server requested {}s retry delay (max: {}s). {provider_message}",
            delay_ms.div_ceil(1000),
            self.max_server_delay_ms.div_ceil(1000),
        ))
    }
}

// --- transport-level classification (provider-retry.ts) --------------------

/// Whether a provider response should be retried, from its status and headers.
///
/// `status: None` means the request never got a response (a transport failure),
/// which upstream treats as retryable. The `x-should-retry` header, which the
/// OpenAI and Anthropic SDKs both send, overrides the status entirely.
pub fn is_retryable_provider_error(status: Option<u16>, headers: &OwnedHeaders) -> bool {
    match headers.get("x-should-retry").map(|v| v.trim()) {
        Some("true") => return true,
        Some("false") => return false,
        _ => {}
    }
    match status {
        None => true,
        Some(status) => status == 408 || status == 409 || status == 429 || status >= 500,
    }
}

/// The delay before the next attempt, honouring server hints.
///
/// Returns `Err` when the server asked for longer than `policy` allows, so the
/// caller fails fast instead of parking the request.
pub fn provider_retry_delay(
    policy: &RetryPolicy,
    headers: &OwnedHeaders,
    attempt: u32,
    provider_message: &str,
) -> Result<Duration, HttpError> {
    if let Some(server_delay) = retry_after_ms(headers) {
        if policy.exceeds_server_delay_cap(server_delay) {
            return Err(policy.server_delay_rejection(server_delay, provider_message));
        }
        return Ok(Duration::from_millis(server_delay));
    }
    Ok(policy.delay_for(attempt))
}

/// A failed attempt, carrying the response headers the classifier needs.
///
/// `HttpError` cannot hold headers without changing its shape, and adapters are
/// already matching on it, so retryability travels alongside instead.
#[derive(Debug, Clone)]
pub struct AttemptFailure {
    pub error: HttpError,
    /// Response headers, empty when the request never got a response.
    pub headers: OwnedHeaders,
}

impl AttemptFailure {
    pub fn new(error: HttpError) -> Self {
        Self {
            error,
            headers: OwnedHeaders::new(),
        }
    }

    pub fn with_headers(error: HttpError, headers: OwnedHeaders) -> Self {
        Self { error, headers }
    }

    fn status(&self) -> Option<u16> {
        match &self.error {
            HttpError::Status { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// Whether this failure should be retried at all.
    pub fn is_retryable(&self) -> bool {
        match &self.error {
            // Cancellation and caller mistakes are terminal regardless of headers.
            HttpError::Aborted | HttpError::InvalidRequest(_) => false,
            _ => is_retryable_provider_error(self.status(), &self.headers),
        }
    }
}

impl From<HttpError> for AttemptFailure {
    fn from(error: HttpError) -> Self {
        AttemptFailure::new(error)
    }
}

/// Run `op`, retrying retryable failures according to `policy`.
///
/// A server-supplied delay above `max_server_delay_ms` aborts immediately,
/// matching upstream.
pub async fn retry_with_backoff<F, Fut, T>(
    policy: &RetryPolicy,
    signal: Option<&AbortSignal>,
    mut op: F,
) -> Result<T, HttpError>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<T, HttpError>>,
{
    retry_attempts(policy, signal, move |attempt| {
        let fut = op(attempt);
        async move { fut.await.map_err(AttemptFailure::from) }
    })
    .await
}

/// [`retry_with_backoff`] for operations that can report response headers, so
/// `x-should-retry` and `retry-after` are honoured.
pub async fn retry_attempts<F, Fut, T>(
    policy: &RetryPolicy,
    signal: Option<&AbortSignal>,
    mut op: F,
) -> Result<T, HttpError>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<T, AttemptFailure>>,
{
    let mut attempt = 0u32;
    loop {
        if signal.is_some_and(|s| s.is_aborted()) {
            return Err(HttpError::Aborted);
        }
        match op(attempt).await {
            Ok(value) => return Ok(value),
            Err(failure) => {
                let last_attempt = attempt + 1 >= policy.max_attempts;
                if last_attempt || !failure.is_retryable() {
                    return Err(failure.error);
                }
                let message = failure.error.to_string();
                // A response can carry `retry-after` in headers or, for errors
                // built by `extract_error`, folded into the error itself.
                let mut headers = failure.headers.clone();
                if let HttpError::Status {
                    retry_after_ms: Some(ms),
                    ..
                } = &failure.error
                {
                    headers
                        .entry("retry-after-ms".to_string())
                        .or_insert_with(|| ms.to_string());
                }
                let delay = match provider_retry_delay(policy, &headers, attempt, &message) {
                    Ok(delay) => delay,
                    // Cap exceeded: surface the original failure, as upstream does.
                    Err(_) => return Err(failure.error),
                };
                sleep_unless_aborted(delay, signal).await?;
                attempt += 1;
            }
        }
    }
}

// --- message-level classification (retry.ts) -------------------------------

/// Subscription / quota exhaustion. Deterministic, so never retried even when
/// the wording also matches a transient pattern.
const NON_RETRYABLE_PROVIDER_LIMIT_PATTERNS: &[&str] = &[
    // OpenCode Zen returns these as 429 JSON error types; they are account
    // limits, not throttles.
    "GoUsageLimitError",
    "FreeUsageLimitError",
    "Monthly usage limit reached",
    "available balance",
    // Generic quota / budget / billing exhaustion. `insufficient_quota` is
    // OpenAI's billing code; the rest cover common gateway wording.
    "insufficient_quota",
    "out of budget",
    "quota exceeded",
    "billing",
];

const RETRYABLE_PROVIDER_ERROR_PATTERNS: &[&str] = &[
    // Provider load, HTTP status, server-side transients.
    "overloaded",
    "rate.?limit",
    "too many requests",
    "429",
    "500",
    "502",
    "503",
    "504",
    "524",
    "service.?unavailable",
    "server.?error",
    "internal.?error",
    // Wrapper/gateway text, including OpenRouter "Provider returned error".
    "provider.?returned.?error",
    "exceeded request buffer limit while retrying upstream",
    // Network, proxy and fetch transport failures. Covers OpenAI Codex raw
    // fetch failures and OpenRouter connection drops.
    "network.?error",
    "connection.?error",
    "connection.?refused",
    "connection.?lost",
    "other side closed",
    "fetch failed",
    "getaddrinfo",
    "ENOTFOUND",
    "EAI_AGAIN",
    "upstream.?connect",
    "reset before headers",
    "socket hang up",
    "socket connection was closed",
    "timed? out",
    "timeout",
    "terminated",
    // WebSocket transports report close/error text instead of HTTP text.
    "websocket.?closed",
    "websocket.?error",
    // Premature stream endings from SDKs and transports.
    "ended without",
    "stream ended before message_stop",
    "stream ended before a terminal response event",
    "http2 request did not get a response",
    // A rejected provider-requested delay must flow through the outer policy so
    // the caller can surface or abort the backoff.
    "retry delay",
    // Explicit mid-stream retry guidance from OpenAI Responses and Bedrock.
    "you can retry your request",
    "try your request again",
    "please retry your request",
    // gRPC-based providers (NVIDIA NIM).
    "ResourceExhausted",
];

fn build_pattern(parts: &[&str]) -> Regex {
    Regex::new(&format!("(?i){}", parts.join("|"))).expect("retry patterns compile")
}

static NON_RETRYABLE_LIMIT: Lazy<Regex> =
    Lazy::new(|| build_pattern(NON_RETRYABLE_PROVIDER_LIMIT_PATTERNS));
static RETRYABLE_ERROR: Lazy<Regex> =
    Lazy::new(|| build_pattern(RETRYABLE_PROVIDER_ERROR_PATTERNS));

/// Whether error *text* looks like a transient provider or transport failure.
///
/// This is classification only — no policy. Callers handle context overflow
/// first (see [`crate::overflow`]), then apply their own budget and backoff.
pub fn is_retryable_error_message(error_message: &str) -> bool {
    if error_message.is_empty() {
        return false;
    }
    if NON_RETRYABLE_LIMIT.is_match(error_message) {
        return false;
    }
    RETRYABLE_ERROR.is_match(error_message)
}

/// Whether a failed assistant turn is worth restarting.
pub fn is_retryable_assistant_error(message: &AssistantMessage) -> bool {
    if message.stop_reason != StopReason::Error {
        return false;
    }
    message
        .error_message
        .as_deref()
        .is_some_and(is_retryable_error_message)
}

/// Bounded retry of an assistant-producing call. Upstream's `RetryPolicy` from
/// `retry.ts`, distinct from the transport-level [`RetryPolicy`] above.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssistantRetryPolicy {
    pub enabled: bool,
    /// Max retry attempts. The initial call never counts as a retry.
    pub max_retries: u32,
    /// Per-attempt delay is `base_delay_ms * 2^(attempt - 1)`.
    pub base_delay_ms: u64,
}

impl Default for AssistantRetryPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 1_000,
        }
    }
}

/// Callbacks emitted around each retry by [`retry_assistant_call`].
///
/// An object-safe trait rather than closures, so it crosses the FFI boundary.
pub trait RetryObserver: Send + Sync {
    /// Before the backoff sleep of each retry attempt (1-indexed).
    fn on_retry_scheduled(
        &self,
        _attempt: u32,
        _max_attempts: u32,
        _delay_ms: u64,
        _error_message: &str,
    ) {
    }
    /// After the backoff sleep, immediately before the retried call starts.
    fn on_retry_attempt_start(&self) {}
    /// Once when the loop ends. `success` is false for exhaustion and aborts.
    fn on_retry_finished(&self, _success: bool, _attempt: u32, _final_error: Option<&str>) {}
}

/// Run a single assistant-producing call with bounded retry on transient errors.
///
/// - A success or an abort returns immediately; an abort is never retried.
/// - A non-retryable error (quota, billing, …) returns immediately so
///   deterministic failures fail fast.
/// - An abort *during the backoff sleep* is normalised into an aborted
///   `AssistantMessage`, so callers need not care when cancellation happened.
///
/// With `policy` absent or disabled this is equivalent to calling `produce`.
pub async fn retry_assistant_call<F, Fut>(
    mut produce: F,
    policy: Option<&AssistantRetryPolicy>,
    signal: Option<&AbortSignal>,
    observer: Option<&dyn RetryObserver>,
) -> AssistantMessage
where
    F: FnMut() -> Fut,
    Fut: Future<Output = AssistantMessage>,
{
    let max_attempts = match policy {
        Some(p) if p.enabled => p.max_retries,
        _ => 0,
    };
    let base_delay_ms = policy.map(|p| p.base_delay_ms).unwrap_or(0);

    let mut attempt = 0u32;
    let mut last_retry: Option<(u32, String)> = None;

    loop {
        let response = produce().await;

        if response.stop_reason == StopReason::Aborted {
            if let (Some(observer), Some((attempt, _))) = (observer, &last_retry) {
                observer.on_retry_finished(false, *attempt, None);
            }
            return response;
        }
        if response.stop_reason != StopReason::Error {
            if let (Some(observer), Some((attempt, _))) = (observer, &last_retry) {
                observer.on_retry_finished(true, *attempt, None);
            }
            return response;
        }
        if attempt >= max_attempts || !is_retryable_assistant_error(&response) {
            if let (Some(observer), Some((attempt, _))) = (observer, &last_retry) {
                observer.on_retry_finished(false, *attempt, response.error_message.as_deref());
            }
            return response;
        }

        attempt += 1;
        let error_message = response
            .error_message
            .clone()
            .unwrap_or_else(|| "Unknown error".to_string());
        last_retry = Some((attempt, error_message.clone()));
        let delay_ms = base_delay_ms.saturating_mul(2u64.saturating_pow(attempt - 1));
        if let Some(observer) = observer {
            observer.on_retry_scheduled(attempt, max_attempts, delay_ms, &error_message);
        }

        if sleep_unless_aborted(Duration::from_millis(delay_ms), signal)
            .await
            .is_err()
        {
            if let Some(observer) = observer {
                observer.on_retry_finished(false, attempt, Some(&error_message));
            }
            // Normalise to the same shape a provider stream abort produces.
            let mut aborted = response;
            aborted.stop_reason = StopReason::Aborted;
            aborted.error_message = None;
            return aborted;
        }
        if let Some(observer) = observer {
            observer.on_retry_attempt_start();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    fn headers(pairs: &[(&str, &str)]) -> OwnedHeaders {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn error_message(text: &str) -> AssistantMessage {
        let mut message = AssistantMessage::pending("a", "p", "m");
        message.stop_reason = StopReason::Error;
        message.error_message = Some(text.to_string());
        message
    }

    // --- backoff curve ------------------------------------------------------

    #[test]
    fn backoff_matches_upstreams_curve_and_cap() {
        let policy = RetryPolicy::default();
        // `min(0.5 * 2^attempt, 8)` seconds, before jitter.
        let undithered: Vec<u64> = (0..6)
            .map(|a| policy.delay_for_with_jitter(a, 0.0).as_millis() as u64)
            .collect();
        assert_eq!(undithered, vec![500, 1_000, 2_000, 4_000, 8_000, 8_000]);
    }

    #[test]
    fn jitter_only_ever_shortens_the_wait() {
        let policy = RetryPolicy::default();
        // Upstream: delay * (1 - random * 0.25), so the range is [75%, 100%].
        assert_eq!(policy.delay_for_with_jitter(2, 0.0).as_millis(), 2_000);
        assert_eq!(policy.delay_for_with_jitter(2, 1.0).as_millis(), 1_500);
        for _ in 0..200 {
            let delay = policy.delay_for(2).as_millis() as u64;
            assert!((1_500..=2_000).contains(&delay), "{delay}");
        }
    }

    #[test]
    fn the_server_delay_cap_defaults_to_sixty_seconds() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_server_delay_ms, DEFAULT_MAX_RETRY_DELAY_MS);
        assert!(!policy.exceeds_server_delay_cap(60_000));
        assert!(policy.exceeds_server_delay_cap(60_001));
        // Zero disables the cap entirely.
        assert!(!RetryPolicy::default()
            .with_max_server_delay_ms(0)
            .exceeds_server_delay_cap(u64::MAX));
    }

    /// Upstream asserts on this exact wording, and the message-level classifier
    /// matches it via the `retry delay` pattern.
    #[test]
    fn a_rejected_server_delay_reports_seconds_rounded_up() {
        let policy = RetryPolicy::default().with_max_server_delay_ms(1_000);
        let error = policy.server_delay_rejection(277_403_000, "Provider error: 429");
        assert!(
            error
                .to_string()
                .contains("Server requested 277403s retry delay (max: 1s)"),
            "{error}"
        );
        assert!(is_retryable_error_message(&error.to_string()));
    }

    #[test]
    fn a_server_delay_within_the_cap_wins_over_the_computed_backoff() {
        let policy = RetryPolicy::default();
        let delay =
            provider_retry_delay(&policy, &headers(&[("retry-after-ms", "1000")]), 0, "").unwrap();
        assert_eq!(delay, Duration::from_millis(1_000));
        // Without a hint, the computed backoff applies.
        let delay = provider_retry_delay(&policy, &headers(&[]), 0, "").unwrap();
        assert!(delay <= Duration::from_millis(500));
    }

    #[test]
    fn a_server_delay_above_the_cap_is_rejected() {
        let policy = RetryPolicy::default().with_max_server_delay_ms(1_000);
        let result = provider_retry_delay(
            &policy,
            &headers(&[("retry-after", "277403")]),
            0,
            "Provider error: 429",
        );
        assert!(result.is_err());
    }

    // --- transport-level classification -------------------------------------

    #[test]
    fn classifies_statuses_the_way_the_pinned_sdks_do() {
        let none = headers(&[]);
        for status in [408, 409, 429, 500, 502, 503, 504, 599] {
            assert!(is_retryable_provider_error(Some(status), &none), "{status}");
        }
        for status in [400, 401, 403, 404, 422] {
            assert!(
                !is_retryable_provider_error(Some(status), &none),
                "{status}"
            );
        }
        // No response at all is a transport failure, which is retryable.
        assert!(is_retryable_provider_error(None, &none));
    }

    #[test]
    fn the_should_retry_header_overrides_the_status() {
        assert!(is_retryable_provider_error(
            Some(400),
            &headers(&[("x-should-retry", "true")])
        ));
        assert!(!is_retryable_provider_error(
            Some(429),
            &headers(&[("x-should-retry", "false")])
        ));
        // Anything else falls back to the status.
        assert!(is_retryable_provider_error(
            Some(429),
            &headers(&[("x-should-retry", "maybe")])
        ));
    }

    #[test]
    fn aborts_and_caller_errors_are_never_retried() {
        assert!(!AttemptFailure::new(HttpError::Aborted).is_retryable());
        assert!(!AttemptFailure::new(HttpError::InvalidRequest("bad".into())).is_retryable());
        assert!(AttemptFailure::new(HttpError::Transport("reset".into())).is_retryable());
        assert!(AttemptFailure::new(HttpError::Timeout(1)).is_retryable());
    }

    // --- retry loop ---------------------------------------------------------

    #[tokio::test]
    async fn retries_until_success() {
        let calls = Arc::new(AtomicU32::new(0));
        let policy = RetryPolicy {
            initial_delay_ms: 1,
            ..Default::default()
        };
        let c = calls.clone();
        let result: Result<u32, HttpError> = retry_with_backoff(&policy, None, move |_| {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(HttpError::Transport("boom".into()))
                } else {
                    Ok(n)
                }
            }
        })
        .await;
        assert_eq!(result.unwrap(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn does_not_retry_non_retryable() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let result: Result<(), HttpError> =
            retry_with_backoff(&RetryPolicy::default(), None, move |_| {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Err(HttpError::InvalidRequest("nope".into()))
                }
            })
            .await;
        assert!(matches!(result, Err(HttpError::InvalidRequest(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stops_at_max_attempts_and_returns_the_last_error() {
        let calls = Arc::new(AtomicU32::new(0));
        let policy = RetryPolicy {
            max_attempts: 3,
            initial_delay_ms: 1,
            ..Default::default()
        };
        let c = calls.clone();
        let result: Result<(), HttpError> = retry_with_backoff(&policy, None, move |_| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(HttpError::Transport("still down".into()))
            }
        })
        .await;
        assert!(matches!(result, Err(HttpError::Transport(_))));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "initial call plus two retries"
        );
    }

    #[tokio::test]
    async fn honours_the_should_retry_header_through_the_loop() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let result: Result<(), HttpError> =
            retry_attempts(&RetryPolicy::default(), None, move |_| {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Err(AttemptFailure::with_headers(
                        HttpError::Status {
                            status: 429,
                            message: "slow down".into(),
                            body: None,
                            retry_after_ms: None,
                        },
                        headers(&[("x-should-retry", "false")]),
                    ))
                }
            })
            .await;
        assert!(result.is_err());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the header should stop the loop"
        );
    }

    #[tokio::test]
    async fn a_server_delay_above_the_cap_fails_immediately() {
        let calls = Arc::new(AtomicU32::new(0));
        let policy = RetryPolicy::default().with_max_server_delay_ms(1_000);
        let c = calls.clone();
        let result: Result<(), HttpError> = retry_with_backoff(&policy, None, move |_| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(HttpError::Status {
                    status: 429,
                    message: "slow down".into(),
                    body: None,
                    retry_after_ms: Some(300_000),
                })
            }
        })
        .await;
        assert!(matches!(result, Err(HttpError::Status { status: 429, .. })));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_abort_during_backoff_ends_the_loop() {
        let (handle, signal) = pi_core::options::AbortHandle::new();
        let policy = RetryPolicy {
            initial_delay_ms: 60_000,
            ..Default::default()
        };
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            handle.abort();
        });
        let result: Result<(), HttpError> = retry_with_backoff(&policy, Some(&signal), move |_| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(HttpError::Transport("boom".into()))
            }
        })
        .await;
        assert!(matches!(result, Err(HttpError::Aborted)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_already_aborted_signal_skips_the_call_entirely() {
        let (handle, signal) = pi_core::options::AbortHandle::new();
        handle.abort();
        let result: Result<(), HttpError> =
            retry_with_backoff(&RetryPolicy::default(), Some(&signal), |_| async {
                unreachable!("must not run")
            })
            .await;
        assert!(matches!(result, Err(HttpError::Aborted)));
    }

    // --- message-level classification ---------------------------------------

    #[test]
    fn matches_explicit_provider_retry_guidance() {
        let cases = [
            "An error occurred while processing your request. You can retry your request, or contact us through our help center at help.openai.com if the error persists. Please include the request ID req_******** in your message.",
            r#"{"message":"The system encountered an unexpected error during processing. Try your request again."}"#,
            "ResourceExhausted: Worker local total request limit reached (288/48)",
        ];
        for text in cases {
            assert!(is_retryable_assistant_error(&error_message(text)), "{text}");
        }
    }

    #[test]
    fn matches_transport_and_stream_failure_wording() {
        let cases = [
            "The socket connection was closed unexpectedly. For more information, pass `verbose: true` in the second argument to fetch()",
            "Error: exceeded request buffer limit while retrying upstream",
            "The pending stream has been canceled (caused by: getaddrinfo ENOTFOUND bedrock-runtime.us-east-1.amazonaws.com)",
            "connect ENOTFOUND api.example.com",
            "EAI_AGAIN api.example.com",
            "getaddrinfo failed for api.example.com",
            "OpenAI Responses stream ended before a terminal response event",
            "Anthropic stream ended before message_stop",
            "overloaded_error",
            "524 status code (no body)",
            "HTTP2 request did not get a response",
            "websocket closed unexpectedly",
        ];
        for text in cases {
            assert!(is_retryable_assistant_error(&error_message(text)), "{text}");
        }
    }

    #[test]
    fn keeps_provider_limit_errors_non_retryable() {
        // These also match a retryable pattern ("429", "rate limit"), so the
        // exclusion list has to win.
        let cases = [
            "429 quota exceeded",
            "insufficient_quota",
            "GoUsageLimitError: rate limit reached",
            "Monthly usage limit reached, enable available balance",
            "503 billing issue on your account",
        ];
        for text in cases {
            assert!(
                !is_retryable_assistant_error(&error_message(text)),
                "{text}"
            );
        }
    }

    #[test]
    fn a_non_error_message_is_never_retryable() {
        let mut message = AssistantMessage::pending("a", "p", "m");
        message.stop_reason = StopReason::Stop;
        message.error_message = Some("overloaded".to_string());
        assert!(!is_retryable_assistant_error(&message));

        let mut errored = error_message("overloaded");
        errored.error_message = None;
        assert!(!is_retryable_assistant_error(&errored));
        assert!(!is_retryable_error_message(""));
    }

    // --- retry_assistant_call ------------------------------------------------

    #[derive(Default)]
    struct RecordingObserver {
        events: Mutex<Vec<String>>,
    }

    impl RetryObserver for RecordingObserver {
        fn on_retry_scheduled(&self, attempt: u32, max_attempts: u32, delay_ms: u64, _error: &str) {
            self.events.lock().unwrap().push(format!(
                "scheduled {attempt}/{max_attempts} in {delay_ms}ms"
            ));
        }
        fn on_retry_attempt_start(&self) {
            self.events.lock().unwrap().push("start".to_string());
        }
        fn on_retry_finished(&self, success: bool, attempt: u32, _final_error: Option<&str>) {
            self.events
                .lock()
                .unwrap()
                .push(format!("finished success={success} attempt={attempt}"));
        }
    }

    fn ok_message(text: &str) -> AssistantMessage {
        let mut message = AssistantMessage::pending("a", "p", "m");
        message.stop_reason = StopReason::Stop;
        message.content = vec![pi_core::content::AssistantContent::text(text)];
        message
    }

    const FAST: AssistantRetryPolicy = AssistantRetryPolicy {
        enabled: true,
        max_retries: 3,
        base_delay_ms: 0,
    };

    #[tokio::test]
    async fn returns_a_successful_response_without_retrying() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let observer = RecordingObserver::default();
        let result = retry_assistant_call(
            || {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    ok_message("ok")
                }
            },
            Some(&FAST),
            None,
            Some(&observer),
        )
        .await;
        assert_eq!(result.text(), "ok");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(observer.events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn does_not_retry_an_aborted_message() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let result = retry_assistant_call(
            || {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    let mut message = AssistantMessage::pending("a", "p", "m");
                    message.stop_reason = StopReason::Aborted;
                    message
                }
            },
            Some(&FAST),
            None,
            None,
        )
        .await;
        assert_eq!(result.stop_reason, StopReason::Aborted);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn does_not_retry_a_quota_error() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let result = retry_assistant_call(
            || {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    error_message("insufficient_quota")
                }
            },
            Some(&FAST),
            None,
            None,
        )
        .await;
        assert_eq!(result.stop_reason, StopReason::Error);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retries_a_transient_error_then_succeeds() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let observer = RecordingObserver::default();
        let result = retry_assistant_call(
            || {
                let c = c.clone();
                async move {
                    if c.fetch_add(1, Ordering::SeqCst) < 2 {
                        error_message("overloaded")
                    } else {
                        ok_message("recovered")
                    }
                }
            },
            Some(&FAST),
            None,
            Some(&observer),
        )
        .await;
        assert_eq!(result.text(), "recovered");
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        let events = observer.events.lock().unwrap().clone();
        assert_eq!(
            events,
            vec![
                "scheduled 1/3 in 0ms",
                "start",
                "scheduled 2/3 in 0ms",
                "start",
                "finished success=true attempt=2",
            ]
        );
    }

    #[tokio::test]
    async fn exhausts_the_budget_and_reports_failure() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let observer = RecordingObserver::default();
        let result = retry_assistant_call(
            || {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    error_message("overloaded")
                }
            },
            Some(&FAST),
            None,
            Some(&observer),
        )
        .await;
        assert_eq!(result.stop_reason, StopReason::Error);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            4,
            "initial call plus three retries"
        );
        assert!(observer
            .events
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .starts_with("finished success=false"));
    }

    #[tokio::test]
    async fn a_disabled_policy_returns_the_first_response() {
        let disabled = AssistantRetryPolicy {
            enabled: false,
            ..FAST
        };
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        for policy in [Some(&disabled), None] {
            calls.store(0, Ordering::SeqCst);
            let c2 = c.clone();
            let result = retry_assistant_call(
                || {
                    let c2 = c2.clone();
                    async move {
                        c2.fetch_add(1, Ordering::SeqCst);
                        error_message("overloaded")
                    }
                },
                policy,
                None,
                None,
            )
            .await;
            assert_eq!(result.stop_reason, StopReason::Error);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn an_abort_during_backoff_normalises_to_an_aborted_message() {
        let (handle, signal) = pi_core::options::AbortHandle::new();
        let policy = AssistantRetryPolicy {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 60_000,
        };
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            handle.abort();
        });
        let result = retry_assistant_call(
            || async { error_message("overloaded") },
            Some(&policy),
            Some(&signal),
            None,
        )
        .await;
        assert_eq!(result.stop_reason, StopReason::Aborted);
        assert_eq!(
            result.error_message, None,
            "the error text must not survive an abort"
        );
    }
}
