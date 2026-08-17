//! Port of `packages/agent/src/harness/telemetry.ts`.
//!
//! Upstream declares two schemas as `as const` object literals and then derives
//! a family of conditional types from them so `startAiSpan` / `startHarnessSpan`
//! are checked at compile time. `pi-telemetry` (W1) already made the call that
//! Rust keeps the *data* half verbatim and validates at runtime, so the schemas
//! live here as JSON transcribed from upstream and are parsed once.
//!
//! Keeping them as JSON rather than builder calls is deliberate: the span names,
//! attribute keys and closed value sets are the observable contract shared with
//! the TypeScript implementation, and a diff against `telemetry.ts` should stay
//! readable.

use std::sync::{Arc, OnceLock};

use pi_telemetry::schema::{TelemetrySchema, TypedSpan, TypedSpanStarter};
use pi_telemetry::{SpanAttributes, TelemetryContext, TelemetryError};

/// The one AI-request span name.
pub const AI_SPAN_REQUEST: &str = "pi.ai.request";

/// Harness span names.
pub const HARNESS_SPAN_RUN: &str = "pi.harness.run";
pub const HARNESS_SPAN_COMPACTION: &str = "pi.harness.compaction";
pub const HARNESS_SPAN_NAVIGATION: &str = "pi.harness.navigation";
pub const HARNESS_SPAN_CHECKPOINT: &str = "pi.harness.checkpoint";
pub const HARNESS_SPAN_TURN: &str = "pi.harness.turn";
pub const HARNESS_SPAN_STEP: &str = "pi.harness.step";
pub const HARNESS_SPAN_TOOL: &str = "pi.harness.tool";
pub const HARNESS_SPAN_HOOK: &str = "pi.harness.hook";
pub const HARNESS_SPAN_SLEEP: &str = "pi.harness.sleep";
pub const HARNESS_SPAN_EVENT_HANDLER: &str = "pi.harness.event_handler";
pub const HARNESS_SPAN_SESSION_WRITE: &str = "pi.session.write";

/// Closed set for `pi.hook.name`.
pub const HOOK_NAMES: [&str; 11] = [
    "before_run",
    "before_resume",
    "before_run_end",
    "transform_context",
    "before_request",
    "before_payload",
    "after_response",
    "before_tool",
    "after_tool",
    "before_compaction",
    "before_navigation",
];

/// Closed set for `pi.event.type`.
pub const EVENT_TYPES: [&str; 29] = [
    "run_start",
    "run_resume",
    "run_suspend",
    "run_abort",
    "run_end",
    "fault",
    "handler_error",
    "turn_start",
    "turn_end",
    "retry_scheduled",
    "retry_start",
    "retry_end",
    "message_start",
    "message_update",
    "message_end",
    "tool_start",
    "tool_update",
    "tool_end",
    "entry_added",
    "write_pending",
    "queue_update",
    "fact_update",
    "config_update",
    "compaction_start",
    "compaction_end",
    "navigation_start",
    "navigation_end",
    "lane_created",
    "usage",
];

const AI_TELEMETRY_SCHEMA_JSON: &str = r#"{
  "version": 1,
  "spans": {
    "pi.ai.request": {
      "description": "One logical request to an AI provider",
      "parents": { "kind": "any" },
      "startAttributes": {
        "pi.ai.operation": {
          "type": "string",
          "required": true,
          "values": ["stream", "fetch_deferred", "cancel_deferred", "generate_images"],
          "description": "Logical provider operation"
        },
        "pi.ai.provider": { "type": "string", "required": true, "description": "Selected provider id" },
        "pi.ai.model": { "type": "string", "required": true, "description": "Requested model id" },
        "pi.ai.api": { "type": "string", "required": true, "description": "Provider API id" },
        "pi.ai.streaming": {
          "type": "boolean",
          "required": true,
          "description": "Whether this operation returns a stream"
        },
        "pi.ai.deferred": {
          "type": "boolean",
          "required": false,
          "description": "Whether the operation requests or participates in deferred execution"
        }
      },
      "endAttributes": {
        "pi.ai.response.model": { "type": "string", "description": "Concrete response model" },
        "pi.ai.response.id": { "type": "string", "cardinality": "high", "description": "Provider response id" },
        "pi.ai.response.stop_reason": {
          "type": "string",
          "values": ["stop", "length", "tool_use", "error", "aborted", "deferred"],
          "description": "Normalized terminal response reason"
        },
        "pi.ai.http.status_code": { "type": "number", "description": "Final HTTP status" },
        "pi.ai.usage.input_tokens": { "type": "number", "description": "Reported input tokens" },
        "pi.ai.usage.output_tokens": { "type": "number", "description": "Reported output tokens" },
        "pi.ai.usage.cache_read_tokens": { "type": "number", "description": "Reported cache-read tokens" },
        "pi.ai.usage.cache_write_tokens": { "type": "number", "description": "Reported cache-write tokens" },
        "pi.ai.usage.reasoning_tokens": { "type": "number", "description": "Reported reasoning tokens" },
        "pi.ai.usage.total_tokens": { "type": "number", "description": "Reported total tokens" },
        "pi.ai.usage.cost": { "type": "number", "description": "Reported total cost" },
        "pi.ai.stream.chunk_count": { "type": "number", "description": "Streamed update chunk count" },
        "pi.ai.stream.time_to_first_chunk_ms": {
          "type": "number",
          "description": "Elapsed milliseconds to first update chunk"
        },
        "pi.ai.error.type": {
          "type": "string",
          "cardinality": "low",
          "description": "Provider or transport error class"
        }
      },
      "status": { "default": "ok", "errorWhen": "The operation throws or returns an error result" }
    }
  }
}"#;

const HARNESS_TELEMETRY_SCHEMA_JSON: &str = r#"{
  "version": 1,
  "spans": {
    "pi.harness.run": {
      "description": "One admitted in-process run invocation",
      "parents": { "kind": "root_or_external" },
      "startAttributes": {
        "pi.session.id": { "type": "string", "required": true, "cardinality": "high", "description": "Session id" },
        "pi.lane.name": { "type": "string", "required": true, "cardinality": "high", "description": "Lane name" },
        "pi.operation.id": {
          "type": "string",
          "required": true,
          "cardinality": "high",
          "description": "Durable operation id"
        },
        "pi.operation.recovery": {
          "type": "boolean",
          "required": true,
          "description": "Whether this invocation resumes durable work"
        },
        "pi.operation.kind": {
          "type": "string",
          "required": true,
          "values": ["run"],
          "description": "Run operation kind"
        }
      },
      "endAttributes": {
        "pi.operation.outcome": {
          "type": "string",
          "values": ["completed", "aborted", "failed", "suspended"],
          "description": "Run invocation outcome"
        },
        "pi.error.code": { "type": "string", "cardinality": "low", "description": "Stable operation error code" },
        "pi.error.type": {
          "type": "string",
          "cardinality": "low",
          "description": "Low-cardinality operation error class"
        }
      },
      "status": { "default": "ok", "errorWhen": "The run fails or throws" }
    },
    "pi.harness.compaction": {
      "description": "One admitted in-process manual compaction invocation",
      "parents": { "kind": "root_or_external" },
      "startAttributes": {
        "pi.session.id": { "type": "string", "required": true, "cardinality": "high", "description": "Session id" },
        "pi.lane.name": { "type": "string", "required": true, "cardinality": "high", "description": "Lane name" },
        "pi.operation.id": {
          "type": "string",
          "required": true,
          "cardinality": "high",
          "description": "Durable operation id"
        },
        "pi.operation.recovery": {
          "type": "boolean",
          "required": true,
          "description": "Whether this invocation resumes durable work"
        },
        "pi.operation.kind": {
          "type": "string",
          "required": true,
          "values": ["compaction"],
          "description": "Compaction operation kind"
        }
      },
      "endAttributes": {
        "pi.operation.outcome": {
          "type": "string",
          "values": ["completed", "declined", "aborted", "failed"],
          "description": "Compaction invocation outcome"
        },
        "pi.error.code": { "type": "string", "cardinality": "low", "description": "Stable operation error code" },
        "pi.error.type": {
          "type": "string",
          "cardinality": "low",
          "description": "Low-cardinality operation error class"
        }
      },
      "status": { "default": "ok", "errorWhen": "The compaction fails or throws" }
    },
    "pi.harness.navigation": {
      "description": "One admitted in-process navigation invocation",
      "parents": { "kind": "root_or_external" },
      "startAttributes": {
        "pi.session.id": { "type": "string", "required": true, "cardinality": "high", "description": "Session id" },
        "pi.lane.name": { "type": "string", "required": true, "cardinality": "high", "description": "Lane name" },
        "pi.operation.id": {
          "type": "string",
          "required": true,
          "cardinality": "high",
          "description": "Durable operation id"
        },
        "pi.operation.recovery": {
          "type": "boolean",
          "required": true,
          "description": "Whether this invocation resumes durable work"
        },
        "pi.operation.kind": {
          "type": "string",
          "required": true,
          "values": ["navigation"],
          "description": "Navigation operation kind"
        }
      },
      "endAttributes": {
        "pi.operation.outcome": {
          "type": "string",
          "values": ["completed", "declined", "aborted", "failed"],
          "description": "Navigation invocation outcome"
        },
        "pi.error.code": { "type": "string", "cardinality": "low", "description": "Stable operation error code" },
        "pi.error.type": {
          "type": "string",
          "cardinality": "low",
          "description": "Low-cardinality operation error class"
        }
      },
      "status": { "default": "ok", "errorWhen": "The navigation fails or throws" }
    },
    "pi.harness.checkpoint": {
      "description": "One run checkpoint",
      "parents": { "kind": "spans", "spans": ["pi.harness.run"] },
      "startAttributes": {
        "pi.lane.name": { "type": "string", "required": true, "cardinality": "high", "description": "Lane name" },
        "pi.operation.id": {
          "type": "string",
          "required": true,
          "cardinality": "high",
          "description": "Durable operation id"
        },
        "pi.checkpoint.kind": {
          "type": "string",
          "required": true,
          "values": ["normal", "failure_drain", "abort_reconcile"],
          "description": "Checkpoint purpose"
        }
      },
      "endAttributes": {},
      "status": { "default": "ok", "errorWhen": "Checkpoint work throws" }
    },
    "pi.harness.turn": {
      "description": "One assistant response and its tool batch",
      "parents": { "kind": "spans", "spans": ["pi.harness.run"] },
      "startAttributes": {
        "pi.lane.name": { "type": "string", "required": true, "cardinality": "high", "description": "Lane name" },
        "pi.operation.id": {
          "type": "string",
          "required": true,
          "cardinality": "high",
          "description": "Durable operation id"
        },
        "pi.turn.id": {
          "type": "string",
          "required": true,
          "cardinality": "high",
          "description": "Invocation-local turn id"
        }
      },
      "endAttributes": {},
      "status": { "default": "ok", "errorWhen": "Turn work throws" }
    },
    "pi.harness.step": {
      "description": "One durable retry attempt",
      "parents": {
        "kind": "spans",
        "spans": ["pi.harness.turn", "pi.harness.checkpoint", "pi.harness.compaction", "pi.harness.navigation"]
      },
      "startAttributes": {
        "pi.lane.name": { "type": "string", "required": true, "cardinality": "high", "description": "Lane name" },
        "pi.operation.id": {
          "type": "string",
          "required": true,
          "cardinality": "high",
          "description": "Durable operation id"
        },
        "pi.step.kind": {
          "type": "string",
          "required": true,
          "values": ["assistant", "compaction", "branch_summary"],
          "description": "Retryable step kind"
        },
        "pi.step.attempt": {
          "type": "number",
          "required": true,
          "description": "One-based durable attempt number"
        },
        "pi.compaction.reason": {
          "type": "string",
          "required": false,
          "values": ["manual", "threshold", "overflow"],
          "description": "Compaction trigger"
        }
      },
      "endAttributes": {
        "pi.step.outcome": {
          "type": "string",
          "values": ["succeeded", "retry", "failed", "aborted", "deferred", "overflow"],
          "description": "Attempt outcome"
        }
      },
      "status": { "default": "ok", "errorWhen": "The attempt retries, fails, or throws" }
    },
    "pi.harness.tool": {
      "description": "One raw phase-2 tool execution",
      "parents": { "kind": "spans", "spans": ["pi.harness.turn", "pi.harness.run"] },
      "startAttributes": {
        "pi.lane.name": { "type": "string", "required": true, "cardinality": "high", "description": "Lane name" },
        "pi.operation.id": {
          "type": "string",
          "required": true,
          "cardinality": "high",
          "description": "Durable operation id"
        },
        "pi.turn.id": {
          "type": "string",
          "required": false,
          "cardinality": "high",
          "description": "Invocation-local live turn id"
        },
        "pi.tool.name": { "type": "string", "required": true, "description": "Tool name" },
        "pi.tool.call_id": {
          "type": "string",
          "required": true,
          "cardinality": "high",
          "description": "Tool call id"
        },
        "pi.tool.replay": {
          "type": "string",
          "required": true,
          "values": ["never", "safe"],
          "description": "Declared replay policy"
        },
        "pi.tool.recovery": {
          "type": "boolean",
          "required": true,
          "description": "Whether this is recovery execution"
        }
      },
      "endAttributes": {
        "pi.tool.is_error": {
          "type": "boolean",
          "description": "Whether raw phase-2 execution returned an error"
        }
      },
      "status": { "default": "ok", "errorWhen": "Raw phase-2 execution returns an error" }
    },
    "pi.harness.hook": {
      "description": "One registered hook handler invocation",
      "parents": { "kind": "any" },
      "startAttributes": {
        "pi.lane.name": { "type": "string", "required": true, "cardinality": "high", "description": "Lane name" },
        "pi.operation.id": {
          "type": "string",
          "required": false,
          "cardinality": "high",
          "description": "Durable operation id when accepted"
        },
        "pi.hook.name": {
          "type": "string",
          "required": true,
          "values": [
            "before_run",
            "before_resume",
            "before_run_end",
            "transform_context",
            "before_request",
            "before_payload",
            "after_response",
            "before_tool",
            "after_tool",
            "before_compaction",
            "before_navigation"
          ],
          "description": "Hook name"
        },
        "pi.hook.registration_id": {
          "type": "string",
          "required": false,
          "description": "Stable hook registration id"
        }
      },
      "endAttributes": {
        "pi.hook.outcome": {
          "type": "string",
          "values": ["completed", "skipped", "blocked", "failed"],
          "description": "Handler outcome"
        }
      },
      "status": { "default": "ok", "errorWhen": "The handler throws" }
    },
    "pi.harness.sleep": {
      "description": "One retry delay",
      "parents": { "kind": "spans", "spans": ["pi.harness.step", "pi.harness.run"] },
      "startAttributes": {
        "pi.operation.id": {
          "type": "string",
          "required": true,
          "cardinality": "high",
          "description": "Durable operation id"
        },
        "pi.sleep.delay_ms": {
          "type": "number",
          "required": true,
          "description": "Requested delay in milliseconds"
        }
      },
      "endAttributes": {
        "pi.sleep.outcome": {
          "type": "string",
          "values": ["elapsed", "aborted"],
          "description": "Delay outcome"
        }
      },
      "status": { "default": "ok", "errorWhen": "Sleep work throws" }
    },
    "pi.harness.event_handler": {
      "description": "One passive event listener invocation",
      "parents": { "kind": "any" },
      "startAttributes": {
        "pi.event.type": {
          "type": "string",
          "required": true,
          "cardinality": "low",
          "values": [
            "run_start",
            "run_resume",
            "run_suspend",
            "run_abort",
            "run_end",
            "fault",
            "handler_error",
            "turn_start",
            "turn_end",
            "retry_scheduled",
            "retry_start",
            "retry_end",
            "message_start",
            "message_update",
            "message_end",
            "tool_start",
            "tool_update",
            "tool_end",
            "entry_added",
            "write_pending",
            "queue_update",
            "fact_update",
            "config_update",
            "compaction_start",
            "compaction_end",
            "navigation_start",
            "navigation_end",
            "lane_created",
            "usage"
          ],
          "description": "Delivered harness event type"
        },
        "pi.lane.name": {
          "type": "string",
          "required": false,
          "cardinality": "high",
          "description": "Lane name for lane-scoped events"
        }
      },
      "endAttributes": {},
      "status": { "default": "ok", "errorWhen": "The listener throws" }
    },
    "pi.session.write": {
      "description": "One committed session mutation",
      "parents": { "kind": "any" },
      "startAttributes": {
        "pi.lane.name": { "type": "string", "required": true, "cardinality": "high", "description": "Lane name" },
        "pi.operation.id": {
          "type": "string",
          "required": false,
          "cardinality": "high",
          "description": "Durable operation id when accepted"
        },
        "pi.session.mutation": {
          "type": "string",
          "required": true,
          "values": ["entry", "record", "lane", "fact"],
          "description": "Session mutation kind"
        },
        "pi.session.item_type": {
          "type": "string",
          "required": false,
          "description": "Entry, record, lane, or fact subtype"
        }
      },
      "endAttributes": {
        "pi.session.seq": {
          "type": "number",
          "description": "Committed session sequence when exposed"
        }
      },
      "status": { "default": "ok", "errorWhen": "Storage rejects the mutation" }
    }
  }
}"#;

fn parse(json: &str, what: &str) -> TelemetrySchema {
    TelemetrySchema::from_json(json)
        .unwrap_or_else(|error| panic!("{what} telemetry schema is malformed: {error}"))
}

/// `AI_TELEMETRY_SCHEMA` upstream.
pub fn ai_telemetry_schema() -> &'static TelemetrySchema {
    static SCHEMA: OnceLock<TelemetrySchema> = OnceLock::new();
    SCHEMA.get_or_init(|| parse(AI_TELEMETRY_SCHEMA_JSON, "ai"))
}

/// `HARNESS_TELEMETRY_SCHEMA` upstream.
pub fn harness_telemetry_schema() -> &'static TelemetrySchema {
    static SCHEMA: OnceLock<TelemetrySchema> = OnceLock::new();
    SCHEMA.get_or_init(|| parse(HARNESS_TELEMETRY_SCHEMA_JSON, "harness"))
}

/// `AGENT_TELEMETRY_SCHEMAS` upstream: the combined span vocabulary.
pub fn agent_telemetry_schemas() -> Vec<TelemetrySchema> {
    vec![
        ai_telemetry_schema().clone(),
        harness_telemetry_schema().clone(),
    ]
}

/// A [`TypedSpanStarter`] over both agent schemas.
pub fn agent_span_starter(
    context: Arc<dyn TelemetryContext>,
) -> Result<TypedSpanStarter, TelemetryError> {
    TypedSpanStarter::new(context, agent_telemetry_schemas())
}

/// `startAiSpan` upstream.
///
/// Upstream owns settlement through a callback because TypeScript has no
/// destructors; `pi-telemetry` settles on drop instead, so this returns the
/// live [`TypedSpan`] rather than taking a closure.
pub fn start_ai_span(
    context: Arc<dyn TelemetryContext>,
    name: &str,
    attributes: SpanAttributes,
) -> Result<TypedSpan, TelemetryError> {
    TypedSpanStarter::new(context, vec![ai_telemetry_schema().clone()])?
        .start_span(name, attributes)
}

/// `startHarnessSpan` upstream. See [`start_ai_span`] for the settlement note.
pub fn start_harness_span(
    context: Arc<dyn TelemetryContext>,
    name: &str,
    attributes: SpanAttributes,
) -> Result<TypedSpan, TelemetryError> {
    TypedSpanStarter::new(context, vec![harness_telemetry_schema().clone()])?
        .start_span(name, attributes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_telemetry::{AttributeValue, InMemoryTelemetryContext};

    fn attrs(pairs: &[(&str, AttributeValue)]) -> SpanAttributes {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn both_schemas_parse_and_validate() {
        assert_eq!(ai_telemetry_schema().version, 1);
        assert_eq!(harness_telemetry_schema().version, 1);
        assert!(ai_telemetry_schema().spans.contains_key(AI_SPAN_REQUEST));
        assert_eq!(harness_telemetry_schema().spans.len(), 11);
    }

    #[test]
    fn the_combined_vocabulary_covers_every_declared_span() {
        let context = Arc::new(InMemoryTelemetryContext::new());
        let starter = agent_span_starter(context).unwrap();
        let mut names: Vec<&str> = starter.span_names().collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec![
                "pi.ai.request",
                "pi.harness.checkpoint",
                "pi.harness.compaction",
                "pi.harness.event_handler",
                "pi.harness.hook",
                "pi.harness.navigation",
                "pi.harness.run",
                "pi.harness.sleep",
                "pi.harness.step",
                "pi.harness.tool",
                "pi.harness.turn",
                "pi.session.write",
            ]
        );
    }

    #[test]
    fn an_ai_request_span_records_its_start_attributes() {
        let context = Arc::new(InMemoryTelemetryContext::new());
        let span = start_ai_span(
            context.clone(),
            AI_SPAN_REQUEST,
            attrs(&[
                ("pi.ai.operation", AttributeValue::String("stream".into())),
                ("pi.ai.provider", AttributeValue::String("openai".into())),
                ("pi.ai.model", AttributeValue::String("mock".into())),
                (
                    "pi.ai.api",
                    AttributeValue::String("openai-responses".into()),
                ),
                ("pi.ai.streaming", AttributeValue::Bool(true)),
            ]),
        )
        .unwrap();
        drop(span);

        let spans = context.spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].name, AI_SPAN_REQUEST);
        assert_eq!(
            spans[0].attributes.get("pi.ai.provider"),
            Some(&AttributeValue::String("openai".into()))
        );
    }

    #[test]
    fn a_missing_required_attribute_is_rejected() {
        let context = Arc::new(InMemoryTelemetryContext::new());
        let error = start_ai_span(
            context,
            AI_SPAN_REQUEST,
            attrs(&[("pi.ai.operation", AttributeValue::String("stream".into()))]),
        )
        .unwrap_err();
        assert!(error.to_string().contains("pi.ai."), "{error}");
    }

    #[test]
    fn a_value_outside_the_closed_set_is_rejected() {
        let context = Arc::new(InMemoryTelemetryContext::new());
        let starter = agent_span_starter(context).unwrap();
        let error = starter
            .start_span(
                HARNESS_SPAN_HOOK,
                attrs(&[
                    ("pi.lane.name", AttributeValue::String("main".into())),
                    ("pi.hook.name", AttributeValue::String("not_a_hook".into())),
                ]),
            )
            .unwrap_err();
        assert!(error.to_string().contains("pi.hook.name"), "{error}");
    }

    /// The `HOOK_NAMES` / `EVENT_TYPES` consts and the closed value sets in the
    /// schema JSON are two copies of the same upstream list; keep them in step.
    #[test]
    fn hook_and_event_name_sets_match_the_schema() {
        let values = |span: &str, attribute: &str| -> Vec<String> {
            harness_telemetry_schema().spans[span].start_attributes[attribute]
                .definition
                .values
                .clone()
                .expect("closed value set")
                .iter()
                .map(|v| match v {
                    AttributeValue::String(s) => s.clone(),
                    other => panic!("expected a string value, got {other:?}"),
                })
                .collect()
        };
        assert_eq!(values(HARNESS_SPAN_HOOK, "pi.hook.name"), HOOK_NAMES);
        assert_eq!(
            values(HARNESS_SPAN_EVENT_HANDLER, "pi.event.type"),
            EVENT_TYPES
        );
    }
}
