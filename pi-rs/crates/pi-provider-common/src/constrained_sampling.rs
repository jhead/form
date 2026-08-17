//! Port of `api/constrained-sampling.ts`.
//!
//! Two mechanisms live here:
//! - **strict JSON Schema**: rewrite a tool's parameter schema into the strict
//!   subset OpenAI accepts (`additionalProperties:false`, every property
//!   required, optional properties widened with `{"anyOf":[T,{"type":"null"}]}`).
//! - **grammar tools**: OpenAI `type:"custom"` tools whose input is constrained
//!   by a Lark grammar or a regex. Their single string argument arrives as a raw
//!   token stream, so it has to be re-encoded as JSON deltas on the way out.
//!
//! # Consolidated from four copies
//!
//! Anthropic, Google and misc had each ported only the JSON-schema half, since
//! grammar tools are an OpenAI feature. The three partial copies were
//! behaviourally identical to the OpenAI one — same key list, same error
//! strings, same widening rule — differing only in how they spelled the error:
//! Anthropic and Google surfaced [`pi_core::AiError`], misc surfaced a bare
//! `String`. `AiError` wins, because that is what every call site ultimately
//! needs and `String` forced the Mistral adapter to re-wrap at the boundary.
//!
//! Adapters that only speak JSON schema simply never call the grammar half.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use pi_core::tool::{ConstrainedSampling, ConstrainedSamplingConfig, GrammarFormat, StrictMode};
use pi_core::{AiError, Tool};

/// A schema shape the strict subset cannot express. Recoverable: unless the tool
/// says `strict: "require"`, the caller falls back to a non-strict tool.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct UnsupportedStrictJsonSchema(pub String);

const UNSUPPORTED_STRICT_SCHEMA_KEYS: [&str; 16] = [
    "$ref",
    "$defs",
    "definitions",
    "allOf",
    "oneOf",
    "patternProperties",
    "dependentSchemas",
    "dependencies",
    "unevaluatedProperties",
    "propertyNames",
    "contains",
    "prefixItems",
    "not",
    "if",
    "then",
    "else",
];

fn is_schema_object(value: &Value) -> bool {
    value.is_object()
}

fn is_structured_schema(schema: &Value) -> bool {
    let Some(obj) = schema.as_object() else {
        return false;
    };
    let types: Vec<&str> = match obj.get("type") {
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(items)) => items.iter().filter_map(|v| v.as_str()).collect(),
        _ => Vec::new(),
    };
    types.contains(&"object")
        || types.contains(&"array")
        || obj.contains_key("properties")
        || obj.contains_key("items")
}

fn schema_allows_null(schema: &Value) -> bool {
    let Some(obj) = schema.as_object() else {
        return false;
    };
    match obj.get("type") {
        Some(Value::String(s)) if s == "null" => return true,
        Some(Value::Array(items)) if items.iter().any(|v| v.as_str() == Some("null")) => {
            return true
        }
        _ => {}
    }
    if obj.get("const") == Some(&Value::Null) {
        return true;
    }
    if let Some(Value::Array(values)) = obj.get("enum") {
        if values.contains(&Value::Null) {
            return true;
        }
    }
    matches!(obj.get("anyOf"), Some(Value::Array(variants)) if variants.iter().any(schema_allows_null))
}

fn make_node_strict(schema: &mut Value) -> Result<(), UnsupportedStrictJsonSchema> {
    if !is_schema_object(schema) {
        return Err(UnsupportedStrictJsonSchema(
            "boolean schemas are unsupported".into(),
        ));
    }
    for key in UNSUPPORTED_STRICT_SCHEMA_KEYS {
        if schema.get(key).is_some() {
            return Err(UnsupportedStrictJsonSchema(format!(
                "{key} schemas are unsupported"
            )));
        }
    }

    if let Some(any_of) = schema.get_mut("anyOf") {
        let Some(variants) = any_of.as_array_mut() else {
            return Err(UnsupportedStrictJsonSchema(
                "anyOf must contain at least one schema".into(),
            ));
        };
        if variants.is_empty() {
            return Err(UnsupportedStrictJsonSchema(
                "anyOf must contain at least one schema".into(),
            ));
        }
        for variant in variants.iter_mut() {
            if is_structured_schema(variant) {
                return Err(UnsupportedStrictJsonSchema(
                    "object and array unions are unsupported".into(),
                ));
            }
            make_node_strict(variant)?;
        }
    }

    if let Some(items) = schema.get_mut("items") {
        if items.is_array() {
            return Err(UnsupportedStrictJsonSchema(
                "tuple schemas are unsupported".into(),
            ));
        }
        make_node_strict(items)?;
    }

    let is_object_schema = schema.get("type").and_then(Value::as_str) == Some("object");
    if schema.get("properties").is_some() && !is_object_schema {
        return Err(UnsupportedStrictJsonSchema(
            "properties require type object".into(),
        ));
    }
    if !is_object_schema {
        return Ok(());
    }
    match schema.get("additionalProperties") {
        None | Some(Value::Bool(false)) => {}
        Some(_) => {
            return Err(UnsupportedStrictJsonSchema(
                "schema-valued or true additionalProperties is unsupported".into(),
            ))
        }
    }
    if let Some(properties) = schema.get("properties") {
        if !properties.is_object() {
            return Err(UnsupportedStrictJsonSchema(
                "object properties must be a schema map".into(),
            ));
        }
    }
    let required: Vec<String> = match schema.get("required") {
        None => Vec::new(),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(s) => out.push(s.to_string()),
                    None => {
                        return Err(UnsupportedStrictJsonSchema(
                            "object required must be a string array".into(),
                        ))
                    }
                }
            }
            out
        }
        Some(_) => {
            return Err(UnsupportedStrictJsonSchema(
                "object required must be a string array".into(),
            ))
        }
    };

    let property_names: Vec<String> = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    if required.iter().any(|key| !property_names.contains(key)) {
        return Err(UnsupportedStrictJsonSchema(
            "required contains an unknown property".into(),
        ));
    }

    if let Some(Value::Object(properties)) = schema.get_mut("properties") {
        for key in property_names.clone() {
            let property = properties.get_mut(&key).expect("key came from this map");
            make_node_strict(property)?;
            if !required.contains(&key) && !schema_allows_null(property) {
                let widened = serde_json::json!({
                    "anyOf": [property.clone(), { "type": "null" }]
                });
                *property = widened;
            }
        }
    }

    let obj = schema.as_object_mut().expect("checked above");
    obj.insert(
        "required".to_string(),
        Value::Array(property_names.into_iter().map(Value::String).collect()),
    );
    obj.insert("additionalProperties".to_string(), Value::Bool(false));
    Ok(())
}

/// Port of `makeStrictJsonSchema`.
pub fn make_strict_json_schema(schema: &Value) -> Result<Value, UnsupportedStrictJsonSchema> {
    let mut cloned = schema.clone();
    if !is_schema_object(&cloned) {
        return Err(UnsupportedStrictJsonSchema(
            "root schema must have type object".into(),
        ));
    }
    make_node_strict(&mut cloned)?;
    if cloned.get("type").and_then(Value::as_str) != Some("object") {
        return Err(UnsupportedStrictJsonSchema(
            "root schema must have type object".into(),
        ));
    }
    Ok(cloned)
}

/// Port of `getJsonSchemaToolParameters`.
pub fn json_schema_tool_parameters(tool: &Tool, strict: Option<bool>) -> Value {
    if strict == Some(true) {
        make_strict_json_schema(&tool.parameters).unwrap_or_else(|_| tool.parameters.clone())
    } else {
        tool.parameters.clone()
    }
}

/// Port of `resolveJsonSchemaStrictSampling`.
///
/// `Ok(None)` means "no opinion" (leave the provider default), `Ok(Some(true))`
/// means "send strict". `Err` is only produced when the tool demands strict
/// sampling that the provider or the schema cannot support.
pub fn resolve_json_schema_strict_sampling(
    tool: &Tool,
    supports_strict_mode: bool,
) -> Result<Option<bool>, AiError> {
    let Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema { strict })) =
        &tool.constrained_sampling
    else {
        return Ok(None);
    };

    if supports_strict_mode {
        return match make_strict_json_schema(&tool.parameters) {
            Ok(_) => Ok(Some(true)),
            Err(err) => {
                if *strict != StrictMode::Require {
                    Ok(None)
                } else {
                    Err(AiError::invalid_request(format!(
                        "Tool \"{}\" requires JSON-schema constrained sampling, but {}.",
                        tool.name, err.0
                    )))
                }
            }
        };
    }
    if *strict == StrictMode::Require {
        return Err(AiError::invalid_request(format!(
            "Tool \"{}\" requires JSON-schema constrained sampling, but strict tools are unsupported.",
            tool.name
        )));
    }
    Ok(None)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrammarConstrainedSampling {
    pub format: GrammarFormat,
    pub definition: String,
    pub input_property: String,
}

impl GrammarConstrainedSampling {
    /// The wire spelling OpenAI expects for `format.syntax`.
    pub fn syntax(&self) -> &'static str {
        match self.format {
            GrammarFormat::OpenaiLark => "lark",
            GrammarFormat::OpenaiRegex => "regex",
        }
    }
}

/// Incremental JSON re-encoder for a grammar tool's single string argument.
#[derive(Debug, Clone, Default)]
pub struct GrammarToolInputJsonBuffer {
    pub input: String,
    pub started: bool,
    pub closed: bool,
}

/// Port of `getGrammarToolInput`.
pub fn grammar_tool_input(
    tool_name: &str,
    arguments: &Map<String, Value>,
    input_property: &str,
) -> Result<String, AiError> {
    match arguments.get(input_property).and_then(Value::as_str) {
        Some(input) => Ok(input.to_string()),
        None => Err(AiError::invalid_request(format!(
            "Grammar tool call \"{tool_name}\" requires argument \"{input_property}\" to be a string."
        ))),
    }
}

/// Port of `appendGrammarToolInputJsonDelta`.
///
/// The provider streams the raw constrained text; the pi protocol expects
/// `toolcall_delta` payloads that concatenate into valid JSON. This synthesizes
/// `{"prop":"` … escaped chunks … `"}`.
pub fn append_grammar_tool_input_json_delta(
    buffer: &mut GrammarToolInputJsonBuffer,
    input_property: &str,
    next_input: &str,
    close: bool,
) -> Result<Option<String>, AiError> {
    if buffer.closed {
        if close && next_input == buffer.input {
            return Ok(None);
        }
        return Err(AiError::protocol(format!(
            "grammar tool input for property \"{input_property}\" changed after it was closed"
        )));
    }
    if !next_input.starts_with(&buffer.input) {
        return Err(AiError::protocol(format!(
            "grammar tool input for property \"{input_property}\" changed non-monotonically"
        )));
    }

    let input_delta = &next_input[buffer.input.len()..];
    if !close && input_delta.is_empty() {
        return Ok(None);
    }

    let mut delta = String::new();
    if !buffer.started {
        delta.push('{');
        delta.push_str(&Value::String(input_property.to_string()).to_string());
        delta.push_str(":\"");
        buffer.started = true;
    }
    let encoded = Value::String(input_delta.to_string()).to_string();
    delta.push_str(&encoded[1..encoded.len() - 1]);
    buffer.input = next_input.to_string();

    if close {
        delta.push_str("\"}");
        buffer.closed = true;
    }
    Ok(Some(delta))
}

fn infer_grammar_input_property(tool: &Tool) -> Result<String, AiError> {
    let schema = &tool.parameters;
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err(AiError::invalid_request(
            "grammar constrained sampling requires an object parameter schema",
        ));
    }
    let required = schema.get("required").and_then(Value::as_array);
    let input_property =
        match required {
            Some(items) if items.len() == 1 => match items[0].as_str() {
                Some(name) => name.to_string(),
                None => return Err(AiError::invalid_request(
                    "grammar constrained sampling requires exactly one required string property",
                )),
            },
            _ => {
                return Err(AiError::invalid_request(
                    "grammar constrained sampling requires exactly one required string property",
                ))
            }
        };
    let property = schema
        .get("properties")
        .and_then(Value::as_object)
        .and_then(|m| m.get(&input_property));
    let Some(property) = property else {
        return Err(AiError::invalid_request(format!(
            "grammar constrained sampling requires a properties entry for {input_property}"
        )));
    };
    if property.get("type").and_then(Value::as_str) != Some("string") {
        return Err(AiError::invalid_request(format!(
            "grammar constrained sampling property {input_property} must have type string"
        )));
    }
    Ok(input_property)
}

/// Port of `resolveGrammarConstrainedSampling`.
pub fn resolve_grammar_constrained_sampling(
    tool: &Tool,
    supports_openai_grammar_tools: bool,
) -> Result<Option<GrammarConstrainedSampling>, AiError> {
    let Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::Grammar { variants })) =
        &tool.constrained_sampling
    else {
        return Ok(None);
    };
    if !supports_openai_grammar_tools {
        return Ok(None);
    }

    let lark = variants
        .openai_lark
        .as_deref()
        .filter(|d| !d.trim().is_empty());
    let regex = variants
        .openai_regex
        .as_deref()
        .filter(|d| !d.trim().is_empty());
    let (format, definition) = match (lark, regex) {
        (Some(lark), _) => (GrammarFormat::OpenaiLark, lark.to_string()),
        (None, Some(regex)) => (GrammarFormat::OpenaiRegex, regex.to_string()),
        (None, None) => {
            return Err(AiError::invalid_request(format!(
                "Tool \"{}\" cannot use grammar constrained sampling: no supported grammar variant was provided.",
                tool.name
            )))
        }
    };

    match infer_grammar_input_property(tool) {
        Ok(input_property) => Ok(Some(GrammarConstrainedSampling {
            format,
            definition,
            input_property,
        })),
        Err(err) => Err(AiError::invalid_request(format!(
            "Tool \"{}\" cannot use grammar constrained sampling: {}.",
            tool.name,
            err.message()
        ))),
    }
}

/// Tool name → the schema property that carries the grammar-constrained text.
pub type GrammarToolInputProperties = BTreeMap<String, String>;

/// Port of `createGrammarToolInputProperties`.
pub fn create_grammar_tool_input_properties(
    tools: Option<&[Tool]>,
    supports_openai_grammar_tools: bool,
) -> Result<GrammarToolInputProperties, AiError> {
    let mut properties = GrammarToolInputProperties::new();
    for tool in tools.unwrap_or(&[]) {
        if let Some(grammar) =
            resolve_grammar_constrained_sampling(tool, supports_openai_grammar_tools)?
        {
            properties.insert(tool.name.clone(), grammar.input_property);
        }
    }
    Ok(properties)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::tool::GrammarVariants;
    use serde_json::json;

    fn tool_with(params: Value) -> Tool {
        Tool::new("t", "d", params)
    }

    #[test]
    fn strict_schema_requires_all_properties_and_widens_optionals() {
        let schema = json!({
            "type": "object",
            "properties": { "a": { "type": "string" }, "b": { "type": "number" } },
            "required": ["a"]
        });
        let strict = make_strict_json_schema(&schema).unwrap();
        assert_eq!(strict["required"], json!(["a", "b"]));
        assert_eq!(strict["additionalProperties"], json!(false));
        assert_eq!(strict["properties"]["a"], json!({"type": "string"}));
        assert_eq!(
            strict["properties"]["b"],
            json!({"anyOf": [{"type": "number"}, {"type": "null"}]})
        );
    }

    #[test]
    fn strict_schema_rejects_refs() {
        let schema = json!({ "type": "object", "properties": {}, "$defs": {} });
        assert!(make_strict_json_schema(&schema).is_err());
    }

    #[test]
    fn require_strict_on_unsupported_schema_is_an_error() {
        let mut tool = tool_with(json!({ "type": "object", "allOf": [] }));
        tool.constrained_sampling = Some(ConstrainedSampling::Config(
            ConstrainedSamplingConfig::JsonSchema {
                strict: StrictMode::Require,
            },
        ));
        assert!(resolve_json_schema_strict_sampling(&tool, true).is_err());
    }

    #[test]
    fn prefer_strict_on_unsupported_schema_falls_back() {
        let mut tool = tool_with(json!({ "type": "object", "allOf": [] }));
        tool.constrained_sampling = Some(ConstrainedSampling::Config(
            ConstrainedSamplingConfig::JsonSchema {
                strict: StrictMode::Prefer,
            },
        ));
        assert_eq!(
            resolve_json_schema_strict_sampling(&tool, true).unwrap(),
            None
        );
    }

    #[test]
    fn grammar_tool_infers_the_single_required_string_property() {
        let mut tool = tool_with(json!({
            "type": "object",
            "properties": { "expr": { "type": "string" } },
            "required": ["expr"]
        }));
        tool.constrained_sampling = Some(ConstrainedSampling::Config(
            ConstrainedSamplingConfig::Grammar {
                variants: GrammarVariants {
                    openai_lark: Some("start: NUMBER".into()),
                    openai_regex: None,
                },
            },
        ));
        let grammar = resolve_grammar_constrained_sampling(&tool, true)
            .unwrap()
            .unwrap();
        assert_eq!(grammar.input_property, "expr");
        assert_eq!(grammar.syntax(), "lark");
    }

    #[test]
    fn grammar_delta_encodes_json_fragments() {
        let mut buffer = GrammarToolInputJsonBuffer::default();
        let first = append_grammar_tool_input_json_delta(&mut buffer, "expr", "1 +", false)
            .unwrap()
            .unwrap();
        assert_eq!(first, r#"{"expr":"1 +"#);
        let second = append_grammar_tool_input_json_delta(&mut buffer, "expr", "1 + 2\"", true)
            .unwrap()
            .unwrap();
        assert_eq!(second, r#" 2\""}"#);
        let joined = format!("{first}{second}");
        let parsed: Value = serde_json::from_str(&joined).unwrap();
        assert_eq!(parsed, json!({"expr": "1 + 2\""}));
    }

    #[test]
    fn grammar_delta_rejects_non_monotonic_input() {
        let mut buffer = GrammarToolInputJsonBuffer::default();
        append_grammar_tool_input_json_delta(&mut buffer, "expr", "abc", false).unwrap();
        assert!(append_grammar_tool_input_json_delta(&mut buffer, "expr", "xyz", false).is_err());
    }

    // --- cases carried over from the anthropic, google and misc copies ---

    /// From the anthropic copy: sibling keys the strict subset does not touch
    /// (`title`, here) must survive the rewrite untouched.
    #[test]
    fn widening_preserves_unrelated_schema_keys() {
        let schema = json!({
            "type": "object",
            "properties": { "value": { "type": "string" }, "optional": { "type": "number" } },
            "required": ["value"],
            "title": "StrictLookupInput"
        });
        let strict = make_strict_json_schema(&schema).unwrap();
        assert_eq!(strict["required"], json!(["value", "optional"]));
        assert_eq!(strict["title"], json!("StrictLookupInput"));
    }

    /// From the misc copy: the rewrite is recursive, not just top level.
    #[test]
    fn recurses_into_nested_objects() {
        let schema = json!({
            "type": "object",
            "properties": {
                "nested": { "type": "object", "properties": { "value": {"type": "string"} } }
            },
            "required": ["nested"]
        });
        let strict = make_strict_json_schema(&schema).unwrap();
        assert_eq!(strict["properties"]["nested"]["required"], json!(["value"]));
        assert_eq!(
            strict["properties"]["nested"]["additionalProperties"],
            json!(false)
        );
    }

    /// From the misc copy: the message text is part of the contract, because
    /// `resolveJsonSchemaStrictSampling` interpolates it into the user-facing
    /// error when a tool says `strict: "require"`.
    #[test]
    fn rejects_unsupported_keywords_by_name() {
        let schema = json!({ "type": "object", "properties": {}, "allOf": [] });
        assert_eq!(
            make_strict_json_schema(&schema).unwrap_err().0,
            "allOf schemas are unsupported"
        );
        let refs = json!({ "type": "object", "properties": {"a": {"$ref": "#/$defs/x"}} });
        assert_eq!(
            make_strict_json_schema(&refs).unwrap_err().0,
            "$ref schemas are unsupported"
        );
    }

    /// A property that already admits null is left alone rather than
    /// double-wrapped.
    #[test]
    fn nullable_optional_properties_are_not_widened_twice() {
        for already_nullable in [
            json!({ "type": ["string", "null"] }),
            json!({ "anyOf": [{"type": "string"}, {"type": "null"}] }),
            json!({ "enum": ["a", null] }),
            json!({ "const": null }),
        ] {
            let schema = json!({
                "type": "object",
                "properties": { "opt": already_nullable.clone() },
                "required": []
            });
            let strict = make_strict_json_schema(&schema).unwrap();
            assert_eq!(
                strict["properties"]["opt"], already_nullable,
                "{already_nullable} should not be re-wrapped"
            );
        }
    }

    /// From the google copy: `require` on a model without strict support is a
    /// different error from `require` on a schema the subset cannot express.
    #[test]
    fn require_strict_on_an_unsupporting_model_is_an_error() {
        let mut tool = tool_with(json!({ "type": "object", "properties": {} }));
        tool.constrained_sampling = Some(ConstrainedSampling::Config(
            ConstrainedSamplingConfig::JsonSchema {
                strict: StrictMode::Require,
            },
        ));
        let err = resolve_json_schema_strict_sampling(&tool, false).unwrap_err();
        assert!(err.message().contains("strict tools are unsupported"));

        // The same tool against a model that does support it resolves cleanly.
        assert_eq!(
            resolve_json_schema_strict_sampling(&tool, true).unwrap(),
            Some(true)
        );
    }

    #[test]
    fn prefer_strict_on_an_unsupporting_model_degrades_silently() {
        let mut tool = tool_with(json!({ "type": "object", "properties": {} }));
        tool.constrained_sampling = Some(ConstrainedSampling::Config(
            ConstrainedSamplingConfig::JsonSchema {
                strict: StrictMode::Prefer,
            },
        ));
        assert_eq!(
            resolve_json_schema_strict_sampling(&tool, false).unwrap(),
            None
        );
    }

    /// From the google copy.
    #[test]
    fn no_constrained_sampling_is_none() {
        let tool = tool_with(json!({ "type": "object" }));
        assert_eq!(
            resolve_json_schema_strict_sampling(&tool, true).unwrap(),
            None
        );
    }

    #[test]
    fn json_schema_tool_parameters_passes_through_when_not_strict() {
        let params = json!({ "type": "object", "properties": { "a": {"type": "string"} } });
        let tool = tool_with(params.clone());
        assert_eq!(json_schema_tool_parameters(&tool, None), params);
        assert_eq!(json_schema_tool_parameters(&tool, Some(false)), params);
        assert_eq!(
            json_schema_tool_parameters(&tool, Some(true))["additionalProperties"],
            json!(false)
        );
    }

    #[test]
    fn a_root_that_is_not_an_object_schema_is_rejected() {
        assert!(make_strict_json_schema(&json!({ "type": "string" })).is_err());
        assert!(make_strict_json_schema(&json!(true)).is_err());
    }

    #[test]
    fn grammar_tools_are_ignored_when_the_model_cannot_serve_them() {
        let mut tool = tool_with(json!({
            "type": "object",
            "properties": { "expr": { "type": "string" } },
            "required": ["expr"]
        }));
        tool.constrained_sampling = Some(ConstrainedSampling::Config(
            ConstrainedSamplingConfig::Grammar {
                variants: GrammarVariants {
                    openai_lark: Some("start: NUMBER".into()),
                    openai_regex: None,
                },
            },
        ));
        assert_eq!(
            resolve_grammar_constrained_sampling(&tool, false).unwrap(),
            None
        );
        assert!(
            create_grammar_tool_input_properties(Some(&[tool.clone()]), false)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            create_grammar_tool_input_properties(Some(&[tool]), true)
                .unwrap()
                .get("t")
                .map(String::as_str),
            Some("expr")
        );
    }

    #[test]
    fn grammar_falls_back_to_the_regex_variant() {
        let mut tool = tool_with(json!({
            "type": "object",
            "properties": { "expr": { "type": "string" } },
            "required": ["expr"]
        }));
        tool.constrained_sampling = Some(ConstrainedSampling::Config(
            ConstrainedSamplingConfig::Grammar {
                variants: GrammarVariants {
                    openai_lark: Some("   ".into()),
                    openai_regex: Some("[0-9]+".into()),
                },
            },
        ));
        let grammar = resolve_grammar_constrained_sampling(&tool, true)
            .unwrap()
            .unwrap();
        assert_eq!(grammar.syntax(), "regex");
        assert_eq!(grammar.definition, "[0-9]+");
    }

    #[test]
    fn grammar_tool_input_requires_a_string_argument() {
        let mut arguments = Map::new();
        arguments.insert("expr".into(), json!(1));
        assert!(grammar_tool_input("t", &arguments, "expr").is_err());
        arguments.insert("expr".into(), json!("1 + 2"));
        assert_eq!(
            grammar_tool_input("t", &arguments, "expr").unwrap(),
            "1 + 2"
        );
    }
}
