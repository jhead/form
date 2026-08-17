//! Schema validation errors.
//!
//! Upstream has no runtime schema errors at all: `defineTelemetrySchema` is a
//! typed identity function and every rule is enforced by the TypeScript type
//! checker. The Rust port has no such checker, so the same rules are enforced
//! when a typed span starts and this enum is what they report.
//!
//! Flat and code-tagged like `pi_core::AiError`: FFI consumers match on
//! [`TelemetryError::code`].

use serde::{Deserialize, Serialize};

use crate::schema::AttributeType;

/// Where an attribute was declared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttributeScope {
    Start,
    End,
    Event(String),
}

impl std::fmt::Display for AttributeScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AttributeScope::Start => f.write_str("start"),
            AttributeScope::End => f.write_str("end"),
            AttributeScope::Event(name) => write!(f, "event `{name}`"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TelemetryError {
    /// Two schemas in one vocabulary declare the same span name. Upstream
    /// rejects this at compile time via `UniqueTelemetrySchemas`.
    #[error("duplicate telemetry span name `{name}`")]
    DuplicateSpanName { name: String },

    #[error("no telemetry span named `{name}` in this schema vocabulary")]
    UnknownSpan { name: String },

    #[error("span `{span}` declares no event named `{event}`")]
    UnknownEvent { span: String, event: String },

    #[error("span `{span}` declares no {scope} attribute named `{attribute}`")]
    UnknownAttribute {
        span: String,
        scope: AttributeScope,
        attribute: String,
    },

    #[error("span `{span}` requires {scope} attribute `{attribute}`")]
    MissingAttribute {
        span: String,
        scope: AttributeScope,
        attribute: String,
    },

    #[error("{scope} attribute `{attribute}` of span `{span}` expects type `{expected}`, got `{actual}`")]
    AttributeTypeMismatch {
        span: String,
        scope: AttributeScope,
        attribute: String,
        expected: AttributeType,
        actual: AttributeType,
    },

    #[error("{scope} attribute `{attribute}` of span `{span}` does not allow value {value}")]
    AttributeValueNotAllowed {
        span: String,
        scope: AttributeScope,
        attribute: String,
        value: String,
    },

    /// The span's `parents` rule does not admit the context it was started
    /// from. Upstream treats `parents` as documentation only; the port enforces
    /// it because nothing else can.
    #[error("span `{span}` cannot be started under {parent}")]
    InvalidParent { span: String, parent: String },

    /// The schema data itself is inconsistent, e.g. a closed-set value whose
    /// type does not match the attribute's declared type.
    #[error("invalid telemetry schema: {message}")]
    InvalidSchema { message: String },
}

impl TelemetryError {
    /// Stable machine-readable code. Do not change these strings: FFI callers
    /// depend on them.
    pub fn code(&self) -> &'static str {
        match self {
            TelemetryError::DuplicateSpanName { .. } => "duplicate_span_name",
            TelemetryError::UnknownSpan { .. } => "unknown_span",
            TelemetryError::UnknownEvent { .. } => "unknown_event",
            TelemetryError::UnknownAttribute { .. } => "unknown_attribute",
            TelemetryError::MissingAttribute { .. } => "missing_attribute",
            TelemetryError::AttributeTypeMismatch { .. } => "attribute_type_mismatch",
            TelemetryError::AttributeValueNotAllowed { .. } => "attribute_value_not_allowed",
            TelemetryError::InvalidParent { .. } => "invalid_parent",
            TelemetryError::InvalidSchema { .. } => "invalid_schema",
        }
    }
}
