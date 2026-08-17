//! Telemetry contracts: spans, events, typed schemas, and the in-memory
//! reference adapter used by the conformance tests.
//!
//! Port of `@earendil-works/pi-telemetry` (`.upstream/packages/telemetry/src/`):
//! `index.ts` maps to this file plus [`schema`], `memory.ts` to [`memory`],
//! `noop.ts` to [`noop`] and `testing/` to [`testing`].
//!
//! ## Shape of the port
//!
//! Upstream's contract is callback-shaped — `startSpan(options, callback)` owns
//! settlement and there is no public `end()`. TypeScript needs that because it
//! has no destructors. Rust does, so the callback contract is expressed as an
//! RAII [`SpanGuard`] layered over an object-safe sink trait:
//!
//! - [`TelemetryContext`] is the object-safe extension point, used as
//!   `Arc<dyn TelemetryContext>`. It has exactly one method, [`TelemetryContext::start_span`].
//! - [`TelemetrySpan`] is what an adapter implements for one live span. Callers
//!   never name it directly; they hold a [`Span`], a cheap `Arc` handle that is
//!   itself a `TelemetryContext` (upstream's `TelemetrySpan extends TelemetryContext`).
//! - [`TelemetryContextExt`] adds the scoped helpers (`in_span`, `try_in_span`
//!   and their async twins) with the same observable semantics as upstream's
//!   callback: the span is created synchronously, the callback runs exactly
//!   once, the result is returned unchanged and the span settles as `error`
//!   when the callback returns `Err` or unwinds.
//!
//! ## Attribute typing
//!
//! Upstream encodes attribute typing in the type system (`InferEventAttributes`
//! and friends). Rust cannot replicate that ergonomically, and generics on the
//! public surface would poison the Swift FFI bridge. Schemas are therefore
//! *data* ([`TelemetrySchema`] / [`SpanDefinition`] / [`AttributeDefinition`])
//! validated when a span starts, with attributes carried as
//! [`SpanAttributes`] = `BTreeMap<String, AttributeValue>`. Span, event and
//! attribute *names* are unchanged: they are the observable contract.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

pub mod error;
pub mod memory;
pub mod noop;
pub mod schema;
pub mod testing;

pub use error::{AttributeScope, TelemetryError};
pub use memory::{InMemoryTelemetryContext, RecordedTelemetryEvent, RecordedTelemetrySpan};
pub use noop::{noop_span, noop_telemetry_context, NoopTelemetryContext};
pub use schema::{
    AttributeCardinality, AttributeDefinition, AttributeType, EventDefinition, ParentDefinition,
    RequiredAttributeDefinition, SpanDefinition, SpanStatusDefault, SpanStatusRule,
    TelemetrySchema, TypedSpan, TypedSpanStarter,
};

/// A value that can be attached to a span or an event.
///
/// Upstream is `string | number | boolean | readonly string[] |
/// readonly number[] | readonly boolean[]`. TypeScript has one `number`; the
/// port keeps integers and floats apart so a JSON round-trip does not turn `1`
/// into `1.0`. Both map back to the schema type [`AttributeType::Number`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AttributeValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    BoolArray(Vec<bool>),
    IntArray(Vec<i64>),
    FloatArray(Vec<f64>),
    StringArray(Vec<String>),
}

impl AttributeValue {
    /// The schema type this value satisfies.
    pub fn attribute_type(&self) -> AttributeType {
        match self {
            AttributeValue::String(_) => AttributeType::String,
            AttributeValue::Int(_) | AttributeValue::Float(_) => AttributeType::Number,
            AttributeValue::Bool(_) => AttributeType::Boolean,
            AttributeValue::StringArray(_) => AttributeType::StringArray,
            AttributeValue::IntArray(_) | AttributeValue::FloatArray(_) => {
                AttributeType::NumberArray
            }
            AttributeValue::BoolArray(_) => AttributeType::BooleanArray,
        }
    }

    /// `true` for an array value with no elements. An empty array carries no
    /// element type, so schema validation accepts it for any array type.
    pub fn is_empty_array(&self) -> bool {
        match self {
            AttributeValue::StringArray(values) => values.is_empty(),
            AttributeValue::IntArray(values) => values.is_empty(),
            AttributeValue::FloatArray(values) => values.is_empty(),
            AttributeValue::BoolArray(values) => values.is_empty(),
            _ => false,
        }
    }

    /// The elements of an array value as scalars, for `elementValues` checks.
    pub fn array_elements(&self) -> Option<Vec<AttributeValue>> {
        match self {
            AttributeValue::StringArray(values) => Some(
                values
                    .iter()
                    .cloned()
                    .map(AttributeValue::String)
                    .collect::<Vec<_>>(),
            ),
            AttributeValue::IntArray(values) => {
                Some(values.iter().copied().map(AttributeValue::Int).collect())
            }
            AttributeValue::FloatArray(values) => {
                Some(values.iter().copied().map(AttributeValue::Float).collect())
            }
            AttributeValue::BoolArray(values) => {
                Some(values.iter().copied().map(AttributeValue::Bool).collect())
            }
            _ => None,
        }
    }
}

impl fmt::Display for AttributeValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match serde_json::to_string(self) {
            Ok(rendered) => f.write_str(&rendered),
            Err(_) => f.write_str("<unrenderable>"),
        }
    }
}

macro_rules! attribute_value_from {
    ($($source:ty => |$binding:ident| $body:expr),+ $(,)?) => {
        $(
            impl From<$source> for AttributeValue {
                fn from($binding: $source) -> Self {
                    $body
                }
            }
        )+
    };
}

attribute_value_from! {
    bool => |value| AttributeValue::Bool(value),
    i32 => |value| AttributeValue::Int(i64::from(value)),
    i64 => |value| AttributeValue::Int(value),
    u32 => |value| AttributeValue::Int(i64::from(value)),
    usize => |value| AttributeValue::Int(value as i64),
    f64 => |value| AttributeValue::Float(value),
    String => |value| AttributeValue::String(value),
    &str => |value| AttributeValue::String(value.to_string()),
    Vec<bool> => |value| AttributeValue::BoolArray(value),
    Vec<i64> => |value| AttributeValue::IntArray(value),
    Vec<f64> => |value| AttributeValue::FloatArray(value),
    Vec<String> => |value| AttributeValue::StringArray(value),
    Vec<&str> => |value| AttributeValue::StringArray(value.into_iter().map(str::to_string).collect()),
    &[&str] => |value| AttributeValue::StringArray(value.iter().map(|item| (*item).to_string()).collect()),
}

/// Open attribute bag. Upstream's `SpanAttributes` allows `undefined` values
/// and ignores them; in Rust an absent value is simply an absent key, which
/// gives the same merge behaviour.
pub type SpanAttributes = BTreeMap<String, AttributeValue>;

/// Build a [`SpanAttributes`] map: `span_attributes! { "a" => 1, "b" => "x" }`.
#[macro_export]
macro_rules! span_attributes {
    () => { $crate::SpanAttributes::new() };
    ($($name:expr => $value:expr),+ $(,)?) => {{
        let mut attributes = $crate::SpanAttributes::new();
        $(
            attributes.insert(
                ::std::string::String::from($name),
                $crate::AttributeValue::from($value),
            );
        )+
        attributes
    }};
}

/// Name and start attributes for a span.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpanOptions {
    pub name: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: SpanAttributes,
}

impl SpanOptions {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            attributes: SpanAttributes::new(),
        }
    }

    pub fn with_attributes(mut self, attributes: SpanAttributes) -> Self {
        self.attributes.extend(attributes);
        self
    }

    pub fn with_attribute(
        mut self,
        name: impl Into<String>,
        value: impl Into<AttributeValue>,
    ) -> Self {
        self.attributes.insert(name.into(), value.into());
        self
    }
}

impl From<&str> for SpanOptions {
    fn from(name: &str) -> Self {
        SpanOptions::new(name)
    }
}

impl From<String> for SpanOptions {
    fn from(name: String) -> Self {
        SpanOptions::new(name)
    }
}

/// Error detail attached to an `error` status. Mirrors upstream's
/// `{ name, message }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanError {
    pub name: String,
    pub message: String,
}

impl SpanError {
    pub fn new(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            message: message.into(),
        }
    }

    /// Derive `{ name, message }` from any displayable error, the way upstream
    /// derives it from a JS `Error`. The name is the error type's short path
    /// segment, which is the closest stable analogue of `error.name`.
    pub fn from_error<E>(error: &E) -> Self
    where
        E: fmt::Display + ?Sized,
    {
        Self {
            name: short_type_name::<E>(),
            message: error.to_string(),
        }
    }
}

fn short_type_name<E: ?Sized>() -> String {
    let full = std::any::type_name::<E>();
    let without_generics = full.split('<').next().unwrap_or(full);
    without_generics
        .rsplit("::")
        .next()
        .unwrap_or(without_generics)
        .to_string()
}

/// A span's outcome. `{ status: "ok" } | { status: "error", error?: {...} }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum SpanStatus {
    #[default]
    Ok,
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<SpanError>,
    },
}

impl SpanStatus {
    pub fn error(error: SpanError) -> Self {
        SpanStatus::Error { error: Some(error) }
    }

    pub fn is_error(&self) -> bool {
        matches!(self, SpanStatus::Error { .. })
    }
}

/// How a span finished. The Rust stand-in for upstream's
/// `settleSpan(failed, error)`: a failure only overwrites the status when the
/// caller never set one explicitly.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SpanOutcome {
    #[default]
    Success,
    Failure(Option<SpanError>),
}

/// The object-safe extension point: something new spans can be started from.
///
/// Held as `Arc<dyn TelemetryContext>`. Implement it on an adapter root; a
/// live [`Span`] also implements it, which is how explicit parentage is
/// propagated (upstream: `TelemetrySpan extends TelemetryContext`).
pub trait TelemetryContext: Send + Sync + 'static {
    /// Start a child span. Adapters must not fail or block: recording is
    /// passive diagnostics and must never change whether the caller's work
    /// runs, succeeds or fails.
    fn start_span(&self, options: SpanOptions) -> Span;
}

/// The recording sink an adapter implements for one live span.
///
/// Every method is passive: it must not panic, must ignore payloads it cannot
/// use, and must be inert once the span has been settled by [`TelemetrySpan::end`].
pub trait TelemetrySpan: Send + Sync + 'static {
    fn start_span(&self, options: SpanOptions) -> Span;
    fn add_event(&self, name: &str, attributes: SpanAttributes);
    fn set_attributes(&self, attributes: SpanAttributes);
    /// Last write wins. An explicit status is never overwritten by the
    /// automatic status derived at settlement.
    fn set_status(&self, status: SpanStatus);
    /// Settle the span. Idempotent; all later calls are inert.
    fn end(&self, outcome: SpanOutcome);
}

/// A handle to a live span: a cheap `Arc` clone that is also a parent context.
///
/// Prefer the scoped helpers on [`TelemetryContextExt`] over calling
/// [`Span::end`] by hand — they settle the span on every path, including a
/// panic.
#[derive(Clone)]
pub struct Span(Arc<dyn TelemetrySpan>);

impl Span {
    pub fn new(sink: Arc<dyn TelemetrySpan>) -> Self {
        Self(sink)
    }

    /// `true` when both handles point at the same recording sink.
    pub fn ptr_eq(left: &Span, right: &Span) -> bool {
        Arc::ptr_eq(&left.0, &right.0)
    }

    pub fn add_event(&self, name: &str, attributes: SpanAttributes) {
        self.0.add_event(name, attributes);
    }

    pub fn set_attributes(&self, attributes: SpanAttributes) {
        self.0.set_attributes(attributes);
    }

    pub fn set_status(&self, status: SpanStatus) {
        self.0.set_status(status);
    }

    pub fn end(&self, outcome: SpanOutcome) {
        self.0.end(outcome);
    }

    /// This span as a parent context, for APIs that take
    /// `Arc<dyn TelemetryContext>`.
    pub fn as_context(&self) -> Arc<dyn TelemetryContext> {
        Arc::new(self.clone())
    }
}

impl TelemetryContext for Span {
    fn start_span(&self, options: SpanOptions) -> Span {
        self.0.start_span(options)
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Span")
    }
}

/// RAII scope for one span: settles on drop, as `error` if the thread is
/// unwinding or [`SpanGuard::fail`] was called.
///
/// This replaces upstream's callback-owned settlement. Dropping the guard is
/// the equivalent of the callback's promise settling.
pub struct SpanGuard {
    span: Span,
    outcome: Mutex<SpanOutcome>,
}

impl SpanGuard {
    pub fn new(span: Span) -> Self {
        Self {
            span,
            outcome: Mutex::new(SpanOutcome::Success),
        }
    }

    pub fn span(&self) -> Span {
        self.span.clone()
    }

    pub fn add_event(&self, name: &str, attributes: SpanAttributes) {
        self.span.add_event(name, attributes);
    }

    pub fn set_attributes(&self, attributes: SpanAttributes) {
        self.span.set_attributes(attributes);
    }

    pub fn set_status(&self, status: SpanStatus) {
        self.span.set_status(status);
    }

    /// Settle as `error` with detail, unless an explicit status was set.
    pub fn fail(&self, error: SpanError) {
        *self.outcome.lock() = SpanOutcome::Failure(Some(error));
    }

    /// Settle as `error` without detail.
    pub fn fail_without_detail(&self) {
        *self.outcome.lock() = SpanOutcome::Failure(None);
    }

    /// Open a child span scope under this one.
    pub fn child(&self, options: SpanOptions) -> SpanGuard {
        SpanGuard::new(self.span.start_span(options))
    }
}

impl TelemetryContext for SpanGuard {
    fn start_span(&self, options: SpanOptions) -> Span {
        self.span.start_span(options)
    }
}

impl fmt::Debug for SpanGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SpanGuard")
    }
}

impl Drop for SpanGuard {
    fn drop(&mut self) {
        let outcome = {
            let mut slot = self.outcome.lock();
            std::mem::take(&mut *slot)
        };
        // An unwind is this port's equivalent of upstream's synchronous throw.
        let outcome = match outcome {
            SpanOutcome::Success if std::thread::panicking() => SpanOutcome::Failure(None),
            other => other,
        };
        self.span.end(outcome);
    }
}

/// Scoped helpers over any [`TelemetryContext`], including `dyn TelemetryContext`,
/// [`Span`] and [`SpanGuard`].
///
/// These are Rust-side ergonomics layered on the object-safe trait; the FFI
/// surface is [`TelemetryContext`] itself.
pub trait TelemetryContextExt: TelemetryContext {
    /// Open a span scope. The span settles when the guard drops.
    fn enter_span(&self, options: SpanOptions) -> SpanGuard {
        SpanGuard::new(self.start_span(options))
    }

    /// Run `f` inside a span. The span is created and `f` invoked
    /// synchronously, exactly once; the value is returned unchanged.
    fn in_span<R>(&self, options: SpanOptions, f: impl FnOnce(Span) -> R) -> R {
        let guard = self.enter_span(options);
        f(guard.span())
    }

    /// Run `f` inside a span, settling the span as `error` when it returns
    /// `Err`. The error value is returned unchanged.
    fn try_in_span<T, E>(
        &self,
        options: SpanOptions,
        f: impl FnOnce(Span) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: fmt::Display,
    {
        let guard = self.enter_span(options);
        let result = f(guard.span());
        if let Err(error) = &result {
            guard.fail(SpanError::from_error(error));
        }
        result
    }

    /// Async [`TelemetryContextExt::in_span`]. The span starts when this is
    /// called, matching upstream's synchronous admission, and settles when the
    /// returned future completes or is dropped.
    fn in_span_async<F, Fut, T>(&self, options: SpanOptions, f: F) -> impl Future<Output = T> + Send
    where
        F: FnOnce(Span) -> Fut + Send,
        Fut: Future<Output = T> + Send,
        T: Send,
    {
        let guard = self.enter_span(options);
        async move {
            let value = f(guard.span()).await;
            drop(guard);
            value
        }
    }

    /// Async [`TelemetryContextExt::try_in_span`].
    fn try_in_span_async<F, Fut, T, E>(
        &self,
        options: SpanOptions,
        f: F,
    ) -> impl Future<Output = Result<T, E>> + Send
    where
        F: FnOnce(Span) -> Fut + Send,
        Fut: Future<Output = Result<T, E>> + Send,
        T: Send,
        E: fmt::Display + Send,
    {
        let guard = self.enter_span(options);
        async move {
            let result = f(guard.span()).await;
            if let Err(error) = &result {
                guard.fail(SpanError::from_error(error));
            }
            drop(guard);
            result
        }
    }
}

impl<T: TelemetryContext + ?Sized> TelemetryContextExt for T {}
