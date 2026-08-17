//! JSON Schema validation and coercion of tool-call arguments.
//! Port of `packages/ai/src/utils/validation.ts`.
//!
//! Upstream compiles the tool's TypeBox schema and runs `Value.Convert` before
//! checking. `pi_core::Tool::parameters` is a plain JSON Schema
//! [`serde_json::Value`], which upstream also has to handle (schemas that
//! came off the wire or out of `settings.json` are not TypeBox objects), so this
//! port implements that branch — the AJV-compatible primitive coercion rules —
//! and validates with the `jsonschema` crate.
//!
//! Two passes run before validation, in upstream's order:
//!
//! 1. [`normalize_optional_nulls`] — models routinely emit `"offset": null` for
//!    an omitted optional argument. Drop those, but only when the property is
//!    genuinely not nullable.
//! 2. [`coerce_with_json_schema`] — `"42"` for a `number`, `1` for a `boolean`,
//!    and friends.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use jsonschema::Validator;
use once_cell::sync::Lazy;
use pi_core::content::ToolCall;
use pi_core::tool::Tool;
use serde_json::{Map, Value};

/// Why validating a tool call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolValidationError {
    #[error("Tool \"{name}\" not found")]
    ToolNotFound { name: String },

    /// The tool's own `parameters` are not a usable JSON Schema.
    #[error("Invalid JSON Schema for tool \"{tool}\": {message}")]
    InvalidSchema { tool: String, message: String },

    /// Arguments did not satisfy the schema. `message` is the full multi-line
    /// text upstream feeds back to the model, so it is worth keeping verbatim.
    #[error("{message}")]
    InvalidArguments {
        tool: String,
        message: String,
        /// One `path: reason` entry per failure, in schema order.
        errors: Vec<String>,
    },
}

impl ToolValidationError {
    /// Stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            ToolValidationError::ToolNotFound { .. } => "tool_not_found",
            ToolValidationError::InvalidSchema { .. } => "invalid_tool_schema",
            ToolValidationError::InvalidArguments { .. } => "invalid_tool_arguments",
        }
    }
}

/// Find the tool by name and validate the call's arguments against its schema.
pub fn validate_tool_call(
    tools: &[Tool],
    tool_call: &ToolCall,
) -> Result<Map<String, Value>, ToolValidationError> {
    let tool = tools
        .iter()
        .find(|t| t.name == tool_call.name)
        .ok_or_else(|| ToolValidationError::ToolNotFound {
            name: tool_call.name.clone(),
        })?;
    validate_tool_arguments(tool, tool_call)
}

/// Validate (and coerce) a call's arguments against one tool's schema.
///
/// Returns the coerced arguments on success — callers should execute with these,
/// not with `tool_call.arguments`.
pub fn validate_tool_arguments(
    tool: &Tool,
    tool_call: &ToolCall,
) -> Result<Map<String, Value>, ToolValidationError> {
    let validator =
        get_validator(&tool.parameters).map_err(|message| ToolValidationError::InvalidSchema {
            tool: tool.name.clone(),
            message,
        })?;

    let mut args = Value::Object(tool_call.arguments.clone());
    normalize_optional_nulls(&mut args, &tool.parameters);
    args = coerce_with_json_schema(args, &tool.parameters);

    if validator.is_valid(&args) {
        return Ok(match args {
            Value::Object(map) => map,
            // Coercion of a non-object root cannot happen for tool arguments,
            // but do not silently drop a value the schema accepted.
            other => {
                let mut map = Map::new();
                map.insert("value".to_string(), other);
                map
            }
        });
    }

    let errors: Vec<String> = validator
        .iter_errors(&args)
        .map(|error| format!("  - {}: {}", format_validation_path(&error), error))
        .collect();
    let joined = if errors.is_empty() {
        "Unknown validation error".to_string()
    } else {
        errors.join("\n")
    };
    let received =
        serde_json::to_string_pretty(&tool_call.arguments).unwrap_or_else(|_| "{}".to_string());
    Err(ToolValidationError::InvalidArguments {
        tool: tool.name.clone(),
        message: format!(
            "Validation failed for tool \"{}\":\n{joined}\n\nReceived arguments:\n{received}",
            tool_call.name
        ),
        errors,
    })
}

/// `instancePath` rendered the way upstream renders it: `/a/b` becomes `a.b`,
/// the root becomes `root`, and a missing-property failure names the property.
fn format_validation_path(error: &jsonschema::ValidationError<'_>) -> String {
    let base = error.instance_path.to_string();
    let base = base.strip_prefix('/').unwrap_or(&base).replace('/', ".");
    if let jsonschema::error::ValidationErrorKind::Required { property } = &error.kind {
        if let Some(name) = property.as_str() {
            return if base.is_empty() {
                name.to_string()
            } else {
                format!("{base}.{name}")
            };
        }
    }
    if base.is_empty() {
        "root".to_string()
    } else {
        base
    }
}

// --- validator cache -------------------------------------------------------

/// Upstream keys a `WeakMap` on the schema object identity. There is no
/// equivalent here, so cache on the serialized schema. Compilation is the
/// expensive part and tool schemas are fixed for the life of a session.
/// Compilation result, cached so a schema that fails to compile is not retried
/// on every call either.
type CachedValidator = Result<Arc<Validator>, String>;

static VALIDATOR_CACHE: Lazy<Mutex<HashMap<String, CachedValidator>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

const VALIDATOR_CACHE_LIMIT: usize = 512;

fn get_validator(schema: &Value) -> Result<Arc<Validator>, String> {
    let key = schema.to_string();
    let mut cache = VALIDATOR_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entry) = cache.get(&key) {
        return entry.clone();
    }
    let compiled = jsonschema::validator_for(schema)
        .map(Arc::new)
        .map_err(|e| e.to_string());
    if cache.len() >= VALIDATOR_CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(key, compiled.clone());
    compiled
}

/// A validator for a sub-schema, or `None` when it cannot stand alone (a bare
/// `$ref` into the parent document, for instance).
fn sub_schema_validator(schema: &Value) -> Option<Arc<Validator>> {
    get_validator(schema).ok()
}

// --- null normalization ----------------------------------------------------

fn schema_required(schema: &Value) -> Vec<&str> {
    schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default()
}

/// Delete `null`s that stand for an omitted optional property.
///
/// A `null` is only removed when the property is optional, its schema is not a
/// `$ref` (unresolvable in isolation, so unknowable), and the schema provably
/// rejects `null`.
pub fn normalize_optional_nulls(value: &mut Value, schema: &Value) {
    if let Value::Array(items) = value {
        match schema.get("items") {
            Some(Value::Array(item_schemas)) => {
                for (index, item) in items.iter_mut().enumerate() {
                    if let Some(item_schema) = item_schemas.get(index) {
                        normalize_optional_nulls(item, item_schema);
                    }
                }
            }
            Some(item_schema) => {
                for item in items.iter_mut() {
                    normalize_optional_nulls(item, item_schema);
                }
            }
            None => {}
        }
        return;
    }

    let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) else {
        return;
    };
    let Value::Object(object) = value else {
        return;
    };

    let required = schema_required(schema);
    let mut drop_keys = Vec::new();
    for (key, property_schema) in properties {
        let Some(current) = object.get_mut(key) else {
            continue;
        };
        if current.is_null()
            && !required.contains(&key.as_str())
            && !property_schema.get("$ref").is_some_and(|r| r.is_string())
            && sub_schema_validator(property_schema).is_some_and(|v| !v.is_valid(&Value::Null))
        {
            drop_keys.push(key.clone());
        } else {
            normalize_optional_nulls(current, property_schema);
        }
    }
    for key in drop_keys {
        object.remove(&key);
    }
}

// --- coercion --------------------------------------------------------------

fn schema_types(schema: &Value) -> Vec<String> {
    match schema.get("type") {
        Some(Value::String(t)) => vec![t.clone()],
        Some(Value::Array(list)) => list
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    }
}

fn matches_json_type(value: &Value, ty: &str) -> bool {
    match ty {
        "number" => value.is_number(),
        "integer" => value.as_f64().is_some_and(|n| n.fract() == 0.0),
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "null" => value.is_null(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

/// AJV-compatible primitive coercion. Returns the value unchanged when no rule
/// applies — the caller relies on that to detect "nothing happened".
fn coerce_primitive_by_type(value: &Value, ty: &str) -> Value {
    match ty {
        "number" => {
            if value.is_null() {
                return Value::from(0);
            }
            if let Some(text) = value.as_str() {
                if !text.trim().is_empty() {
                    if let Ok(parsed) = text.trim().parse::<f64>() {
                        if parsed.is_finite() {
                            return number_value(parsed);
                        }
                    }
                }
            }
            if let Some(b) = value.as_bool() {
                return Value::from(if b { 1 } else { 0 });
            }
            value.clone()
        }
        "integer" => {
            if value.is_null() {
                return Value::from(0);
            }
            if let Some(text) = value.as_str() {
                if !text.trim().is_empty() {
                    if let Ok(parsed) = text.trim().parse::<f64>() {
                        if parsed.is_finite() && parsed.fract() == 0.0 {
                            return number_value(parsed);
                        }
                    }
                }
            }
            if let Some(b) = value.as_bool() {
                return Value::from(if b { 1 } else { 0 });
            }
            value.clone()
        }
        "boolean" => {
            if value.is_null() {
                return Value::Bool(false);
            }
            match value.as_str() {
                Some("true") => return Value::Bool(true),
                Some("false") => return Value::Bool(false),
                _ => {}
            }
            if let Some(n) = value.as_f64() {
                if n == 1.0 {
                    return Value::Bool(true);
                }
                if n == 0.0 {
                    return Value::Bool(false);
                }
            }
            value.clone()
        }
        "string" => {
            if value.is_null() {
                return Value::String(String::new());
            }
            if value.is_number() || value.is_boolean() {
                return Value::String(value.to_string());
            }
            value.clone()
        }
        "null" => {
            let empty_string = value.as_str() == Some("");
            let zero = value.as_f64() == Some(0.0);
            let untrue = value.as_bool() == Some(false);
            if empty_string || zero || untrue {
                return Value::Null;
            }
            value.clone()
        }
        _ => value.clone(),
    }
}

/// Prefer an integer representation so `"42"` becomes `42`, not `42.0`, which
/// matters for schemas with `"type": "integer"` and for the wire payload.
fn number_value(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9.007_199_254_740_992e15 {
        Value::from(n as i64)
    } else {
        serde_json::Number::from_f64(n)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    }
}

fn coerce_with_union_schema(value: Value, schemas: &[Value]) -> Value {
    // An arm that already accepts the value wins outright, so a legitimate
    // `null` in a `T | null` union is never coerced away.
    for schema in schemas {
        if sub_schema_validator(schema).is_some_and(|v| v.is_valid(&value)) {
            return value;
        }
    }
    for schema in schemas {
        let coerced = coerce_with_json_schema(value.clone(), schema);
        if sub_schema_validator(schema).is_some_and(|v| v.is_valid(&coerced)) {
            return coerced;
        }
    }
    value
}

/// Coerce `value` towards `schema`, recursing through objects, arrays and
/// `allOf` / `anyOf` / `oneOf`.
pub fn coerce_with_json_schema(value: Value, schema: &Value) -> Value {
    let mut next = value;

    if let Some(all_of) = schema.get("allOf").and_then(|v| v.as_array()) {
        for nested in all_of {
            next = coerce_with_json_schema(next, nested);
        }
    }
    if let Some(any_of) = schema.get("anyOf").and_then(|v| v.as_array()) {
        next = coerce_with_union_schema(next, any_of);
    }
    if let Some(one_of) = schema.get("oneOf").and_then(|v| v.as_array()) {
        next = coerce_with_union_schema(next, one_of);
    }

    let types = schema_types(schema);
    let matches_union_member = types.len() > 1 && types.iter().any(|t| matches_json_type(&next, t));
    if !types.is_empty() && !matches_union_member {
        for ty in &types {
            let candidate = coerce_primitive_by_type(&next, ty);
            if candidate != next {
                next = candidate;
                break;
            }
        }
    }

    if types.iter().any(|t| t == "object") {
        if let Value::Object(_) = next {
            next = apply_object_coercion(next, schema);
        }
    }
    if types.iter().any(|t| t == "array") {
        if let Value::Array(_) = next {
            next = apply_array_coercion(next, schema);
        }
    }

    next
}

fn apply_object_coercion(value: Value, schema: &Value) -> Value {
    let Value::Object(mut object) = value else {
        return value;
    };
    let properties = schema.get("properties").and_then(|p| p.as_object());

    if let Some(properties) = properties {
        for (key, property_schema) in properties {
            if let Some(current) = object.remove(key) {
                object.insert(
                    key.clone(),
                    coerce_with_json_schema(current, property_schema),
                );
            }
        }
    }

    if let Some(additional) = schema.get("additionalProperties").filter(|v| v.is_object()) {
        let defined: Vec<String> = properties
            .map(|p| p.keys().cloned().collect())
            .unwrap_or_default();
        let keys: Vec<String> = object.keys().cloned().collect();
        for key in keys {
            if defined.contains(&key) {
                continue;
            }
            if let Some(current) = object.remove(&key) {
                object.insert(key, coerce_with_json_schema(current, additional));
            }
        }
    }

    Value::Object(object)
}

fn apply_array_coercion(value: Value, schema: &Value) -> Value {
    let Value::Array(mut items) = value else {
        return value;
    };
    match schema.get("items") {
        Some(Value::Array(item_schemas)) => {
            for (index, item) in items.iter_mut().enumerate() {
                if let Some(item_schema) = item_schemas.get(index) {
                    *item = coerce_with_json_schema(item.clone(), item_schema);
                }
            }
        }
        Some(item_schema) if item_schema.is_object() => {
            for item in items.iter_mut() {
                *item = coerce_with_json_schema(item.clone(), item_schema);
            }
        }
        _ => {}
    }
    Value::Array(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn echo_tool(parameters: Value) -> Tool {
        Tool::new("echo", "Echo tool", parameters)
    }

    fn call(arguments: Value) -> ToolCall {
        ToolCall {
            id: "tool-1".into(),
            name: "echo".into(),
            arguments: arguments.as_object().cloned().unwrap_or_default(),
            thought_signature: None,
            namespace: None,
        }
    }

    /// `createToolCallWithPlainSchema` from the upstream suite: wrap the schema
    /// under a single required `value` property.
    fn plain_schema_case(schema: Value, value: Value) -> (Tool, ToolCall) {
        let tool = echo_tool(json!({
            "type": "object",
            "properties": { "value": schema },
            "required": ["value"],
        }));
        (tool, call(json!({ "value": value })))
    }

    #[test]
    fn coerces_plain_json_schemas_with_ajv_compatible_primitive_rules() {
        let cases: Vec<(Value, Value, Value)> = vec![
            (json!({"type": "number"}), json!("42"), json!(42)),
            (json!({"type": "number"}), json!(true), json!(1)),
            (json!({"type": "number"}), json!(null), json!(0)),
            (json!({"type": "integer"}), json!("42"), json!(42)),
            (json!({"type": "boolean"}), json!("true"), json!(true)),
            (json!({"type": "boolean"}), json!("false"), json!(false)),
            (json!({"type": "boolean"}), json!(1), json!(true)),
            (json!({"type": "boolean"}), json!(0), json!(false)),
            (json!({"type": "string"}), json!(null), json!("")),
            (json!({"type": "string"}), json!(true), json!("true")),
            (json!({"type": "null"}), json!(""), json!(null)),
            (json!({"type": "null"}), json!(0), json!(null)),
            (json!({"type": "null"}), json!(false), json!(null)),
            // A value already matching one arm of a union type is left alone.
            (
                json!({"type": ["number", "string"]}),
                json!("1"),
                json!("1"),
            ),
            // No arm matches, so the first rule that changes anything wins.
            (json!({"type": ["boolean", "number"]}), json!("1"), json!(1)),
        ];

        for (schema, input, expected) in cases {
            let (tool, tool_call) = plain_schema_case(schema.clone(), input.clone());
            let result = validate_tool_arguments(&tool, &tool_call)
                .unwrap_or_else(|e| panic!("schema {schema} input {input}: {e}"));
            assert_eq!(
                Value::Object(result),
                json!({ "value": expected }),
                "schema {schema}"
            );
        }
    }

    #[test]
    fn rejects_invalid_coercions_for_plain_json_schemas() {
        let cases: Vec<(Value, Value)> = vec![
            (json!({"type": "boolean"}), json!("1")),
            (json!({"type": "boolean"}), json!("0")),
            (json!({"type": "null"}), json!("null")),
            (json!({"type": "integer"}), json!("42.1")),
        ];
        for (schema, input) in cases {
            let (tool, tool_call) = plain_schema_case(schema.clone(), input.clone());
            let err = validate_tool_arguments(&tool, &tool_call).expect_err("should reject");
            assert!(
                err.to_string().contains("Validation failed"),
                "schema {schema} input {input}: {err}"
            );
            assert_eq!(err.code(), "invalid_tool_arguments");
        }
    }

    #[test]
    fn treats_null_as_omission_for_optional_non_nullable_properties() {
        let tool = echo_tool(json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "offset": { "type": "number" },
                "nullable": { "anyOf": [{ "type": "string" }, { "type": "null" }] },
                "metadata": {
                    "type": "object",
                    "properties": { "enabled": { "type": "boolean" } },
                },
            },
            "required": ["path", "metadata"],
        }));
        let tool_call = call(json!({
            "path": "file.txt",
            "offset": null,
            "nullable": null,
            "metadata": { "enabled": null },
        }));

        let result = validate_tool_arguments(&tool, &tool_call).unwrap();
        assert_eq!(
            Value::Object(result),
            json!({ "path": "file.txt", "nullable": null, "metadata": {} })
        );
    }

    #[test]
    fn preserves_optional_nulls_whose_referenced_schema_is_nullable() {
        let tool = echo_tool(json!({
            "type": "object",
            "properties": { "value": { "$ref": "#/$defs/value" } },
            "$defs": { "value": { "anyOf": [{ "type": "number" }, { "type": "null" }] } },
        }));
        let result = validate_tool_arguments(&tool, &call(json!({ "value": null }))).unwrap();
        assert_eq!(Value::Object(result), json!({ "value": null }));
    }

    #[test]
    fn preserves_a_value_that_already_matches_a_nullable_union_arm() {
        for keyword in ["anyOf", "oneOf"] {
            let (tool, tool_call) = plain_schema_case(
                json!({ keyword: [{ "type": "number" }, { "type": "null" }] }),
                json!(null),
            );
            let result = validate_tool_arguments(&tool, &tool_call).unwrap();
            assert_eq!(Value::Object(result), json!({ "value": null }), "{keyword}");
        }
    }

    #[test]
    fn still_coerces_nullable_unions_when_no_arm_matches() {
        let (tool, tool_call) = plain_schema_case(
            json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] }),
            json!("42"),
        );
        let result = validate_tool_arguments(&tool, &tool_call).unwrap();
        assert_eq!(Value::Object(result), json!({ "value": 42 }));
    }

    #[test]
    fn accepts_null_for_nullable_array_schemas_with_items() {
        let (tool, tool_call) = plain_schema_case(
            json!({ "type": ["array", "null"], "items": { "type": "string" } }),
            json!(null),
        );
        let result = validate_tool_arguments(&tool, &tool_call).unwrap();
        assert_eq!(Value::Object(result), json!({ "value": null }));
    }

    #[test]
    fn coerces_inside_arrays_and_nested_objects() {
        let tool = echo_tool(json!({
            "type": "object",
            "properties": {
                "edits": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": { "line": { "type": "integer" }, "keep": { "type": "boolean" } },
                        "required": ["line", "keep"],
                    },
                },
            },
            "required": ["edits"],
        }));
        let tool_call = call(json!({ "edits": [{ "line": "3", "keep": "true" }] }));
        let result = validate_tool_arguments(&tool, &tool_call).unwrap();
        assert_eq!(
            Value::Object(result),
            json!({ "edits": [{ "line": 3, "keep": true }] })
        );
    }

    #[test]
    fn reports_the_missing_property_path_and_the_received_arguments() {
        let tool = echo_tool(json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
        }));
        let err = validate_tool_arguments(&tool, &call(json!({}))).expect_err("missing required");
        let message = err.to_string();
        assert!(
            message.starts_with("Validation failed for tool \"echo\":"),
            "{message}"
        );
        assert!(message.contains("- path:"), "{message}");
        assert!(message.contains("Received arguments:"), "{message}");
    }

    #[test]
    fn reports_a_nested_path_with_dots() {
        let tool = echo_tool(json!({
            "type": "object",
            "properties": {
                "opts": {
                    "type": "object",
                    "properties": { "depth": { "type": "integer" } },
                },
            },
        }));
        let err = validate_tool_arguments(&tool, &call(json!({ "opts": { "depth": "deep" } })))
            .unwrap_err();
        assert!(err.to_string().contains("- opts.depth:"), "{err}");
    }

    #[test]
    fn missing_tool_is_reported_by_name() {
        let err = validate_tool_call(&[], &call(json!({}))).unwrap_err();
        assert_eq!(err.to_string(), "Tool \"echo\" not found");
        assert_eq!(err.code(), "tool_not_found");
    }

    #[test]
    fn an_unusable_schema_is_reported_separately_from_bad_arguments() {
        let tool = echo_tool(json!({ "type": "not-a-json-type" }));
        let err = validate_tool_arguments(&tool, &call(json!({}))).unwrap_err();
        assert_eq!(err.code(), "invalid_tool_schema");
    }

    #[test]
    fn accepts_arguments_that_already_validate_without_touching_them() {
        let tool = echo_tool(json!({
            "type": "object",
            "properties": { "a": { "type": "string" }, "b": { "type": "integer" } },
            "required": ["a"],
            "additionalProperties": false,
        }));
        let args = json!({ "a": "x", "b": 7 });
        let result = validate_tool_arguments(&tool, &call(args.clone())).unwrap();
        assert_eq!(Value::Object(result), args);
    }
}
