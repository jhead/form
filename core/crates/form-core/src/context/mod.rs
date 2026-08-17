//! Context-window accounting (F10).
//!
//! **Owner: W4** (`docs/specs/04-catalog-settings.md` §3).
//!
//! `estimate_tokens` is shared with the stub harness so the ring and the usage figures
//! agree. TODO(W4): per-image cost from dimensions, tool-schema segment, and the fixture
//! test over a real transcript.

use crate::catalog::Model;
use crate::protocol::{
    ContextSegment, ContextUsage, Cost, EntryKind, Message, SegmentKind, Session,
};

/// Rough but *consistent* estimate. Consistency between the harness, the ring and the
/// dashboard matters more than absolute accuracy while the harness is a stub.
pub fn estimate_tokens(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    let chars = text.chars().count() as u64;
    // CJK runs are denser per character; approximate by weighting non-ASCII.
    let non_ascii = text.chars().filter(|c| !c.is_ascii()).count() as u64;
    ((chars + non_ascii) as f64 / 4.0).ceil() as u64
}

pub fn context_usage(session: &Session, model: Option<&Model>) -> ContextUsage {
    let mut transcript = 0u64;
    let mut cost = Cost::default();

    for entry in &session.entries {
        if let EntryKind::Message { message } = &entry.kind {
            match message {
                Message::User(m) => transcript += estimate_tokens(&m.content.to_text()),
                Message::Assistant(m) => {
                    transcript += estimate_tokens(&m.text());
                    cost.input += m.usage.cost.input;
                    cost.output += m.usage.cost.output;
                    cost.cache_read += m.usage.cost.cache_read;
                    cost.cache_write += m.usage.cost.cache_write;
                    cost.total += m.usage.cost.total;
                }
                Message::ToolResult(m) => {
                    for c in &m.content {
                        if let Some(t) = c.as_text() {
                            transcript += estimate_tokens(&t.text);
                        }
                    }
                }
            }
        }
    }

    let system = 0; // TODO(W4): count the resolved system prompt.
    let tools = 0; // TODO(W4): count serialized tool schemas.
    let attachments = 0; // TODO(W4): derive from attachment dimensions.
    let output_reserve = model.map(|m| m.max_output).unwrap_or(0);
    let total = model.map(|m| m.context_window).unwrap_or(0);
    let used = (system + tools + transcript + attachments + output_reserve).min(total.max(1));

    ContextUsage {
        session_id: session.summary.id.clone(),
        used,
        total,
        segments: vec![
            ContextSegment {
                kind: SegmentKind::System,
                tokens: system,
            },
            ContextSegment {
                kind: SegmentKind::Tools,
                tokens: tools,
            },
            ContextSegment {
                kind: SegmentKind::Transcript,
                tokens: transcript,
            },
            ContextSegment {
                kind: SegmentKind::Attachments,
                tokens: attachments,
            },
            ContextSegment {
                kind: SegmentKind::OutputReserve,
                tokens: output_reserve,
            },
        ],
        cost,
        message_count: session.entries.len() as u64,
    }
}
