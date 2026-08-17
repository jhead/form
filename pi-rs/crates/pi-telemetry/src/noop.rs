//! The no-op context.
//!
//! Port of `.upstream/packages/telemetry/src/noop.ts`. One shared inert span
//! serves as both the context and every span started from it, so the whole
//! thing is a single allocation for the life of the process and nesting costs
//! an `Arc` clone.

use std::sync::{Arc, OnceLock};

use crate::{
    Span, SpanAttributes, SpanOptions, SpanOutcome, SpanStatus, TelemetryContext, TelemetrySpan,
};

/// Passive context used when an application does not provide one. It does not
/// inspect or retain names, attributes, events or statuses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoopTelemetryContext;

impl TelemetryContext for NoopTelemetryContext {
    fn start_span(&self, _options: SpanOptions) -> Span {
        noop_span()
    }
}

impl TelemetrySpan for NoopTelemetryContext {
    fn start_span(&self, _options: SpanOptions) -> Span {
        noop_span()
    }

    fn add_event(&self, _name: &str, _attributes: SpanAttributes) {}

    fn set_attributes(&self, _attributes: SpanAttributes) {}

    fn set_status(&self, _status: SpanStatus) {}

    fn end(&self, _outcome: SpanOutcome) {}
}

fn singleton() -> &'static Arc<NoopTelemetryContext> {
    static NOOP: OnceLock<Arc<NoopTelemetryContext>> = OnceLock::new();
    NOOP.get_or_init(|| Arc::new(NoopTelemetryContext))
}

/// The shared inert span. Every call returns a handle to the same sink, which
/// is what upstream's single frozen `noopTelemetrySpan` guarantees.
pub fn noop_span() -> Span {
    Span::new(singleton().clone())
}

/// The shared no-op context, upstream's `NOOP_TELEMETRY_CONTEXT`.
pub fn noop_telemetry_context() -> Arc<dyn TelemetryContext> {
    singleton().clone()
}
