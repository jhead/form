//! In-memory recording adapter.
//!
//! Port of `.upstream/packages/telemetry/src/memory.ts`. Backend-neutral
//! reference implementation: deterministic ids, no timestamps, unbounded
//! process-local storage. Create a fresh instance per test or recording scope.

use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::noop::noop_span;
use crate::{
    Span, SpanAttributes, SpanOptions, SpanOutcome, SpanStatus, TelemetryContext, TelemetrySpan,
};

/// Detached snapshot of one recorded event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedTelemetryEvent {
    pub name: String,
    #[serde(default)]
    pub attributes: SpanAttributes,
}

/// Detached snapshot of one recorded span.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedTelemetrySpan {
    pub id: u64,
    pub parent_id: Option<u64>,
    pub name: String,
    #[serde(default)]
    pub attributes: SpanAttributes,
    #[serde(default)]
    pub events: Vec<RecordedTelemetryEvent>,
    pub status: SpanStatus,
    pub settled: bool,
    /// Order in which spans settled, starting at 1. `None` while open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_sequence: Option<u64>,
}

#[derive(Debug)]
struct SpanRecord {
    snapshot: RecordedTelemetrySpan,
    explicit_status: bool,
}

#[derive(Debug, Default)]
struct State {
    spans: Vec<SpanRecord>,
    next_span_id: u64,
    next_end_sequence: u64,
}

impl State {
    fn new() -> Self {
        Self {
            spans: Vec::new(),
            next_span_id: 1,
            next_end_sequence: 1,
        }
    }

    /// Span ids are handed out sequentially from 1 in push order, so the id
    /// doubles as the slot index.
    fn record_mut(&mut self, id: u64) -> Option<&mut SpanRecord> {
        self.spans
            .get_mut(usize::try_from(id).ok()?.checked_sub(1)?)
    }

    fn is_settled(&self, id: u64) -> bool {
        usize::try_from(id)
            .ok()
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| self.spans.get(index))
            .is_some_and(|record| record.snapshot.settled)
    }

    fn create(&mut self, parent_id: Option<u64>, options: SpanOptions) -> u64 {
        let id = self.next_span_id;
        self.next_span_id += 1;
        self.spans.push(SpanRecord {
            snapshot: RecordedTelemetrySpan {
                id,
                parent_id,
                name: options.name,
                attributes: options.attributes,
                events: Vec::new(),
                status: SpanStatus::Ok,
                settled: false,
                end_sequence: None,
            },
            explicit_status: false,
        });
        id
    }

    fn settle(&mut self, id: u64, outcome: SpanOutcome) {
        let end_sequence = self.next_end_sequence;
        let Some(record) = self.record_mut(id) else {
            return;
        };
        if record.snapshot.settled {
            return;
        }
        if let SpanOutcome::Failure(error) = outcome {
            // An explicit status is never overwritten by the automatic one.
            if !record.explicit_status {
                record.snapshot.status = SpanStatus::Error { error };
            }
        }
        record.snapshot.settled = true;
        record.snapshot.end_sequence = Some(end_sequence);
        self.next_end_sequence += 1;
    }
}

/// Backend-neutral reference adapter that records spans in process memory.
///
/// Cloning shares the recording state, so one clone can be handed out as
/// `Arc<dyn TelemetryContext>` while another reads [`InMemoryTelemetryContext::spans`].
#[derive(Debug, Clone)]
pub struct InMemoryTelemetryContext {
    state: Arc<Mutex<State>>,
}

impl InMemoryTelemetryContext {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State::new())),
        }
    }

    /// Detached snapshots in span-start order, open spans included.
    pub fn spans(&self) -> Vec<RecordedTelemetrySpan> {
        self.state
            .lock()
            .spans
            .iter()
            .map(|record| record.snapshot.clone())
            .collect()
    }

    /// This adapter as a shared context handle.
    pub fn as_context(&self) -> Arc<dyn TelemetryContext> {
        Arc::new(self.clone())
    }
}

impl Default for InMemoryTelemetryContext {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetryContext for InMemoryTelemetryContext {
    fn start_span(&self, options: SpanOptions) -> Span {
        start_span(&self.state, None, options)
    }
}

fn start_span(state: &Arc<Mutex<State>>, parent_id: Option<u64>, options: SpanOptions) -> Span {
    let id = {
        let mut locked = state.lock();
        // A child of a settled span is not recorded; upstream falls back to the
        // no-op context so the caller's callback still runs.
        if parent_id.is_some_and(|parent| locked.is_settled(parent)) {
            return noop_span();
        }
        locked.create(parent_id, options)
    };
    Span::new(Arc::new(InMemorySpan {
        state: state.clone(),
        id,
    }))
}

#[derive(Debug)]
struct InMemorySpan {
    state: Arc<Mutex<State>>,
    id: u64,
}

impl TelemetrySpan for InMemorySpan {
    fn start_span(&self, options: SpanOptions) -> Span {
        start_span(&self.state, Some(self.id), options)
    }

    fn add_event(&self, name: &str, attributes: SpanAttributes) {
        let mut state = self.state.lock();
        let Some(record) = state.record_mut(self.id) else {
            return;
        };
        if record.snapshot.settled {
            return;
        }
        record.snapshot.events.push(RecordedTelemetryEvent {
            name: name.to_string(),
            attributes,
        });
    }

    fn set_attributes(&self, attributes: SpanAttributes) {
        let mut state = self.state.lock();
        let Some(record) = state.record_mut(self.id) else {
            return;
        };
        if record.snapshot.settled {
            return;
        }
        record.snapshot.attributes.extend(attributes);
    }

    fn set_status(&self, status: SpanStatus) {
        let mut state = self.state.lock();
        let Some(record) = state.record_mut(self.id) else {
            return;
        };
        if record.snapshot.settled {
            return;
        }
        record.snapshot.status = status;
        record.explicit_status = true;
    }

    fn end(&self, outcome: SpanOutcome) {
        self.state.lock().settle(self.id, outcome);
    }
}
