//! The adapter conformance suite.
//!
//! Port of `.upstream/packages/telemetry/src/testing/conformance.ts`. The cases
//! check the observable semantics every adapter must have: single synchronous
//! admission, result and failure identity, automatic and explicit status,
//! attribute merging, event ordering, inert post-settlement calls, and
//! parentage with deterministic end ordering.
//!
//! Two upstream groups do not survive the port. Its "passivity" cases wrap
//! payloads in `Proxy` objects whose getters throw, to prove an adapter cannot
//! be broken by a hostile attribute bag; in Rust a [`SpanAttributes`] map
//! cannot fail to be read, so the equivalent hazard does not exist. The group
//! is kept with a degenerate-payload case in its place. Upstream's concurrent
//! parentage case gates two child promises against each other; here the same
//! observable shape — two children of one parent settling in a known order —
//! comes from interleaved [`SpanGuard`] lifetimes, which needs no runtime.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use super::types::{
    find_span, BoxFuture, TelemetryAdapterConformanceCase, TelemetryAdapterFixture,
    TelemetryAdapterFixtureFactory,
};
use crate::{
    span_attributes, InMemoryTelemetryContext, RecordedTelemetryEvent, SpanAttributes, SpanError,
    SpanOptions, SpanStatus, TelemetryContext, TelemetryContextExt,
};

/// Failure value used by the suite. Adapters must preserve it unchanged and
/// derive `{ name: "ConformanceError", message }` for the automatic status.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ConformanceError(&'static str);

impl std::fmt::Display for ConformanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

type CaseBody = for<'a> fn(&'a dyn TelemetryAdapterFixture) -> BoxFuture<'a, ()>;

/// Wraps an `async fn(&dyn TelemetryAdapterFixture)` as a [`CaseBody`].
macro_rules! case {
    ($factory:expr, $group:expr, $name:expr, $body:ident) => {{
        fn erased(fixture: &dyn TelemetryAdapterFixture) -> BoxFuture<'_, ()> {
            Box::pin($body(fixture))
        }
        build_case($factory, $group, $name, erased)
    }};
}

fn build_case(
    factory: &Arc<dyn TelemetryAdapterFixtureFactory>,
    group: &'static str,
    name: &'static str,
    body: CaseBody,
) -> TelemetryAdapterConformanceCase {
    let factory = factory.clone();
    TelemetryAdapterConformanceCase::new(
        group,
        name,
        Arc::new(move || {
            let factory = factory.clone();
            Box::pin(async move {
                let fixture = factory.create().await;
                body(fixture.as_ref()).await;
                fixture.close().await;
            })
        }),
    )
}

/// Creates runner-independent cases for the telemetry adapter contract.
pub fn telemetry_adapter_conformance(
    factory: Arc<dyn TelemetryAdapterFixtureFactory>,
) -> Vec<TelemetryAdapterConformanceCase> {
    vec![
        case!(
            &factory,
            "callback lifecycle",
            "admits once synchronously and preserves the result",
            admits_once_synchronously
        ),
        case!(
            &factory,
            "callback lifecycle",
            "preserves failure values and records an error status",
            preserves_failure_values
        ),
        case!(
            &factory,
            "status",
            "uses last explicit status without automatic overwrite",
            uses_last_explicit_status
        ),
        case!(
            &factory,
            "recording",
            "merges attributes and records ordered events",
            merges_attributes_and_orders_events
        ),
        case!(
            &factory,
            "recording",
            "makes calls after settlement inert",
            post_settlement_calls_are_inert
        ),
        case!(
            &factory,
            "parentage",
            "records nested and sibling child relationships",
            records_parentage
        ),
        case!(
            &factory,
            "passivity",
            "accepts degenerate payloads without failing",
            accepts_degenerate_payloads
        ),
    ]
}

/// Run every case in order, panicking on the first failure.
pub async fn run_telemetry_adapter_conformance(factory: Arc<dyn TelemetryAdapterFixtureFactory>) {
    for case in telemetry_adapter_conformance(factory) {
        case.run().await;
    }
}

async fn admits_once_synchronously(fixture: &dyn TelemetryAdapterFixture) {
    let context = fixture.context();
    let calls = Arc::new(AtomicUsize::new(0));

    let result = {
        let calls = calls.clone();
        context.in_span(SpanOptions::new("success"), move |_span| {
            calls.fetch_add(1, Ordering::SeqCst);
            42
        })
    };

    assert_eq!(calls.load(Ordering::SeqCst), 1, "callback must run once");
    assert_eq!(result, 42, "the callback result must be returned unchanged");

    let spans = fixture.spans().await;
    let span = find_span(&spans, "success");
    assert_eq!(span.status, SpanStatus::Ok);
    assert!(span.settled, "a completed span must be settled");

    // The async helper admits the same way once polled.
    let value = context
        .in_span_async(SpanOptions::new("async-success"), |_span| async { 7 })
        .await;
    assert_eq!(value, 7);
    let spans = fixture.spans().await;
    assert_eq!(find_span(&spans, "async-success").status, SpanStatus::Ok);
}

async fn preserves_failure_values(fixture: &dyn TelemetryAdapterFixture) {
    let context = fixture.context();

    let expected = ConformanceError("sync");
    let error = context
        .try_in_span(SpanOptions::new("sync-error"), |_span| {
            Err::<(), _>(expected.clone())
        })
        .expect_err("the failure must propagate");
    assert_eq!(error, expected, "the failure value must be unchanged");

    let expected = ConformanceError("async");
    let error = context
        .try_in_span_async(SpanOptions::new("async-error"), |_span| async {
            Err::<(), _>(ConformanceError("async"))
        })
        .await
        .expect_err("the failure must propagate");
    assert_eq!(error, expected);

    let spans = fixture.spans().await;
    for name in ["sync-error", "async-error"] {
        let span = find_span(&spans, name);
        assert!(
            span.status.is_error(),
            "`{name}` must settle with an error status"
        );
        if let SpanStatus::Error { error: Some(error) } = &span.status {
            assert_eq!(
                error.message,
                name.trim_end_matches("-error"),
                "the recorded message must come from the failure value"
            );
        }
    }
}

async fn uses_last_explicit_status(fixture: &dyn TelemetryAdapterFixture) {
    let context = fixture.context();

    context.in_span(SpanOptions::new("last-status"), |span| {
        span.set_status(SpanStatus::error(SpanError::new("Expected", "first")));
        span.set_status(SpanStatus::Ok);
    });

    let _ = context.try_in_span(SpanOptions::new("explicit-before-failure"), |span| {
        span.set_status(SpanStatus::Ok);
        Err::<(), _>(ConformanceError("after explicit status"))
    });

    let _ = context.try_in_span(SpanOptions::new("explicit-error-before-failure"), |span| {
        span.set_status(SpanStatus::error(SpanError::new(
            "Expected",
            "async failure",
        )));
        Err::<(), _>(ConformanceError("rejected"))
    });

    // An expected failure returned as a normal value must be set explicitly.
    context.in_span(SpanOptions::new("expected-failure"), |span| {
        span.set_status(SpanStatus::error(SpanError::new(
            "Expected",
            "returned failure",
        )));
    });

    let spans = fixture.spans().await;
    assert_eq!(find_span(&spans, "last-status").status, SpanStatus::Ok);
    assert_eq!(
        find_span(&spans, "explicit-before-failure").status,
        SpanStatus::Ok,
        "an explicit status must survive an automatic failure"
    );
    assert_eq!(
        find_span(&spans, "explicit-error-before-failure").status,
        SpanStatus::error(SpanError::new("Expected", "async failure"))
    );
    assert_eq!(
        find_span(&spans, "expected-failure").status,
        SpanStatus::error(SpanError::new("Expected", "returned failure"))
    );
}

async fn merges_attributes_and_orders_events(fixture: &dyn TelemetryAdapterFixture) {
    let context = fixture.context();

    context.in_span(
        SpanOptions::new("recording")
            .with_attribute("start", "value")
            .with_attribute("overwrite", "start"),
        |span| {
            span.set_attributes(span_attributes! { "count" => 1, "overwrite" => "middle" });
            // Upstream also passes `count: undefined` here to prove an
            // undefined value neither overwrites nor deletes. An absent key is
            // the Rust equivalent and has the same merge result.
            span.set_attributes(span_attributes! { "overwrite" => "end" });
            span.add_event("first", span_attributes! { "index" => 1 });
            span.add_event("second", span_attributes! { "index" => 2 });
        },
    );

    let spans = fixture.spans().await;
    let span = find_span(&spans, "recording");
    assert_eq!(
        span.attributes,
        span_attributes! { "start" => "value", "overwrite" => "end", "count" => 1 }
    );
    assert_eq!(
        span.events,
        vec![
            RecordedTelemetryEvent {
                name: "first".to_string(),
                attributes: span_attributes! { "index" => 1 },
            },
            RecordedTelemetryEvent {
                name: "second".to_string(),
                attributes: span_attributes! { "index" => 2 },
            },
        ]
    );
}

async fn post_settlement_calls_are_inert(fixture: &dyn TelemetryAdapterFixture) {
    let context = fixture.context();

    let settled = context.in_span(
        SpanOptions::new("settled").with_attribute("value", "initial"),
        |span| span,
    );

    settled.set_attributes(span_attributes! { "value" => "late" });
    settled.add_event("late", span_attributes! { "value" => true });
    settled.set_status(SpanStatus::Error { error: None });

    // A child of a settled span still runs its callback, but records nothing.
    let child_result = settled.in_span(SpanOptions::new("late-child"), |_span| 7);
    assert_eq!(child_result, 7);

    let spans = fixture.spans().await;
    assert_eq!(spans.len(), 1, "no span may be recorded after settlement");
    assert_eq!(
        spans[0].attributes,
        span_attributes! { "value" => "initial" }
    );
    assert!(spans[0].events.is_empty());
    assert_eq!(spans[0].status, SpanStatus::Ok);
}

async fn records_parentage(fixture: &dyn TelemetryAdapterFixture) {
    let context = fixture.context();

    let parent = context.enter_span(SpanOptions::new("parent"));
    let first = parent.child(SpanOptions::new("first-child"));
    let second = parent.child(SpanOptions::new("second-child"));
    drop(second);
    drop(first);
    drop(parent);

    let spans = fixture.spans().await;
    let parent = find_span(&spans, "parent");
    let first = find_span(&spans, "first-child");
    let second = find_span(&spans, "second-child");

    assert_eq!(parent.parent_id, None, "a root span has no parent");
    assert_eq!(first.parent_id, Some(parent.id));
    assert_eq!(second.parent_id, Some(parent.id));

    let (parent_end, first_end, second_end) = (
        parent.end_sequence.expect("parent must have settled"),
        first.end_sequence.expect("first child must have settled"),
        second.end_sequence.expect("second child must have settled"),
    );
    assert!(
        second_end < first_end,
        "children must settle in completion order"
    );
    assert!(
        first_end < parent_end,
        "a parent must settle after its children"
    );
}

async fn accepts_degenerate_payloads(fixture: &dyn TelemetryAdapterFixture) {
    let context = fixture.context();

    let result = context.in_span(
        SpanOptions::new("degenerate").with_attribute("", ""),
        |span| {
            span.add_event("", SpanAttributes::new());
            span.set_attributes(SpanAttributes::new());
            span.set_attributes(span_attributes! { "empty.array" => Vec::<String>::new() });
            span.set_status(SpanStatus::error(SpanError::new("", "")));
            "result"
        },
    );

    assert_eq!(
        result, "result",
        "recording must never change the caller's result"
    );

    let spans = fixture.spans().await;
    let span = find_span(&spans, "degenerate");
    assert!(span.settled);
    assert!(span.status.is_error(), "the explicit status must be kept");
    assert_eq!(span.events.len(), 1);
}

/// Fixture factory for [`InMemoryTelemetryContext`], the reference adapter.
/// Also the worked example an adapter crate copies for its own backend.
#[derive(Debug, Clone, Copy, Default)]
pub struct InMemoryTelemetryFixtureFactory;

struct InMemoryFixture {
    context: InMemoryTelemetryContext,
}

#[async_trait]
impl TelemetryAdapterFixture for InMemoryFixture {
    fn context(&self) -> Arc<dyn TelemetryContext> {
        self.context.as_context()
    }

    async fn spans(&self) -> Vec<crate::RecordedTelemetrySpan> {
        self.context.spans()
    }
}

#[async_trait]
impl TelemetryAdapterFixtureFactory for InMemoryTelemetryFixtureFactory {
    async fn create(&self) -> Box<dyn TelemetryAdapterFixture> {
        Box::new(InMemoryFixture {
            context: InMemoryTelemetryContext::new(),
        })
    }
}
