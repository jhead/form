//! Telemetry schemas as data, and the typed span starter that validates
//! against them.
//!
//! Port of the schema half of `.upstream/packages/telemetry/src/index.ts`.
//!
//! Upstream expresses schemas twice: once as serializable data
//! (`TelemetrySchemaDefinition`) and once as a large family of conditional
//! types (`InferStartAttributes`, `InferEventAttributes`,
//! `ExactTelemetryAttributes`, `TypedSpanStarter`, …) that check span names and
//! attribute bags at compile time. Rust has no equivalent, and encoding one
//! would put generic parameters all over the public API, which the Swift FFI
//! bridge forbids. This port keeps the data half verbatim and turns the type
//! half into runtime validation performed by [`TypedSpanStarter`] when a span
//! starts, an event is added or end attributes are set.
//!
//! Everything upstream checks statically is checked here dynamically:
//! duplicate span names across schemas, unknown span names, missing required
//! start/event attributes, unknown attribute keys, closed-set values, and
//! undeclared events. The one addition is [`ParentDefinition`] enforcement,
//! which upstream documents as descriptive metadata only — with no type
//! checker to lean on, enforcing it is the only way that data does any work.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{AttributeScope, TelemetryError};
use crate::{
    AttributeValue, Span, SpanAttributes, SpanError, SpanGuard, SpanOptions, SpanStatus,
    TelemetryContext,
};

/// Supported attribute types. Names match upstream exactly, including the
/// `[]` suffix, because schemas are serialized as JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttributeType {
    #[serde(rename = "string")]
    String,
    #[serde(rename = "number")]
    Number,
    #[serde(rename = "boolean")]
    Boolean,
    #[serde(rename = "string[]")]
    StringArray,
    #[serde(rename = "number[]")]
    NumberArray,
    #[serde(rename = "boolean[]")]
    BooleanArray,
}

impl AttributeType {
    pub fn as_str(&self) -> &'static str {
        match self {
            AttributeType::String => "string",
            AttributeType::Number => "number",
            AttributeType::Boolean => "boolean",
            AttributeType::StringArray => "string[]",
            AttributeType::NumberArray => "number[]",
            AttributeType::BooleanArray => "boolean[]",
        }
    }

    pub fn is_array(&self) -> bool {
        matches!(
            self,
            AttributeType::StringArray | AttributeType::NumberArray | AttributeType::BooleanArray
        )
    }

    /// The scalar type of this array type's elements.
    pub fn element_type(&self) -> Option<AttributeType> {
        match self {
            AttributeType::StringArray => Some(AttributeType::String),
            AttributeType::NumberArray => Some(AttributeType::Number),
            AttributeType::BooleanArray => Some(AttributeType::Boolean),
            _ => None,
        }
    }
}

impl fmt::Display for AttributeType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AttributeCardinality {
    Low,
    High,
}

/// One attribute's type, closed value set, examples and metadata.
///
/// Upstream is a discriminated union over `type` where each arm carries either
/// `values` (scalars) or `elementValues` (arrays), intersected with the shared
/// metadata. The wire shape of any valid instance is identical to this flat
/// struct; [`TelemetrySchema::validate`] rejects the combinations the union
/// makes unrepresentable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributeDefinition {
    #[serde(rename = "type")]
    pub attribute_type: AttributeType,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitive: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cardinality: Option<AttributeCardinality>,
    /// Closed set for a scalar attribute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<AttributeValue>>,
    /// Closed set for the elements of an array attribute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element_values: Option<Vec<AttributeValue>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<AttributeValue>,
}

impl AttributeDefinition {
    pub fn new(attribute_type: AttributeType, description: impl Into<String>) -> Self {
        Self {
            attribute_type,
            description: description.into(),
            sensitive: None,
            cardinality: None,
            values: None,
            element_values: None,
            examples: Vec::new(),
        }
    }

    pub fn string(description: impl Into<String>) -> Self {
        Self::new(AttributeType::String, description)
    }

    pub fn number(description: impl Into<String>) -> Self {
        Self::new(AttributeType::Number, description)
    }

    pub fn boolean(description: impl Into<String>) -> Self {
        Self::new(AttributeType::Boolean, description)
    }

    pub fn with_values(mut self, values: impl IntoIterator<Item = AttributeValue>) -> Self {
        self.values = Some(values.into_iter().collect());
        self
    }

    pub fn with_element_values(mut self, values: impl IntoIterator<Item = AttributeValue>) -> Self {
        self.element_values = Some(values.into_iter().collect());
        self
    }

    pub fn with_examples(mut self, examples: impl IntoIterator<Item = AttributeValue>) -> Self {
        self.examples = examples.into_iter().collect();
        self
    }

    pub fn sensitive(mut self) -> Self {
        self.sensitive = Some(true);
        self
    }

    pub fn with_cardinality(mut self, cardinality: AttributeCardinality) -> Self {
        self.cardinality = Some(cardinality);
        self
    }

    /// Turn this into a start or event attribute definition.
    pub fn required(self, required: bool) -> RequiredAttributeDefinition {
        RequiredAttributeDefinition {
            definition: self,
            required,
        }
    }
}

/// A start or event attribute: an [`AttributeDefinition`] plus requiredness.
/// End attributes are always optional and use [`AttributeDefinition`] directly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequiredAttributeDefinition {
    #[serde(flatten)]
    pub definition: AttributeDefinition,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventDefinition {
    pub description: String,
    #[serde(default)]
    pub attributes: BTreeMap<String, RequiredAttributeDefinition>,
}

impl EventDefinition {
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            attributes: BTreeMap::new(),
        }
    }

    pub fn with_attribute(
        mut self,
        name: impl Into<String>,
        definition: RequiredAttributeDefinition,
    ) -> Self {
        self.attributes.insert(name.into(), definition);
        self
    }
}

/// Which contexts a span may be started from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParentDefinition {
    /// Root, or any caller span.
    Any,
    /// Root, or a caller-owned span outside this vocabulary.
    RootOrExternal,
    /// Only the listed schema spans.
    Spans { spans: Vec<String> },
}

impl ParentDefinition {
    pub fn spans(names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        ParentDefinition::Spans {
            spans: names.into_iter().map(Into::into).collect(),
        }
    }

    /// `parent` is the name of the enclosing schema span, or `None` for a root
    /// or externally owned parent.
    fn admits(&self, parent: Option<&str>) -> bool {
        match self {
            ParentDefinition::Any => true,
            ParentDefinition::RootOrExternal => parent.is_none(),
            ParentDefinition::Spans { spans } => {
                parent.is_some_and(|name| spans.iter().any(|allowed| allowed == name))
            }
        }
    }
}

/// Upstream's `status: { default: "ok", errorWhen: string }`. `default` is a
/// literal `"ok"` upstream, so it is a unit enum here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpanStatusDefault {
    Ok,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpanStatusRule {
    pub default: SpanStatusDefault,
    /// Prose describing when the span is an error.
    pub error_when: String,
}

impl SpanStatusRule {
    pub fn new(error_when: impl Into<String>) -> Self {
        Self {
            default: SpanStatusDefault::Ok,
            error_when: error_when.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpanDefinition {
    pub description: String,
    pub parents: ParentDefinition,
    #[serde(default)]
    pub start_attributes: BTreeMap<String, RequiredAttributeDefinition>,
    #[serde(default)]
    pub end_attributes: BTreeMap<String, AttributeDefinition>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub events: BTreeMap<String, EventDefinition>,
    pub status: SpanStatusRule,
}

impl SpanDefinition {
    pub fn new(
        description: impl Into<String>,
        parents: ParentDefinition,
        status: SpanStatusRule,
    ) -> Self {
        Self {
            description: description.into(),
            parents,
            start_attributes: BTreeMap::new(),
            end_attributes: BTreeMap::new(),
            events: BTreeMap::new(),
            status,
        }
    }

    pub fn with_start_attribute(
        mut self,
        name: impl Into<String>,
        definition: RequiredAttributeDefinition,
    ) -> Self {
        self.start_attributes.insert(name.into(), definition);
        self
    }

    pub fn with_end_attribute(
        mut self,
        name: impl Into<String>,
        definition: AttributeDefinition,
    ) -> Self {
        self.end_attributes.insert(name.into(), definition);
        self
    }

    pub fn with_event(mut self, name: impl Into<String>, definition: EventDefinition) -> Self {
        self.events.insert(name.into(), definition);
        self
    }
}

/// A versioned set of span definitions.
///
/// The Rust replacement for `defineTelemetrySchema()`, which is a typed
/// identity function with no runtime behaviour: construct the struct (or
/// deserialize it with [`TelemetrySchema::from_json`]) and call
/// [`TelemetrySchema::validate`] if the data came from outside the binary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetrySchema {
    pub version: u32,
    #[serde(default)]
    pub spans: BTreeMap<String, SpanDefinition>,
}

impl TelemetrySchema {
    pub fn new(version: u32) -> Self {
        Self {
            version,
            spans: BTreeMap::new(),
        }
    }

    pub fn with_span(mut self, name: impl Into<String>, definition: SpanDefinition) -> Self {
        self.spans.insert(name.into(), definition);
        self
    }

    pub fn from_json(json: &str) -> Result<Self, TelemetryError> {
        let schema: TelemetrySchema =
            serde_json::from_str(json).map_err(|error| TelemetryError::InvalidSchema {
                message: error.to_string(),
            })?;
        schema.validate()?;
        Ok(schema)
    }

    /// Check the internal consistency the TypeScript union enforces at compile
    /// time: closed-set values must match the declared attribute type, and
    /// `values` / `elementValues` must be used on scalars / arrays respectively.
    pub fn validate(&self) -> Result<(), TelemetryError> {
        for (span_name, span) in &self.spans {
            for (name, attribute) in &span.start_attributes {
                validate_definition(span_name, name, &attribute.definition)?;
            }
            for (name, attribute) in &span.end_attributes {
                validate_definition(span_name, name, attribute)?;
            }
            for event in span.events.values() {
                for (name, attribute) in &event.attributes {
                    validate_definition(span_name, name, &attribute.definition)?;
                }
            }
        }
        Ok(())
    }
}

fn invalid_schema(message: impl Into<String>) -> TelemetryError {
    TelemetryError::InvalidSchema {
        message: message.into(),
    }
}

fn validate_definition(
    span: &str,
    attribute: &str,
    definition: &AttributeDefinition,
) -> Result<(), TelemetryError> {
    let declared = definition.attribute_type;
    if let Some(values) = &definition.values {
        if declared.is_array() {
            return Err(invalid_schema(format!(
                "attribute `{attribute}` of span `{span}` is `{declared}`; array attributes use `elementValues`"
            )));
        }
        for value in values {
            if value.attribute_type() != declared {
                return Err(invalid_schema(format!(
                    "attribute `{attribute}` of span `{span}` allows value {value}, which is not `{declared}`"
                )));
            }
        }
    }
    if let Some(values) = &definition.element_values {
        let Some(element) = declared.element_type() else {
            return Err(invalid_schema(format!(
                "attribute `{attribute}` of span `{span}` is `{declared}`; scalar attributes use `values`"
            )));
        };
        for value in values {
            if value.attribute_type() != element {
                return Err(invalid_schema(format!(
                    "attribute `{attribute}` of span `{span}` allows element {value}, which is not `{element}`"
                )));
            }
        }
    }
    Ok(())
}

/// The combined span vocabulary of one or more schemas.
#[derive(Debug)]
struct Vocabulary {
    spans: BTreeMap<String, Arc<SpanDefinition>>,
}

impl Vocabulary {
    fn build(schemas: Vec<TelemetrySchema>) -> Result<Self, TelemetryError> {
        let mut spans: BTreeMap<String, Arc<SpanDefinition>> = BTreeMap::new();
        for schema in schemas {
            schema.validate()?;
            for (name, definition) in schema.spans {
                if spans.contains_key(&name) {
                    return Err(TelemetryError::DuplicateSpanName { name });
                }
                spans.insert(name, Arc::new(definition));
            }
        }
        Ok(Self { spans })
    }
}

/// Binds a parent context to the combined span vocabulary of one or more
/// schemas, validating every span, event and attribute against them.
///
/// Port of `createTypedSpanStarter()`. Upstream returns an overload set that
/// type-checks names and attribute bags; this returns a value that checks the
/// same rules at runtime. Duplicate span names across schemas are rejected
/// when the starter is built, as upstream's `UniqueTelemetrySchemas` does.
#[derive(Clone)]
pub struct TypedSpanStarter {
    context: Arc<dyn TelemetryContext>,
    vocabulary: Arc<Vocabulary>,
    /// Name of the enclosing schema span, when this starter is bound to one.
    parent: Option<Arc<str>>,
}

impl fmt::Debug for TypedSpanStarter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypedSpanStarter")
            .field("spans", &self.vocabulary.spans.keys())
            .field("parent", &self.parent)
            .finish()
    }
}

impl TypedSpanStarter {
    pub fn new(
        context: Arc<dyn TelemetryContext>,
        schemas: Vec<TelemetrySchema>,
    ) -> Result<Self, TelemetryError> {
        Ok(Self {
            context,
            vocabulary: Arc::new(Vocabulary::build(schemas)?),
            parent: None,
        })
    }

    /// Span names in the combined vocabulary.
    pub fn span_names(&self) -> impl Iterator<Item = &str> {
        self.vocabulary.spans.keys().map(String::as_str)
    }

    pub fn definition(&self, name: &str) -> Option<&SpanDefinition> {
        self.vocabulary.spans.get(name).map(Arc::as_ref)
    }

    /// Start a validated span. The span settles when the returned [`TypedSpan`]
    /// drops.
    ///
    /// A validation failure means the caller's instrumentation disagrees with
    /// its schema — a programmer error, not a runtime condition. Callers that
    /// must never fail on telemetry should ignore the error rather than
    /// propagate it.
    pub fn start_span(
        &self,
        name: &str,
        attributes: SpanAttributes,
    ) -> Result<TypedSpan, TelemetryError> {
        let definition = self
            .vocabulary
            .spans
            .get(name)
            .ok_or_else(|| TelemetryError::UnknownSpan {
                name: name.to_string(),
            })?
            .clone();

        if !definition.parents.admits(self.parent.as_deref()) {
            return Err(TelemetryError::InvalidParent {
                span: name.to_string(),
                parent: match self.parent.as_deref() {
                    Some(parent) => format!("span `{parent}`"),
                    None => "a root or external context".to_string(),
                },
            });
        }

        check_required_attributes(
            name,
            &AttributeScope::Start,
            &definition.start_attributes,
            &attributes,
        )?;

        let span = self
            .context
            .start_span(SpanOptions::new(name).with_attributes(attributes));
        let name: Arc<str> = Arc::from(name);
        Ok(TypedSpan {
            children: Self {
                context: span.as_context(),
                vocabulary: self.vocabulary.clone(),
                parent: Some(name.clone()),
            },
            guard: SpanGuard::new(span),
            name,
            definition,
        })
    }

    /// Start a validated span and run `f` inside it.
    pub fn in_span<R>(
        &self,
        name: &str,
        attributes: SpanAttributes,
        f: impl FnOnce(&TypedSpan) -> R,
    ) -> Result<R, TelemetryError> {
        let span = self.start_span(name, attributes)?;
        Ok(f(&span))
    }

    /// The untyped context this starter records into.
    pub fn context(&self) -> Arc<dyn TelemetryContext> {
        self.context.clone()
    }
}

/// A live span validated against a schema. Settles on drop.
pub struct TypedSpan {
    guard: SpanGuard,
    name: Arc<str>,
    definition: Arc<SpanDefinition>,
    children: TypedSpanStarter,
}

impl fmt::Debug for TypedSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypedSpan")
            .field("name", &self.name)
            .finish()
    }
}

impl TypedSpan {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn definition(&self) -> &SpanDefinition {
        &self.definition
    }

    /// The untyped handle, for passing to code that takes a plain context.
    pub fn span(&self) -> Span {
        self.guard.span()
    }

    /// A starter over the same vocabulary, bound to this span as parent.
    /// Upstream passes the equivalent as the callback's `startChildSpan`.
    pub fn children(&self) -> &TypedSpanStarter {
        &self.children
    }

    /// Start a validated child span.
    pub fn start_span(
        &self,
        name: &str,
        attributes: SpanAttributes,
    ) -> Result<TypedSpan, TelemetryError> {
        self.children.start_span(name, attributes)
    }

    /// Record an event declared by this span.
    pub fn add_event(&self, name: &str, attributes: SpanAttributes) -> Result<(), TelemetryError> {
        let event =
            self.definition
                .events
                .get(name)
                .ok_or_else(|| TelemetryError::UnknownEvent {
                    span: self.name.to_string(),
                    event: name.to_string(),
                })?;
        check_required_attributes(
            &self.name,
            &AttributeScope::Event(name.to_string()),
            &event.attributes,
            &attributes,
        )?;
        self.guard.add_event(name, attributes);
        Ok(())
    }

    /// Merge completion attributes. Only this span's declared `endAttributes`
    /// are accepted, matching upstream's schema-scoped `setAttributes`.
    pub fn set_attributes(&self, attributes: SpanAttributes) -> Result<(), TelemetryError> {
        check_optional_attributes(
            &self.name,
            &AttributeScope::End,
            &self.definition.end_attributes,
            &attributes,
        )?;
        self.guard.set_attributes(attributes);
        Ok(())
    }

    pub fn set_status(&self, status: SpanStatus) {
        self.guard.set_status(status);
    }

    pub fn fail(&self, error: SpanError) {
        self.guard.fail(error);
    }
}

impl TelemetryContext for TypedSpan {
    fn start_span(&self, options: SpanOptions) -> Span {
        self.guard.start_span(options)
    }
}

fn check_required_attributes(
    span: &str,
    scope: &AttributeScope,
    declared: &BTreeMap<String, RequiredAttributeDefinition>,
    attributes: &SpanAttributes,
) -> Result<(), TelemetryError> {
    for (name, value) in attributes {
        let Some(definition) = declared.get(name) else {
            return Err(TelemetryError::UnknownAttribute {
                span: span.to_string(),
                scope: scope.clone(),
                attribute: name.clone(),
            });
        };
        check_value(span, scope, name, &definition.definition, value)?;
    }
    for (name, definition) in declared {
        if definition.required && !attributes.contains_key(name) {
            return Err(TelemetryError::MissingAttribute {
                span: span.to_string(),
                scope: scope.clone(),
                attribute: name.clone(),
            });
        }
    }
    Ok(())
}

fn check_optional_attributes(
    span: &str,
    scope: &AttributeScope,
    declared: &BTreeMap<String, AttributeDefinition>,
    attributes: &SpanAttributes,
) -> Result<(), TelemetryError> {
    for (name, value) in attributes {
        let Some(definition) = declared.get(name) else {
            return Err(TelemetryError::UnknownAttribute {
                span: span.to_string(),
                scope: scope.clone(),
                attribute: name.clone(),
            });
        };
        check_value(span, scope, name, definition, value)?;
    }
    Ok(())
}

fn check_value(
    span: &str,
    scope: &AttributeScope,
    attribute: &str,
    definition: &AttributeDefinition,
    value: &AttributeValue,
) -> Result<(), TelemetryError> {
    let declared = definition.attribute_type;
    let actual = value.attribute_type();
    // An empty array has no element type, so it satisfies any array type.
    let compatible = actual == declared || (declared.is_array() && value.is_empty_array());
    if !compatible {
        return Err(TelemetryError::AttributeTypeMismatch {
            span: span.to_string(),
            scope: scope.clone(),
            attribute: attribute.to_string(),
            expected: declared,
            actual,
        });
    }

    if let Some(allowed) = &definition.values {
        if !allowed.contains(value) {
            return Err(TelemetryError::AttributeValueNotAllowed {
                span: span.to_string(),
                scope: scope.clone(),
                attribute: attribute.to_string(),
                value: value.to_string(),
            });
        }
    }

    if let (Some(allowed), Some(elements)) = (&definition.element_values, value.array_elements()) {
        for element in elements {
            if !allowed.contains(&element) {
                return Err(TelemetryError::AttributeValueNotAllowed {
                    span: span.to_string(),
                    scope: scope.clone(),
                    attribute: attribute.to_string(),
                    value: element.to_string(),
                });
            }
        }
    }

    Ok(())
}
