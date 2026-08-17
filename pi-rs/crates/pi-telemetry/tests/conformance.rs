//! Port of `.upstream/packages/telemetry/test/conformance.test.ts`: run the
//! reusable suite against the in-memory reference adapter, plus the
//! adapter-specific snapshot cases upstream keeps outside the suite.

use std::sync::Arc;

use pi_telemetry::testing::{
    run_telemetry_adapter_conformance, telemetry_adapter_conformance,
    InMemoryTelemetryFixtureFactory, TelemetryAdapterFixtureFactory,
};
use pi_telemetry::{
    span_attributes, InMemoryTelemetryContext, RecordedTelemetryEvent, SpanOptions, SpanOutcome,
    SpanStatus, TelemetryContext, TelemetryContextExt,
};

fn factory() -> Arc<dyn TelemetryAdapterFixtureFactory> {
    Arc::new(InMemoryTelemetryFixtureFactory)
}

#[tokio::test]
async fn in_memory_context_passes_the_conformance_suite() {
    run_telemetry_adapter_conformance(factory()).await;
}

#[tokio::test]
async fn conformance_cases_are_grouped_and_individually_runnable() {
    let cases = telemetry_adapter_conformance(factory());
    assert!(!cases.is_empty());
    // Every case must be independently runnable against a fresh fixture, which
    // is what lets a downstream crate register them one-by-one with its runner.
    for case in &cases {
        assert!(!case.group.is_empty());
        case.run().await;
    }
}

#[test]
fn returns_detached_snapshots_without_exposing_recording_state() {
    let context = InMemoryTelemetryContext::new();

    let mut open_settled = None;
    let mut open_end_sequence = None;
    context.in_span(
        SpanOptions::new("snapshot").with_attribute("tags", vec!["initial"]),
        |span| {
            span.add_event("event", span_attributes! { "value" => 1 });
            let open = context.spans();
            open_settled = Some(open[0].settled);
            open_end_sequence = open[0].end_sequence;
        },
    );

    assert_eq!(open_settled, Some(false), "an open span is not settled");
    assert_eq!(open_end_sequence, None, "an open span has no end sequence");

    let first = context.spans();
    assert!(first[0].settled);
    assert_eq!(first[0].end_sequence, Some(1));

    // Snapshots are owned clones, so upstream's "mutate the snapshot and
    // re-read" check is a compile-time guarantee here. Mutating the copy is
    // still worth showing: the recorded state must not follow it.
    let mut mutated = first;
    mutated[0].attributes = span_attributes! { "tags" => vec!["mutated"] };
    mutated[0].events[0].attributes = span_attributes! { "value" => 2 };

    let second = context.spans();
    assert_eq!(
        second[0].attributes,
        span_attributes! { "tags" => vec!["initial"] }
    );
    assert_eq!(
        second[0].events,
        vec![RecordedTelemetryEvent {
            name: "event".to_string(),
            attributes: span_attributes! { "value" => 1 },
        }]
    );
}

#[test]
fn ids_and_end_sequences_start_at_one_and_increment() {
    let context = InMemoryTelemetryContext::new();
    context.in_span(SpanOptions::new("first"), |_| {});
    context.in_span(SpanOptions::new("second"), |_| {});

    let spans = context.spans();
    assert_eq!(spans[0].id, 1);
    assert_eq!(spans[1].id, 2);
    assert_eq!(spans[0].end_sequence, Some(1));
    assert_eq!(spans[1].end_sequence, Some(2));
}

#[test]
fn settling_twice_is_idempotent() {
    let context = InMemoryTelemetryContext::new();
    let span = context.start_span(SpanOptions::new("once"));
    span.end(SpanOutcome::Success);
    span.end(SpanOutcome::Failure(None));

    let spans = context.spans();
    assert_eq!(spans[0].status, SpanStatus::Ok, "a settled span is frozen");
    assert_eq!(spans[0].end_sequence, Some(1));
}

#[test]
fn an_unwinding_scope_settles_as_an_error() {
    let context = InMemoryTelemetryContext::new();
    let recorder = context.clone();

    // A panic is this port's analogue of upstream's synchronous throw: the
    // guard must still settle the span, and settle it as an error.
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        recorder.in_span(SpanOptions::new("panicking"), |_span| {
            panic!("boom");
        })
    }));
    assert!(panicked.is_err());

    let spans = context.spans();
    assert!(spans[0].settled);
    assert!(spans[0].status.is_error());
}

#[tokio::test]
async fn concurrent_children_record_their_parent_and_settle_in_completion_order() {
    // The conformance suite models this with interleaved guard lifetimes so it
    // needs no runtime; here it is with genuinely concurrent tasks.
    let context = InMemoryTelemetryContext::new();
    let (release, released) = tokio::sync::oneshot::channel::<()>();

    context
        .in_span_async(SpanOptions::new("parent"), |parent| async move {
            let first = parent.in_span_async(SpanOptions::new("first-child"), |_| async move {
                let _ = released.await;
            });
            let second =
                parent.in_span_async(SpanOptions::new("second-child"), |_| async move { "done" });

            assert_eq!(second.await, "done");
            let _ = release.send(());
            first.await;
        })
        .await;

    let spans = context.spans();
    let parent = spans.iter().find(|span| span.name == "parent").unwrap();
    let first = spans
        .iter()
        .find(|span| span.name == "first-child")
        .unwrap();
    let second = spans
        .iter()
        .find(|span| span.name == "second-child")
        .unwrap();

    assert_eq!(parent.parent_id, None);
    assert_eq!(first.parent_id, Some(parent.id));
    assert_eq!(second.parent_id, Some(parent.id));
    assert!(second.end_sequence < first.end_sequence);
    assert!(first.end_sequence < parent.end_sequence);
}
