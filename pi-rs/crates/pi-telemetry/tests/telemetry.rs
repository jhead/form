//! Port of `.upstream/packages/telemetry/test/telemetry.test.ts`.
//!
//! Upstream's schema cases are largely `expectTypeOf` / `@ts-expect-error`
//! assertions: they prove the type checker rejects a missing required
//! attribute, a value outside a closed set, an undeclared event, an unknown
//! span name and a duplicate span name across schemas. This port checks the
//! same rules, but as the runtime validation that replaces them.

use std::sync::Arc;

use pi_telemetry::{
    noop_span, noop_telemetry_context, span_attributes, AttributeDefinition, AttributeValue,
    EventDefinition, InMemoryTelemetryContext, ParentDefinition, Span, SpanDefinition, SpanError,
    SpanOptions, SpanStatus, SpanStatusRule, TelemetryContext, TelemetryContextExt, TelemetryError,
    TelemetrySchema, TypedSpanStarter,
};

fn operation_schema() -> TelemetrySchema {
    TelemetrySchema::new(1).with_span(
        "operation",
        SpanDefinition::new(
            "Test operation",
            ParentDefinition::Any,
            SpanStatusRule::new("The operation fails"),
        )
        .with_start_attribute(
            "kind",
            AttributeDefinition::string("Kind")
                .with_values([AttributeValue::from("read"), AttributeValue::from("write")])
                .required(true),
        )
        .with_event(
            "result",
            EventDefinition::new("Result").with_attribute(
                "outcome",
                AttributeDefinition::string("Outcome")
                    .with_values([AttributeValue::from("ok"), AttributeValue::from("error")])
                    .required(true),
            ),
        ),
    )
}

/// The `parents: { kind: "root_or_external" }` variant from upstream's second
/// schema case.
fn root_operation_schema() -> TelemetrySchema {
    TelemetrySchema::new(1).with_span(
        "operation",
        SpanDefinition::new(
            "Operation",
            ParentDefinition::RootOrExternal,
            SpanStatusRule::new("The operation fails"),
        )
        .with_start_attribute(
            "kind",
            AttributeDefinition::string("Kind")
                .with_values([AttributeValue::from("read"), AttributeValue::from("write")])
                .required(true),
        ),
    )
}

fn request_schema() -> TelemetrySchema {
    TelemetrySchema::new(3).with_span(
        "request",
        SpanDefinition::new(
            "Request",
            ParentDefinition::spans(["operation"]),
            SpanStatusRule::new("The request fails"),
        )
        .with_start_attribute(
            "provider",
            AttributeDefinition::string("Provider").required(true),
        )
        .with_end_attribute("response", AttributeDefinition::string("Response kind")),
    )
}

fn starter(schemas: Vec<TelemetrySchema>) -> (InMemoryTelemetryContext, TypedSpanStarter) {
    let context = InMemoryTelemetryContext::new();
    let starter = TypedSpanStarter::new(context.as_context(), schemas).expect("valid vocabulary");
    (context, starter)
}

// --- schemas as data -------------------------------------------------------

#[test]
fn preserves_serializable_definitions() {
    let schema = operation_schema();
    schema.validate().expect("schema is internally consistent");

    let json = serde_json::to_string(&schema).expect("schemas are JSON-serializable");
    let round_tripped = TelemetrySchema::from_json(&json).expect("round-trips");
    assert_eq!(round_tripped, schema);

    // The wire shape is upstream's, because domain packages ship these as JSON.
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let span = &value["spans"]["operation"];
    assert_eq!(span["parents"]["kind"], "any");
    assert_eq!(span["status"]["default"], "ok");
    assert_eq!(span["status"]["errorWhen"], "The operation fails");
    assert_eq!(span["startAttributes"]["kind"]["type"], "string");
    assert_eq!(span["startAttributes"]["kind"]["required"], true);
    assert_eq!(
        span["startAttributes"]["kind"]["values"],
        serde_json::json!(["read", "write"])
    );
    assert_eq!(
        span["events"]["result"]["attributes"]["outcome"]["required"],
        true
    );
}

#[test]
fn rejects_closed_set_values_that_contradict_the_declared_type() {
    let schema = TelemetrySchema::new(1).with_span(
        "operation",
        SpanDefinition::new(
            "Test operation",
            ParentDefinition::Any,
            SpanStatusRule::new("fails"),
        )
        .with_start_attribute(
            "kind",
            AttributeDefinition::string("Kind")
                .with_values([AttributeValue::Int(1)])
                .required(true),
        ),
    );

    let error = schema.validate().expect_err("mistyped closed set");
    assert_eq!(error.code(), "invalid_schema");
}

#[test]
fn rejects_element_values_on_a_scalar_attribute() {
    let schema = TelemetrySchema::new(1).with_span(
        "operation",
        SpanDefinition::new("op", ParentDefinition::Any, SpanStatusRule::new("fails"))
            .with_end_attribute(
                "kind",
                AttributeDefinition::string("Kind")
                    .with_element_values([AttributeValue::from("read")]),
            ),
    );

    assert_eq!(
        schema.validate().expect_err("scalars use `values`").code(),
        "invalid_schema"
    );
}

// --- typed span starter ----------------------------------------------------

#[test]
fn validates_start_attributes_against_the_schema() {
    let (_context, starter) = starter(vec![operation_schema()]);

    // Upstream: `@ts-expect-error unknown span names are rejected`.
    assert_eq!(
        starter
            .start_span("unknown", span_attributes! {})
            .expect_err("unknown span")
            .code(),
        "unknown_span"
    );

    // Required attributes cannot be omitted.
    assert_eq!(
        starter
            .start_span("operation", span_attributes! {})
            .expect_err("missing required attribute")
            .code(),
        "missing_attribute"
    );

    // Closed-set values are exact.
    assert_eq!(
        starter
            .start_span("operation", span_attributes! { "kind" => "other" })
            .expect_err("value outside the closed set")
            .code(),
        "attribute_value_not_allowed"
    );

    // Declared types are enforced.
    assert_eq!(
        starter
            .start_span("operation", span_attributes! { "kind" => 1 })
            .expect_err("wrong type")
            .code(),
        "attribute_type_mismatch"
    );

    // Undeclared keys are rejected, like `ExactTelemetryAttributes` upstream.
    assert_eq!(
        starter
            .start_span(
                "operation",
                span_attributes! { "kind" => "read", "unknown" => true }
            )
            .expect_err("undeclared attribute")
            .code(),
        "unknown_attribute"
    );

    starter
        .start_span("operation", span_attributes! { "kind" => "read" })
        .expect("a valid start");
}

#[test]
fn validates_events_and_end_attributes() {
    let (context, starter) = starter(vec![operation_schema()]);
    let span = starter
        .start_span("operation", span_attributes! { "kind" => "read" })
        .expect("valid start");

    // Upstream: `@ts-expect-error undeclared events are rejected`.
    assert_eq!(
        span.add_event("unknown", span_attributes! {})
            .expect_err("undeclared event")
            .code(),
        "unknown_event"
    );

    // Required event attributes cannot be omitted.
    assert_eq!(
        span.add_event("result", span_attributes! {})
            .expect_err("missing event attribute")
            .code(),
        "missing_attribute"
    );

    // Closed-set event values are exact.
    assert_eq!(
        span.add_event("result", span_attributes! { "outcome" => "other" })
            .expect_err("value outside the closed set")
            .code(),
        "attribute_value_not_allowed"
    );

    // An empty end schema rejects every attribute.
    assert_eq!(
        span.set_attributes(span_attributes! { "unknown" => true })
            .expect_err("empty end schema")
            .code(),
        "unknown_attribute"
    );

    span.add_event("result", span_attributes! { "outcome" => "ok" })
        .expect("a declared event");
    drop(span);

    let spans = context.spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].events.len(), 1, "only the valid event is recorded");
    assert_eq!(spans[0].events[0].name, "result");
    assert_eq!(
        spans[0].attributes,
        span_attributes! { "kind" => "read" },
        "a rejected setAttributes call records nothing"
    );
}

#[test]
fn combines_schema_vocabularies_and_binds_children_to_their_parent() {
    let (context, starter) = starter(vec![root_operation_schema(), request_schema()]);

    let result = {
        let operation = starter
            .start_span("operation", span_attributes! { "kind" => "read" })
            .expect("valid operation");
        let request = operation
            .start_span("request", span_attributes! { "provider" => "example" })
            .expect("valid request");
        request
            .set_attributes(span_attributes! { "response" => "cached" })
            .expect("declared end attribute");
        42
    };
    assert_eq!(result, 42);

    let spans = context.spans();
    let operation = spans.iter().find(|span| span.name == "operation").unwrap();
    let request = spans.iter().find(|span| span.name == "request").unwrap();
    assert_eq!(operation.parent_id, None);
    assert_eq!(request.parent_id, Some(operation.id));
    assert_eq!(
        request.attributes,
        span_attributes! { "provider" => "example", "response" => "cached" },
        "start and end attributes land on the same span"
    );
    assert!(request.end_sequence < operation.end_sequence);
}

#[test]
fn rejects_duplicate_span_names_across_schemas() {
    let context = InMemoryTelemetryContext::new();
    let error = TypedSpanStarter::new(
        context.as_context(),
        vec![operation_schema(), operation_schema()],
    )
    .expect_err("duplicate span names");

    assert_eq!(error.code(), "duplicate_span_name");
    assert!(matches!(
        error,
        TelemetryError::DuplicateSpanName { ref name } if name == "operation"
    ));
}

#[test]
fn rejects_attributes_belonging_to_another_schemas_span() {
    let (_context, starter) = starter(vec![root_operation_schema(), request_schema()]);
    let operation = starter
        .start_span("operation", span_attributes! { "kind" => "read" })
        .expect("valid operation");

    // Upstream: `@ts-expect-error attributes are selected from the schema that
    // owns the span`.
    assert_eq!(
        operation
            .start_span("request", span_attributes! { "kind" => "read" })
            .expect_err("attributes from the wrong span")
            .code(),
        "unknown_attribute"
    );
}

#[test]
fn enforces_parent_rules() {
    let (_context, starter) = starter(vec![root_operation_schema(), request_schema()]);

    // `parents: { kind: "spans", spans: ["operation"] }` — not startable at root.
    assert_eq!(
        starter
            .start_span("request", span_attributes! { "provider" => "example" })
            .expect_err("request needs an operation parent")
            .code(),
        "invalid_parent"
    );

    // `parents: { kind: "root_or_external" }` — not startable under a schema span.
    let operation = starter
        .start_span("operation", span_attributes! { "kind" => "read" })
        .expect("valid operation");
    assert_eq!(
        operation
            .start_span("operation", span_attributes! { "kind" => "write" })
            .expect_err("operation must be root or external")
            .code(),
        "invalid_parent"
    );
}

#[test]
fn typed_spans_settle_and_carry_failures() {
    let (context, starter) = starter(vec![operation_schema()]);

    {
        let span = starter
            .start_span("operation", span_attributes! { "kind" => "write" })
            .expect("valid start");
        span.fail(SpanError::new("Expected", "sync"));
    }

    let spans = context.spans();
    assert!(spans[0].settled);
    assert_eq!(
        spans[0].status,
        SpanStatus::error(SpanError::new("Expected", "sync"))
    );
}

#[test]
fn typed_spans_expose_the_untyped_handle_for_unschematized_callees() {
    let (context, starter) = starter(vec![operation_schema()]);
    let span = starter
        .start_span("operation", span_attributes! { "kind" => "read" })
        .expect("valid start");

    // A callee that takes a plain context records an unvalidated child span.
    let plain: Arc<dyn TelemetryContext> = span.span().as_context();
    plain.in_span(SpanOptions::new("adhoc"), |_| {});
    drop(span);

    let spans = context.spans();
    let adhoc = spans.iter().find(|span| span.name == "adhoc").unwrap();
    let operation = spans.iter().find(|span| span.name == "operation").unwrap();
    assert_eq!(adhoc.parent_id, Some(operation.id));
}

// --- no-op context ---------------------------------------------------------

#[test]
fn noop_admits_callbacks_and_reuses_one_inert_span() {
    let context = noop_telemetry_context();

    let mut captured: Option<Span> = None;
    let result = context.in_span(SpanOptions::new("first"), |span| {
        let child = span.in_span(SpanOptions::new("child"), |child| child);
        assert!(
            Span::ptr_eq(&child, &span),
            "nested no-op spans reuse one inert span"
        );
        captured = Some(span);
        42
    });

    assert_eq!(result, 42);
    let captured = captured.expect("callback ran");
    assert!(Span::ptr_eq(&captured, &noop_span()));
}

#[test]
fn noop_preserves_failure_values() {
    let context = noop_telemetry_context();

    #[derive(Debug, PartialEq)]
    struct Failure(&'static str);
    impl std::fmt::Display for Failure {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }

    let error = context
        .try_in_span(SpanOptions::new("sync"), |_| Err::<(), _>(Failure("sync")))
        .expect_err("the failure propagates");
    assert_eq!(error, Failure("sync"));
}

#[tokio::test]
async fn noop_preserves_async_failure_values() {
    let context = noop_telemetry_context();
    let error = context
        .try_in_span_async(SpanOptions::new("async"), |_| async {
            Err::<(), _>(std::io::Error::other("async"))
        })
        .await
        .expect_err("the failure propagates");
    assert_eq!(error.to_string(), "async");
}

#[test]
fn noop_does_not_retain_telemetry_payloads() {
    let context = noop_telemetry_context();
    context.in_span(
        SpanOptions::new("operation").with_attribute("secret", "prompt content"),
        |span| {
            // None of these may panic or retain anything.
            span.add_event("event", span_attributes! { "secret" => "content" });
            span.set_attributes(span_attributes! { "secret" => "content" });
            span.set_status(SpanStatus::error(SpanError::new("Ignored", "ignored")));
        },
    );
}

#[test]
fn error_codes_are_stable() {
    // FFI callers match on these strings.
    for (error, code) in [
        (
            TelemetryError::UnknownSpan { name: "x".into() },
            "unknown_span",
        ),
        (
            TelemetryError::InvalidSchema {
                message: "x".into(),
            },
            "invalid_schema",
        ),
    ] {
        assert_eq!(error.code(), code);
        // Errors round-trip as JSON for the bridge.
        let json = serde_json::to_string(&error).unwrap();
        let back: TelemetryError = serde_json::from_str(&json).unwrap();
        assert_eq!(back, error);
    }
}
