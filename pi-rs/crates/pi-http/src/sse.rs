//! Server-sent events parsing.
//!
//! Port of the SSE handling in `packages/ai/src/utils/event-stream.ts` and the
//! per-adapter `parseSSE` helpers. Follows the WHATWG event-stream rules the
//! providers actually rely on: `event:`, `data:` (multi-line, newline joined),
//! `id:`, `retry:`, comments (`:`), and dispatch on a blank line.
//!
//! ## Chunking
//!
//! A provider's chunk boundaries are arbitrary. They land mid-line, mid-CRLF,
//! and mid-UTF-8-sequence, so the parser buffers **bytes** and decodes only up
//! to the last complete UTF-8 sequence. Decoding each chunk independently —
//! the obvious implementation — turns any multi-byte character split across a
//! boundary into U+FFFD, silently corrupting text deltas and tool arguments.
//!
//! Line terminators are `\r\n`, `\n` *and* a lone `\r`, per spec. A trailing
//! `\r` at the end of the buffer is held back rather than dispatched, because
//! the next chunk may begin with the `\n` that completes it.

use bytes::Bytes;
use futures_core::Stream;
use futures_util::StreamExt;

/// One dispatched server-sent event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` field, empty when the provider omits it.
    pub event: String,
    /// The joined `data:` payload.
    pub data: String,
    pub id: Option<String>,
    pub retry_ms: Option<u64>,
    /// The event's field lines exactly as received, newline-joined and without
    /// the terminating blank line. Comments and unknown fields are included.
    ///
    /// Adapters use this to reproduce upstream's `raw=…` parse-error text: once
    /// `data` has been split and rejoined, the original framing is gone, and a
    /// malformed-payload diagnostic that shows the reassembled form sends people
    /// looking for a bug in the wrong place.
    pub raw: String,
}

impl SseEvent {
    /// Whether this is the OpenAI-style terminator.
    pub fn is_done_sentinel(&self) -> bool {
        self.data.trim() == "[DONE]"
    }

    /// Parse the `data` payload as JSON.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_str(&self.data)
    }
}

/// Incremental SSE parser. Feed it bytes, drain events.
#[derive(Debug, Default)]
pub struct SseParser {
    /// Undecoded bytes: a partial UTF-8 sequence, or a trailing lone `\r`.
    bytes: Vec<u8>,
    /// Decoded text not yet split into a complete line.
    buffer: String,
    /// Byte offset in `buffer` already known to hold no line terminator.
    /// Without this, a large event delivered in many small chunks re-scans the
    /// whole buffer per chunk, which is quadratic in the event size.
    scanned: usize,
    event: String,
    data: Vec<String>,
    id: Option<String>,
    retry_ms: Option<u64>,
    /// Field lines of the event being assembled, verbatim.
    raw: Vec<String>,
    /// A leading UTF-8 BOM is stripped once, per spec.
    bom_checked: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a text chunk and return every event completed by it.
    pub fn push(&mut self, chunk: &str) -> Vec<SseEvent> {
        self.push_bytes(chunk.as_bytes())
    }

    /// Feed raw bytes and return every event completed by them.
    ///
    /// Safe to call with arbitrary splits, including one byte at a time.
    pub fn push_bytes(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.bytes.extend_from_slice(chunk);
        self.decode_ready();
        self.drain_lines(false)
    }

    /// Flush a trailing event when the transport closes without a blank line.
    ///
    /// Providers do end a stream on the last `data:` line, so the pending event
    /// is dispatched rather than discarded. At most one event can be pending,
    /// because [`push_bytes`](Self::push_bytes) has already drained every
    /// complete line.
    pub fn finish(&mut self) -> Option<SseEvent> {
        // Any bytes still buffered are an incomplete UTF-8 sequence or a lone
        // `\r`; decode lossily so a truncated character does not eat the line.
        if !self.bytes.is_empty() {
            let rest = std::mem::take(&mut self.bytes);
            self.buffer.push_str(&String::from_utf8_lossy(&rest));
        }
        let mut last = None;
        for event in self.drain_lines(true) {
            last = Some(event);
        }
        last.or_else(|| self.take_event())
    }

    /// Move every complete UTF-8 sequence from `bytes` into `buffer`.
    fn decode_ready(&mut self) {
        if self.bytes.is_empty() {
            return;
        }
        let complete = complete_utf8_len(&self.bytes);
        if complete == 0 {
            return;
        }
        let (head, tail) = self.bytes.split_at(complete);
        // `complete` is chosen so this is always valid.
        let text = String::from_utf8_lossy(head).into_owned();
        self.bytes = tail.to_vec();
        self.buffer.push_str(&text);

        if !self.bom_checked {
            self.bom_checked = true;
            if let Some(stripped) = self.buffer.strip_prefix('\u{feff}') {
                self.buffer = stripped.to_string();
            }
        }
    }

    /// Split `buffer` into lines and feed them through. When `final_chunk` is
    /// false, a trailing `\r` is held back in case `\n` follows.
    fn drain_lines(&mut self, final_chunk: bool) -> Vec<SseEvent> {
        let mut out = Vec::new();
        loop {
            let from = self.scanned.min(self.buffer.len());
            let Some(position) = self.buffer[from..].find(['\n', '\r']).map(|p| p + from) else {
                break;
            };
            let is_cr = self.buffer.as_bytes()[position] == b'\r';
            let after_cr = position + 1;
            if is_cr && !final_chunk && after_cr == self.buffer.len() {
                // Could be the first half of a CRLF; wait for more bytes.
                self.scanned = position;
                return out;
            }
            let terminator_len = if is_cr && self.buffer.as_bytes().get(after_cr) == Some(&b'\n') {
                2
            } else {
                1
            };
            let line: String = self.buffer[..position].to_string();
            self.buffer.drain(..position + terminator_len);
            self.scanned = 0;
            if let Some(event) = self.push_line(&line) {
                out.push(event);
            }
        }
        self.scanned = self.buffer.len();
        if final_chunk && !self.buffer.is_empty() {
            self.scanned = 0;
            // A final line with no terminator: providers do truncate here, and
            // dropping it would discard the last delta.
            let line = std::mem::take(&mut self.buffer);
            if let Some(event) = self.push_line(&line) {
                out.push(event);
            }
        }
        out
    }

    fn push_line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            return self.take_event();
        }
        self.raw.push(line.to_string());
        if line.starts_with(':') {
            return None; // comment / keepalive
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = value.to_string(),
            "data" => self.data.push(value.to_string()),
            "id" => self.id = Some(value.to_string()),
            // `retry` is a stream-level setting, so it is not cleared on dispatch.
            "retry" => self.retry_ms = value.parse().ok(),
            _ => {}
        }
        None
    }

    fn take_event(&mut self) -> Option<SseEvent> {
        if self.data.is_empty() && self.event.is_empty() {
            self.id = None;
            // A comment-only block (a keepalive) leaves no event, so its raw
            // lines must not leak into the next one.
            self.raw.clear();
            return None;
        }
        Some(SseEvent {
            event: std::mem::take(&mut self.event),
            data: std::mem::take(&mut self.data).join("\n"),
            id: self.id.take(),
            retry_ms: self.retry_ms,
            raw: std::mem::take(&mut self.raw).join("\n"),
        })
    }
}

/// Length of the longest prefix of `bytes` that is complete, valid UTF-8.
///
/// Returns the full length when the bytes are valid, and otherwise the offset
/// of the error — except when the error is a *truncated* final sequence, where
/// the valid prefix ends just before it.
fn complete_utf8_len(bytes: &[u8]) -> usize {
    match std::str::from_utf8(bytes) {
        Ok(_) => bytes.len(),
        Err(err) => err.valid_up_to(),
    }
}

/// Adapt a byte stream into a stream of [`SseEvent`]s.
///
/// Transport errors terminate the stream; adapters convert that into a protocol
/// `Error` event, so no error type leaks here.
pub fn sse_stream<S, E>(bytes: S) -> impl Stream<Item = Result<SseEvent, crate::HttpError>> + Send
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let mut parser = SseParser::new();
    let mut pending: std::collections::VecDeque<SseEvent> = std::collections::VecDeque::new();
    let mut bytes = Box::pin(bytes);
    let mut finished = false;

    futures_util::stream::poll_fn(move |cx| {
        use std::task::Poll;
        loop {
            if let Some(event) = pending.pop_front() {
                return Poll::Ready(Some(Ok(event)));
            }
            if finished {
                return Poll::Ready(None);
            }
            match bytes.poll_next_unpin(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(chunk))) => {
                    pending.extend(parser.push_bytes(&chunk));
                }
                Poll::Ready(Some(Err(err))) => {
                    finished = true;
                    return Poll::Ready(Some(Err(crate::HttpError::Transport(err.to_string()))));
                }
                Poll::Ready(None) => {
                    finished = true;
                    if let Some(event) = parser.finish() {
                        pending.push_back(event);
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `input` in every chunk size from 1 byte up to the whole thing, and
    /// assert the events are identical each time. This is the property that
    /// matters: chunk boundaries must be invisible.
    fn events_for_every_chunking(input: &str) -> Vec<SseEvent> {
        let bytes = input.as_bytes();
        let mut reference: Option<Vec<SseEvent>> = None;
        for size in 1..=bytes.len().max(1) {
            let mut parser = SseParser::new();
            let mut events = Vec::new();
            for chunk in bytes.chunks(size) {
                events.extend(parser.push_bytes(chunk));
            }
            if let Some(event) = parser.finish() {
                events.push(event);
            }
            match &reference {
                None => reference = Some(events),
                Some(expected) => assert_eq!(&events, expected, "chunk size {size} diverged"),
            }
        }
        reference.unwrap_or_default()
    }

    #[test]
    fn parses_named_events_and_multiline_data() {
        let mut p = SseParser::new();
        let events = p.push("event: message_start\ndata: {\"a\":1}\ndata: {\"b\":2}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "message_start");
        assert_eq!(events[0].data, "{\"a\":1}\n{\"b\":2}");
    }

    #[test]
    fn handles_split_chunks_and_crlf() {
        let mut p = SseParser::new();
        assert!(p.push("data: hel").is_empty());
        let events = p.push("lo\r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello");
    }

    #[test]
    fn ignores_comments() {
        let mut p = SseParser::new();
        assert!(p.push(": ping\n\n").is_empty());
    }

    #[test]
    fn detects_done_sentinel() {
        let mut p = SseParser::new();
        let events = p.push("data: [DONE]\n\n");
        assert!(events[0].is_done_sentinel());
    }

    #[test]
    fn parses_id_and_retry_fields() {
        let mut p = SseParser::new();
        let events = p.push("id: 42\nretry: 3000\ndata: x\n\n");
        assert_eq!(events[0].id.as_deref(), Some("42"));
        assert_eq!(events[0].retry_ms, Some(3000));
    }

    #[test]
    fn a_value_with_no_leading_space_is_kept_verbatim() {
        let mut p = SseParser::new();
        let events = p.push("data:{\"a\":1}\n\n");
        assert_eq!(events[0].data, "{\"a\":1}");
        // Only one leading space is stripped.
        let mut p = SseParser::new();
        let events = p.push("data:  padded\n\n");
        assert_eq!(events[0].data, " padded");
    }

    #[test]
    fn a_field_with_no_colon_is_treated_as_an_empty_value() {
        let mut p = SseParser::new();
        // `data` alone contributes an empty line to the payload.
        let events = p.push("data: a\ndata\ndata: b\n\n");
        assert_eq!(events[0].data, "a\n\nb");
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let mut p = SseParser::new();
        let events = p.push("foo: bar\ndata: x\n\n");
        assert_eq!(events[0].data, "x");
    }

    #[test]
    fn the_raw_lines_are_retained_verbatim() {
        let mut p = SseParser::new();
        let events = p.push("event: delta\ndata: {\"a\":1}\ndata: {\"b\":2}\n\n");
        // Multi-line data is joined for `data` but preserved as sent in `raw`.
        assert_eq!(events[0].data, "{\"a\":1}\n{\"b\":2}");
        assert_eq!(
            events[0].raw,
            "event: delta\ndata: {\"a\":1}\ndata: {\"b\":2}"
        );
    }

    #[test]
    fn raw_lines_do_not_leak_between_events() {
        let mut p = SseParser::new();
        p.push("event: first\ndata: 1\n\n");
        // A keepalive comment between events must not attach to the next one.
        p.push(": ping\n\n");
        let events = p.push("data: 2\n\n");
        assert_eq!(events[0].raw, "data: 2");
    }

    #[test]
    fn a_leading_bom_is_stripped_once() {
        let mut p = SseParser::new();
        let events = p.push("\u{feff}data: x\n\n");
        assert_eq!(events[0].data, "x");
        // A later BOM is data, not a marker.
        let events = p.push("data: \u{feff}y\n\n");
        assert_eq!(events[0].data, "\u{feff}y");
    }

    #[test]
    fn a_lone_cr_terminates_a_line() {
        let mut p = SseParser::new();
        // The trailing CR is held back in case a LF follows, so nothing is
        // dispatched until the next chunk resolves it.
        assert!(p.push("data: a\r\r").is_empty());
        let events = p.push("data: b\n\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "a");
        assert_eq!(events[1].data, "b");

        // At end of stream there is nothing more to wait for.
        let mut p = SseParser::new();
        assert!(p.push("data: a\r\r").is_empty());
        assert_eq!(p.finish().map(|e| e.data), Some("a".to_string()));
    }

    #[test]
    fn a_cr_at_a_chunk_boundary_does_not_split_a_crlf() {
        let mut p = SseParser::new();
        assert!(
            p.push("data: a\r").is_empty(),
            "a trailing CR must be held back"
        );
        let events = p.push("\ndata: b\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "a\nb");
    }

    #[test]
    fn multibyte_characters_survive_any_byte_split() {
        // A three-byte character and a four-byte emoji, split at every offset.
        let events = events_for_every_chunking("data: héllo 🙈 wörld 日本語\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "héllo 🙈 wörld 日本語");
    }

    #[test]
    fn an_anthropic_style_stream_survives_any_byte_split() {
        let stream = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n",
            "\n",
            ": ping\n",
            "\n",
            "event: content_block_delta\n",
            "data: {\"delta\":{\"text\":\"héllo 🙈\"}}\r\n",
            "\r\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n",
            "\n",
        );
        let events = events_for_every_chunking(stream);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event, "message_start");
        assert_eq!(events[1].event, "content_block_delta");
        assert!(events[1].data.contains("héllo 🙈"));
        assert_eq!(events[2].event, "message_stop");
    }

    #[test]
    fn an_openai_style_stream_survives_any_byte_split() {
        let stream = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"b\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let events = events_for_every_chunking(stream);
        assert_eq!(events.len(), 3);
        assert!(events[2].is_done_sentinel());
    }

    #[test]
    fn a_stream_ending_without_a_blank_line_still_dispatches() {
        let mut p = SseParser::new();
        assert!(p.push("event: done\ndata: {\"a\":1}\n").is_empty());
        let event = p.finish().expect("pending event should flush");
        assert_eq!(event.event, "done");
        assert_eq!(event.data, "{\"a\":1}");
    }

    #[test]
    fn a_stream_ending_mid_line_still_dispatches_what_it_has() {
        let mut p = SseParser::new();
        assert!(p.push("data: {\"a\":1").is_empty());
        let event = p.finish().expect("truncated line should flush");
        assert_eq!(event.data, "{\"a\":1");
    }

    #[test]
    fn finishing_an_empty_parser_yields_nothing() {
        assert!(SseParser::new().finish().is_none());
        let mut p = SseParser::new();
        p.push("data: x\n\n");
        assert!(
            p.finish().is_none(),
            "a dispatched event must not be replayed"
        );
    }

    #[test]
    fn a_truncated_utf8_sequence_at_eof_does_not_corrupt_earlier_text() {
        let mut p = SseParser::new();
        let mut bytes = b"data: ok".to_vec();
        bytes.push(0xE6); // first byte of a three-byte sequence, then EOF
        assert!(p.push_bytes(&bytes).is_empty());
        let event = p.finish().expect("flushes");
        assert!(event.data.starts_with("ok"), "{:?}", event.data);
    }

    #[test]
    fn an_event_field_with_no_data_still_dispatches() {
        // Providers do send bare named events; dropping them would lose state
        // transitions even though the WHATWG spec would discard them.
        let mut p = SseParser::new();
        let events = p.push("event: ping\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "ping");
        assert_eq!(events[0].data, "");
    }

    #[test]
    fn consecutive_blank_lines_do_not_emit_empty_events() {
        let mut p = SseParser::new();
        assert!(p.push("\n\n\n\n").is_empty());
        let events = p.push("data: x\n\n\n\n");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn the_event_name_resets_between_events() {
        let mut p = SseParser::new();
        let first = p.push("event: a\ndata: 1\n\n");
        let second = p.push("data: 2\n\n");
        assert_eq!(first[0].event, "a");
        assert_eq!(second[0].event, "", "event name leaked into the next event");
    }

    #[test]
    fn a_very_long_single_event_is_assembled_correctly() {
        // 512 KiB of payload delivered in 1 KiB chunks: guards the buffer
        // management as much as the parsing.
        let payload = "x".repeat(512 * 1024);
        let stream = format!("data: {payload}\n\n");
        let mut p = SseParser::new();
        let mut events = Vec::new();
        for chunk in stream.as_bytes().chunks(1024) {
            events.extend(p.push_bytes(chunk));
        }
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data.len(), payload.len());
    }

    #[tokio::test]
    async fn the_stream_adapter_flushes_a_trailing_event_and_reports_errors() {
        use futures_util::stream;

        let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
            Ok(Bytes::from_static(b"data: a\n\ndata: ")),
            Ok(Bytes::from_static(b"b\n")),
        ];
        let events: Vec<_> = sse_stream(stream::iter(chunks)).collect().await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].as_ref().unwrap().data, "a");
        assert_eq!(events[1].as_ref().unwrap().data, "b");

        let failing: Vec<Result<Bytes, std::io::Error>> = vec![
            Ok(Bytes::from_static(b"data: a\n\n")),
            Err(std::io::Error::other("connection reset")),
        ];
        let events: Vec<_> = sse_stream(stream::iter(failing)).collect().await;
        assert_eq!(events.len(), 2);
        assert!(matches!(events[1], Err(crate::HttpError::Transport(_))));
    }
}
