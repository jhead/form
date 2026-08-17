//! Thin SSE transport built on `pi_http::HttpClient`.
//!
//! `pi_http::HttpClient::post_sse` is almost what these adapters need, but two
//! upstream behaviours need control it does not expose yet:
//!
//! 1. Both Mistral and pi-messages format their error message from the **raw**
//!    response body (`Mistral API error (403): {"message":…}`), and
//!    `HttpError::Status` only carries the already-extracted message plus the
//!    parsed JSON. So the transport here returns the untouched body text.
//! 2. `pi_http::sse_stream` decodes each transport chunk with
//!    `String::from_utf8_lossy`, which mangles a multi-byte character split
//!    across two chunks. Upstream has an explicit regression test for that, so
//!    the byte stream is run through an incremental UTF-8 decoder before it
//!    reaches the SSE parser.
//!
//! Both are noted in the workstream report as `pi-http` follow-ups; the parsing
//! itself still uses `pi_http::SseParser`.

use std::collections::{BTreeMap, VecDeque};
use std::pin::Pin;
use std::time::Duration;

use bytes::Bytes;
use futures_core::Stream;
use futures_util::StreamExt;
use pi_core::options::AbortSignal;
use pi_http::{HttpClient, SseEvent, SseParser};
use serde_json::Value;
use tokio::time::Instant;

pub type OwnedHeaders = BTreeMap<String, String>;

/// Everything that can go wrong before or during an SSE response.
#[derive(Debug, Clone)]
pub enum TransportFailure {
    Transport(String),
    Timeout,
    Aborted,
    Status {
        status: u16,
        status_text: String,
        headers: OwnedHeaders,
        body: String,
    },
}

impl TransportFailure {
    pub fn message(&self) -> String {
        match self {
            TransportFailure::Transport(message) => message.clone(),
            TransportFailure::Timeout => "The operation timed out".to_string(),
            TransportFailure::Aborted => "Request was aborted".to_string(),
            TransportFailure::Status { status, body, .. } => format!("HTTP {status}: {body}"),
        }
    }

    pub fn is_aborted(&self) -> bool {
        matches!(self, TransportFailure::Aborted)
    }
}

/// A POST that expects `text/event-stream` back.
pub struct SseRequest {
    pub url: String,
    pub headers: OwnedHeaders,
    pub body: Value,
    pub signal: Option<AbortSignal>,
    /// Deadline for the whole exchange, headers *and* body, matching upstream's
    /// `AbortSignal.timeout()` which stays attached to the body reader.
    pub timeout: Option<Duration>,
}

/// A live SSE response. Events are pulled one at a time so the caller can
/// interleave abort checks.
pub struct SseStreamResponse {
    pub status: u16,
    pub headers: OwnedHeaders,
    body: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
    parser: SseParser,
    pending: VecDeque<SseEvent>,
    decoder: Utf8Decoder,
    signal: Option<AbortSignal>,
    deadline: Option<Instant>,
    finished: bool,
}

impl std::fmt::Debug for SseStreamResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SseStreamResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

pub async fn post_sse(
    client: &HttpClient,
    request: SseRequest,
) -> Result<SseStreamResponse, TransportFailure> {
    let SseRequest {
        url,
        headers,
        body,
        signal,
        timeout,
    } = request;

    if signal.as_ref().is_some_and(AbortSignal::is_aborted) {
        return Err(TransportFailure::Aborted);
    }

    let deadline = timeout.map(|duration| Instant::now() + duration);
    let mut builder = client.raw().post(&url);
    for (name, value) in &headers {
        builder = builder.header(name, value);
    }
    builder = builder.json(&body);

    let response = await_with_limits(builder.send(), signal.as_ref(), deadline)
        .await?
        .map_err(|error| TransportFailure::Transport(error.to_string()))?;

    let status = response.status();
    let status_text = status.canonical_reason().unwrap_or("").to_string();
    let response_headers = collect_headers(response.headers());

    if !status.is_success() {
        let body = await_with_limits(response.text(), signal.as_ref(), deadline)
            .await?
            .unwrap_or_default();
        return Err(TransportFailure::Status {
            status: status.as_u16(),
            status_text,
            headers: response_headers,
            body,
        });
    }

    Ok(SseStreamResponse {
        status: status.as_u16(),
        headers: response_headers,
        body: Box::pin(response.bytes_stream()),
        parser: SseParser::new(),
        pending: VecDeque::new(),
        decoder: Utf8Decoder::default(),
        signal,
        deadline,
        finished: false,
    })
}

impl SseStreamResponse {
    /// Next event, or `None` at end of stream.
    pub async fn next_event(&mut self) -> Option<Result<SseEvent, TransportFailure>> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(Ok(event));
            }
            if self.finished {
                return None;
            }
            if self.signal.as_ref().is_some_and(AbortSignal::is_aborted) {
                self.finished = true;
                return Some(Err(TransportFailure::Aborted));
            }

            let chunk = match await_with_limits(
                self.body.next(),
                self.signal.as_ref(),
                self.deadline,
            )
            .await
            {
                Ok(chunk) => chunk,
                Err(failure) => {
                    self.finished = true;
                    return Some(Err(failure));
                }
            };

            match chunk {
                Some(Ok(bytes)) => {
                    let text = self.decoder.push(&bytes);
                    self.pending.extend(self.parser.push(&text));
                }
                Some(Err(error)) => {
                    self.finished = true;
                    return Some(Err(TransportFailure::Transport(error.to_string())));
                }
                None => {
                    self.finished = true;
                    let text = self.decoder.finish();
                    if !text.is_empty() {
                        self.pending.extend(self.parser.push(&text));
                    }
                    if let Some(event) = self.parser.finish() {
                        self.pending.push_back(event);
                    }
                }
            }
        }
    }
}

/// Run `future` under the abort signal and the request deadline.
async fn await_with_limits<F: std::future::Future>(
    future: F,
    signal: Option<&AbortSignal>,
    deadline: Option<Instant>,
) -> Result<F::Output, TransportFailure> {
    match (signal, deadline) {
        (Some(signal), Some(deadline)) => tokio::select! {
            biased;
            _ = signal.aborted() => Err(TransportFailure::Aborted),
            _ = tokio::time::sleep_until(deadline) => Err(TransportFailure::Timeout),
            output = future => Ok(output),
        },
        (Some(signal), None) => tokio::select! {
            biased;
            _ = signal.aborted() => Err(TransportFailure::Aborted),
            output = future => Ok(output),
        },
        (None, Some(deadline)) => tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => Err(TransportFailure::Timeout),
            output = future => Ok(output),
        },
        (None, None) => Ok(future.await),
    }
}

pub fn collect_headers(headers: &reqwest::header::HeaderMap) -> OwnedHeaders {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_lowercase(), value.to_string()))
        })
        .collect()
}

/// Streaming UTF-8 decoder that holds back an incomplete trailing sequence
/// instead of replacing it, so a character split across chunks survives.
#[derive(Debug, Default)]
struct Utf8Decoder {
    tail: Vec<u8>,
}

impl Utf8Decoder {
    fn push(&mut self, chunk: &[u8]) -> String {
        self.tail.extend_from_slice(chunk);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.tail) {
                Ok(text) => {
                    out.push_str(text);
                    self.tail.clear();
                    return out;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    out.push_str(std::str::from_utf8(&self.tail[..valid]).expect("valid prefix"));
                    match error.error_len() {
                        // Truly invalid bytes: drop them and keep decoding.
                        Some(len) => {
                            out.push(char::REPLACEMENT_CHARACTER);
                            self.tail.drain(..valid + len);
                        }
                        // Incomplete trailing sequence: wait for the next chunk.
                        None => {
                            self.tail.drain(..valid);
                            return out;
                        }
                    }
                }
            }
        }
    }

    fn finish(&mut self) -> String {
        if self.tail.is_empty() {
            return String::new();
        }
        let text = String::from_utf8_lossy(&self.tail).into_owned();
        self.tail.clear();
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_characters_split_across_chunks() {
        let bytes = "héllo 🌍".as_bytes().to_vec();
        let mut decoder = Utf8Decoder::default();
        let mut out = String::new();
        for byte in bytes {
            out.push_str(&decoder.push(&[byte]));
        }
        out.push_str(&decoder.finish());
        assert_eq!(out, "héllo 🌍");
    }

    #[test]
    fn replaces_invalid_bytes() {
        let mut decoder = Utf8Decoder::default();
        assert_eq!(decoder.push(&[b'a', 0xff, b'b']), "a\u{fffd}b");
    }
}
