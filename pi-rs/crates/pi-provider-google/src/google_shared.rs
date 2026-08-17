//! Port of `api/google-shared.ts` — everything the Gemini and Vertex adapters
//! have in common: message/tool conversion, thought-signature rules, finish
//! reason mapping and cost accounting.

use serde_json::{Map, Value};

use pi_core::content::{AssistantContent, InputContent};
use pi_core::message::{Message, StopReason};
use pi_core::model::{Model, ModelThinkingLevel};
use pi_core::tool::{Context, Tool};
use pi_core::AiError;

use crate::wire::{Blob, Content, FunctionCall, FunctionCallingConfigMode, FunctionResponse, Part};
use pi_provider_common::constrained_sampling::{
    json_schema_tool_parameters, resolve_json_schema_strict_sampling,
};
use pi_provider_common::sanitize_unicode::sanitize_surrogates;
use pi_provider_common::transform_messages::transform_messages;

/// Whether a streamed part is thinking content.
///
/// `thought: true` is the only marker. `thoughtSignature` rides on any part
/// type for context replay and must not be read as a thinking marker.
/// See <https://ai.google.dev/gemini-api/docs/thought-signatures>.
pub fn is_thinking_part(part: &Part) -> bool {
    part.thought == Some(true)
}

/// Keep the last non-empty signature seen for the current streamed block.
///
/// Some backends only send `thoughtSignature` on the first delta of a block.
/// This never moves a signature across distinct response parts.
pub fn retain_thought_signature(
    existing: Option<String>,
    incoming: Option<&str>,
) -> Option<String> {
    match incoming {
        Some(sig) if !sig.is_empty() => Some(sig.to_string()),
        _ => existing,
    }
}

/// Thought signatures are `TYPE_BYTES` on the wire, so they must be base64.
fn is_valid_thought_signature(signature: Option<&str>) -> bool {
    let Some(signature) = signature else {
        return false;
    };
    if signature.is_empty() || signature.len() % 4 != 0 {
        return false;
    }
    let body = signature.trim_end_matches('=');
    if body.is_empty() || signature.len() - body.len() > 2 {
        return false;
    }
    body.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/')
}

/// Only signatures minted by this exact provider+model can be replayed.
fn resolve_thought_signature(
    is_same_provider_and_model: bool,
    signature: Option<&str>,
) -> Option<String> {
    if is_same_provider_and_model && is_valid_thought_signature(signature) {
        signature.map(str::to_string)
    } else {
        None
    }
}

fn gemini_major_version(model_id: &str) -> Option<u32> {
    let id = model_id.to_lowercase();
    let rest = id
        .strip_prefix("gemini-live-")
        .or_else(|| id.strip_prefix("gemini-"))?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Models behind the Google APIs that need explicit ids on function calls and
/// responses (`/^gemini(?:-live)?-(\d+)/` >= 3, Claude and gpt-oss passthrough).
pub fn requires_tool_call_id(model_id: &str) -> bool {
    model_id.starts_with("claude-")
        || model_id.starts_with("gpt-oss-")
        || gemini_major_version(model_id).is_some_and(|v| v >= 3)
}

fn supports_multimodal_function_response(model_id: &str) -> bool {
    match gemini_major_version(model_id) {
        Some(version) => version >= 3,
        None => true,
    }
}

/// `/gemini-3(?:\.\d+)?-{suffix}/` without pulling in a regex engine.
fn matches_gemini3_variant(model_id: &str, suffix: &str) -> bool {
    let lower = model_id.to_lowercase();
    for (index, _) in lower.match_indices("gemini-3") {
        let rest = &lower[index + "gemini-3".len()..];
        if rest.starts_with(suffix) {
            return true;
        }
        if let Some(after_dot) = rest.strip_prefix('.') {
            let digits = after_dot.chars().take_while(char::is_ascii_digit).count();
            if digits > 0 && after_dot[digits..].starts_with(suffix) {
                return true;
            }
        }
    }
    false
}

pub fn is_gemini3_pro_model(model_id: &str) -> bool {
    matches_gemini3_variant(model_id, "-pro")
}

pub fn is_gemini3_flash_model(model_id: &str) -> bool {
    let id = model_id.to_lowercase();
    matches_gemini3_variant(&id, "-flash")
        || id == "gemini-flash-latest"
        || id == "gemini-flash-lite-latest"
}

/// `/gemma-?4/`
pub fn is_gemma4_model(model_id: &str) -> bool {
    let id = model_id.to_lowercase();
    id.contains("gemma4") || id.contains("gemma-4")
}

/// Gemini 3+ enforces required function parameters in validated tool-calling modes.
pub fn supports_google_strict_tool_sampling(model_id: &str) -> bool {
    gemini_major_version(model_id).is_some_and(|v| v >= 3)
}

fn normalize_tool_call_id(model_id: &str, id: &str) -> String {
    if !requires_tool_call_id(model_id) {
        return id.to_string();
    }
    let sanitized: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    sanitized.chars().take(64).collect()
}

/// Port of `convertMessages`: internal messages to Gemini `Content[]`.
pub fn convert_messages(model: &Model, context: &Context) -> Vec<Content> {
    let mut contents: Vec<Content> = Vec::new();
    let model_id = model.id.clone();
    let normalize = move |id: &str| normalize_tool_call_id(&model_id, id);
    // Upstream's normalizer is `(id, model, source)`; Google only needs the id.
    let normalize_id =
        |id: &str, _model: &Model, _source: &pi_core::AssistantMessage| normalize(id);
    let transformed = transform_messages(&context.messages, model, Some(&normalize_id));

    for msg in &transformed {
        match msg {
            Message::User(user) => {
                let blocks = user.content.blocks();
                let parts: Vec<Part> = blocks
                    .iter()
                    .map(|item| match item {
                        InputContent::Text(text) => {
                            Part::text(sanitize_surrogates(&text.text).to_string())
                        }
                        InputContent::Image(image) => {
                            Part::inline_data(&image.mime_type, &image.data)
                        }
                    })
                    .collect();
                if parts.is_empty() {
                    continue;
                }
                contents.push(Content::new("user", parts));
            }
            Message::Assistant(assistant) => {
                let mut parts: Vec<Part> = Vec::new();
                // Thinking blocks and signatures only replay for the same provider+model.
                let same = assistant.provider == model.provider && assistant.model == model.id;

                for block in &assistant.content {
                    match block {
                        AssistantContent::Text(text) => {
                            let signature =
                                resolve_thought_signature(same, text.text_signature.as_deref());
                            // Gemini can attach a signature to a part whose visible text is
                            // empty and requires it echoed back; dropping it breaks the
                            // reasoning chain, so empty blocks are only skipped when unsigned.
                            if text.text.trim().is_empty() && signature.is_none() {
                                continue;
                            }
                            parts.push(Part {
                                text: Some(sanitize_surrogates(&text.text).to_string()),
                                thought_signature: signature,
                                ..Default::default()
                            });
                        }
                        AssistantContent::Thinking(thinking) => {
                            if same {
                                let signature = resolve_thought_signature(
                                    same,
                                    thinking.thinking_signature.as_deref(),
                                );
                                if thinking.thinking.trim().is_empty() && signature.is_none() {
                                    continue;
                                }
                                parts.push(Part {
                                    thought: Some(true),
                                    text: Some(sanitize_surrogates(&thinking.thinking).to_string()),
                                    thought_signature: signature,
                                    ..Default::default()
                                });
                            } else {
                                // Cross-model: the signature is unusable and the text becomes
                                // plain text (untagged, so the model does not mimic markers).
                                if thinking.thinking.trim().is_empty() {
                                    continue;
                                }
                                parts.push(Part::text(
                                    sanitize_surrogates(&thinking.thinking).to_string(),
                                ));
                            }
                        }
                        AssistantContent::ToolCall(call) => {
                            let signature =
                                resolve_thought_signature(same, call.thought_signature.as_deref());
                            parts.push(Part {
                                function_call: Some(FunctionCall {
                                    id: requires_tool_call_id(&model.id).then(|| call.id.clone()),
                                    args: Some(call.arguments.clone()),
                                    name: Some(call.name.clone()),
                                }),
                                thought_signature: signature,
                                ..Default::default()
                            });
                        }
                    }
                }

                if parts.is_empty() {
                    continue;
                }
                contents.push(Content::new("model", parts));
            }
            Message::ToolResult(result) => {
                let text_result = result
                    .content
                    .iter()
                    .filter_map(|c| c.as_text().map(|t| t.text.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n");
                let image_parts: Vec<Part> = if model.supports_images() {
                    result
                        .content
                        .iter()
                        .filter_map(|c| match c {
                            InputContent::Image(image) => Some(Part {
                                inline_data: Some(Blob {
                                    mime_type: image.mime_type.clone(),
                                    data: image.data.clone(),
                                }),
                                ..Default::default()
                            }),
                            InputContent::Text(_) => None,
                        })
                        .collect()
                } else {
                    Vec::new()
                };

                let has_text = !text_result.is_empty();
                let has_images = !image_parts.is_empty();
                let multimodal = supports_multimodal_function_response(&model.id);

                let response_value = if has_text {
                    sanitize_surrogates(&text_result).to_string()
                } else if has_images {
                    "(see attached image)".to_string()
                } else {
                    String::new()
                };
                let mut response = Map::new();
                response.insert(
                    if result.is_error { "error" } else { "output" }.to_string(),
                    Value::String(response_value),
                );

                let function_response_part = Part {
                    function_response: Some(FunctionResponse {
                        name: Some(result.tool_name.clone()),
                        response: Some(Value::Object(response)),
                        parts: (has_images && multimodal).then(|| image_parts.clone()),
                        id: requires_tool_call_id(&model.id).then(|| result.tool_call_id.clone()),
                    }),
                    ..Default::default()
                };

                // Cloud Code Assist requires all function responses in one user turn.
                let mergeable = contents.last().is_some_and(|last| {
                    last.role.as_deref() == Some("user")
                        && last.parts().iter().any(|p| p.function_response.is_some())
                });
                if mergeable {
                    contents
                        .last_mut()
                        .expect("checked above")
                        .parts_mut()
                        .push(function_response_part);
                } else {
                    contents.push(Content::new("user", vec![function_response_part]));
                }

                // Gemini < 3 cannot nest images, so they get their own user turn.
                if has_images && !multimodal {
                    let mut parts = vec![Part::text("Tool result image:")];
                    parts.extend(image_parts);
                    contents.push(Content::new("user", parts));
                }
            }
        }
    }

    contents
}

const JSON_SCHEMA_META_DECLARATIONS: [&str; 8] = [
    "$schema",
    "$id",
    "$anchor",
    "$dynamicAnchor",
    "$vocabulary",
    "$comment",
    "$defs",
    // pre-draft-2019-09 equivalent of $defs
    "definitions",
];

/// Strip JSON Schema meta-declarations, which OpenAPI 3.03 schemas reject.
fn sanitize_for_openapi(schema: &Value) -> Value {
    let Some(object) = schema.as_object() else {
        return schema.clone();
    };
    let mut result = Map::new();
    for (key, value) in object {
        if JSON_SCHEMA_META_DECLARATIONS.contains(&key.as_str()) {
            continue;
        }
        result.insert(key.clone(), sanitize_for_openapi(value));
    }
    Value::Object(result)
}

/// Port of `convertTools`.
///
/// Defaults to `parametersJsonSchema`, which accepts full JSON Schema. Setting
/// `use_parameters` switches to the legacy OpenAPI `parameters` field, needed
/// by Cloud Code Assist with Claude models.
pub fn convert_tools(
    tools: &[Tool],
    use_parameters: bool,
    supports_strict_mode: bool,
) -> Result<Option<Vec<Value>>, AiError> {
    if tools.is_empty() {
        return Ok(None);
    }
    let mut declarations = Vec::with_capacity(tools.len());
    for tool in tools {
        let strict = resolve_json_schema_strict_sampling(tool, supports_strict_mode)?;
        let parameters = json_schema_tool_parameters(tool, strict);
        let mut declaration = Map::new();
        declaration.insert("name".into(), Value::String(tool.name.clone()));
        declaration.insert(
            "description".into(),
            Value::String(tool.description.clone()),
        );
        if use_parameters {
            declaration.insert("parameters".into(), sanitize_for_openapi(&parameters));
        } else {
            declaration.insert("parametersJsonSchema".into(), parameters);
        }
        declarations.push(Value::Object(declaration));
    }
    Ok(Some(vec![serde_json::json!({
        "functionDeclarations": declarations
    })]))
}

/// `"auto" | "none" | "any"` to a Gemini function calling mode.
pub fn map_tool_choice(choice: &str) -> FunctionCallingConfigMode {
    match choice {
        "none" => FunctionCallingConfigMode::None,
        "any" => FunctionCallingConfigMode::Any,
        _ => FunctionCallingConfigMode::Auto,
    }
}

/// Port of `resolveGoogleFunctionCallingMode`.
pub fn resolve_google_function_calling_mode(
    tools: &[Tool],
    tool_choice: Option<&str>,
    supports_strict_mode: bool,
) -> Result<Option<FunctionCallingConfigMode>, AiError> {
    let mut use_strict_mode = false;
    for tool in tools {
        if resolve_json_schema_strict_sampling(tool, supports_strict_mode)? == Some(true) {
            use_strict_mode = true;
        }
    }
    if matches!(tool_choice, Some("none") | Some("any")) {
        return Ok(Some(map_tool_choice(tool_choice.unwrap())));
    }
    if use_strict_mode {
        return Ok(Some(FunctionCallingConfigMode::Validated));
    }
    Ok(tool_choice.map(map_tool_choice))
}

/// Port of `mapStopReasonString`. Every finish reason other than `STOP` and
/// `MAX_TOKENS` — safety blocks, recitation, malformed function calls — is an
/// error as far as pi is concerned.
pub fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "STOP" => StopReason::Stop,
        "MAX_TOKENS" => StopReason::Length,
        _ => StopReason::Error,
    }
}

/// Port of `calculateCost` from `models.ts`. Lives in `pi-provider-common`;
/// re-exported so `stream.rs` and the crate root keep their spelling.
pub use pi_provider_common::cost::calculate_cost;

/// `Exclude<ThinkingLevel, "xhigh" | "max">` — the levels Google budgets cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClampedThinkingLevel {
    Minimal,
    Low,
    Medium,
    High,
}

impl ClampedThinkingLevel {
    /// `clampedReasoning === "off" ? "high" : clampedReasoning` from upstream.
    pub fn from_model_level(level: ModelThinkingLevel) -> Self {
        match level {
            ModelThinkingLevel::Minimal => ClampedThinkingLevel::Minimal,
            ModelThinkingLevel::Low => ClampedThinkingLevel::Low,
            ModelThinkingLevel::Medium => ClampedThinkingLevel::Medium,
            // "off", "high", "xhigh" and "max" all land on high.
            _ => ClampedThinkingLevel::High,
        }
    }

    pub fn budget_from(self, budgets: &pi_core::model::ThinkingBudgets) -> Option<u32> {
        match self {
            ClampedThinkingLevel::Minimal => budgets.minimal,
            ClampedThinkingLevel::Low => budgets.low,
            ClampedThinkingLevel::Medium => budgets.medium,
            ClampedThinkingLevel::High => budgets.high,
        }
    }
}

/// Port of `clampMaxTokensToContext` from `simple-options.ts`. Lives in
/// `pi-provider-common`, on top of `pi_http::estimate`.
///
/// The copy that used to live here estimated context tokens with its own
/// ad-hoc walk: it counted UTF-8 bytes rather than UTF-16 code units, summed
/// tool `name + description + parameters` instead of the serialized tool array,
/// and — unlike upstream — ignored assistant `usage` blocks entirely, so it
/// re-estimated a prefix the provider had already reported an exact count for.
pub use pi_provider_common::simple_options::clamp_max_tokens_to_context;

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::content::{TextContent, ThinkingContent, ToolCall};
    use pi_core::message::{AssistantMessage, ToolResultMessage, UserMessage};
    use pi_core::model::{Api, Modality};
    use pi_core::tool::{ConstrainedSampling, ConstrainedSamplingConfig, StrictMode};

    const VALID_SIG: &str = "AAAAAAAAAAAAAAAAAAAAAA==";

    fn model(id: &str) -> Model {
        let mut m = Model::new(id, Api::GoogleGenerativeAi, "google", "https://example.com");
        m.reasoning = true;
        m.input = vec![Modality::Text, Modality::Image];
        m
    }

    fn assistant(model: &Model, content: Vec<AssistantContent>) -> Message {
        let mut msg = AssistantMessage::pending(model.api.as_str(), &model.provider, &model.id);
        msg.content = content;
        msg.stop_reason = StopReason::ToolUse;
        Message::Assistant(msg)
    }

    // --- google-thinking-signature.test.ts ---

    #[test]
    fn thought_true_is_the_only_thinking_marker() {
        assert!(is_thinking_part(&Part {
            thought: Some(true),
            ..Default::default()
        }));
        assert!(!is_thinking_part(&Part {
            thought_signature: Some("opaque".into()),
            ..Default::default()
        }));
        assert!(!is_thinking_part(&Part {
            thought: Some(false),
            thought_signature: Some("opaque".into()),
            ..Default::default()
        }));
        assert!(!is_thinking_part(&Part::default()));
    }

    #[test]
    fn retains_signature_across_deltas() {
        let first = retain_thought_signature(None, Some("sig-1"));
        assert_eq!(first.as_deref(), Some("sig-1"));
        let second = retain_thought_signature(first, None);
        assert_eq!(second.as_deref(), Some("sig-1"));
        let third = retain_thought_signature(second, Some(""));
        assert_eq!(third.as_deref(), Some("sig-1"));
        let fourth = retain_thought_signature(third, Some("sig-2"));
        assert_eq!(fourth.as_deref(), Some("sig-2"));
    }

    // --- google-shared-gemini3-unsigned-tool-call.test.ts ---

    #[test]
    fn requires_tool_call_id_matrix() {
        assert!(!requires_tool_call_id("gemini-2.5-flash"));
        assert!(requires_tool_call_id("gemini-3.6-flash"));
        assert!(requires_tool_call_id("claude-sonnet-4-5"));
        assert!(requires_tool_call_id("gpt-oss-120b"));
    }

    #[test]
    fn preserves_tool_call_ids_for_gemini3() {
        let m = model("gemini-3-pro-preview");
        let context = Context::new(vec![
            Message::User(UserMessage::text("Hi")),
            assistant(
                &m,
                vec![
                    AssistantContent::ToolCall(ToolCall::new("call_1", "bash")),
                    AssistantContent::ToolCall(ToolCall::new("call_2", "bash")),
                ],
            ),
            Message::ToolResult(ToolResultMessage::text("call_1", "bash", "hi", false)),
            Message::ToolResult(ToolResultMessage::text("call_2", "bash", "files", false)),
        ]);
        let contents = convert_messages(&m, &context);
        let call_ids: Vec<_> = contents
            .iter()
            .flat_map(|c| c.parts())
            .filter_map(|p| p.function_call.as_ref().and_then(|f| f.id.clone()))
            .collect();
        let response_ids: Vec<_> = contents
            .iter()
            .flat_map(|c| c.parts())
            .filter_map(|p| p.function_response.as_ref().and_then(|f| f.id.clone()))
            .collect();
        assert_eq!(call_ids, vec!["call_1", "call_2"]);
        assert_eq!(response_ids, vec!["call_1", "call_2"]);
    }

    #[test]
    fn omits_ids_and_signatures_for_pre_gemini3() {
        let m = model("gemini-2.5-flash");
        let mut other = m.clone();
        other.id = "other-model".into();
        let context = Context::new(vec![
            Message::User(UserMessage::text("Hi")),
            assistant(
                &other,
                vec![AssistantContent::ToolCall(ToolCall {
                    thought_signature: Some(VALID_SIG.into()),
                    ..ToolCall::new("call_1", "bash")
                })],
            ),
            Message::ToolResult(ToolResultMessage::text("call_1", "bash", "hi", false)),
        ]);
        let contents = convert_messages(&m, &context);
        let model_turn = contents
            .iter()
            .find(|c| c.role.as_deref() == Some("model"))
            .unwrap();
        assert!(model_turn.parts()[0]
            .function_call
            .as_ref()
            .unwrap()
            .id
            .is_none());
        assert!(model_turn.parts()[0].thought_signature.is_none());
    }

    #[test]
    fn keeps_valid_signature_for_same_provider_and_model() {
        let m = model("gemini-3-pro-preview");
        let context = Context::new(vec![assistant(
            &m,
            vec![
                AssistantContent::ToolCall(ToolCall {
                    thought_signature: Some(VALID_SIG.into()),
                    ..ToolCall::new("call_1", "bash")
                }),
                AssistantContent::ToolCall(ToolCall::new("call_2", "bash")),
            ],
        )]);
        let contents = convert_messages(&m, &context);
        let model_turn = contents
            .iter()
            .find(|c| c.role.as_deref() == Some("model"))
            .unwrap();
        assert_eq!(
            model_turn.parts()[0].thought_signature.as_deref(),
            Some(VALID_SIG)
        );
        assert!(model_turn.parts()[1].thought_signature.is_none());
    }

    // --- google-shared-signed-empty-blocks.test.ts ---

    #[test]
    fn keeps_signed_empty_blocks_and_drops_unsigned_ones() {
        let m = model("gemini-3-pro-preview");
        let context = Context::new(vec![assistant(
            &m,
            vec![
                AssistantContent::Thinking(ThinkingContent {
                    thinking: String::new(),
                    thinking_signature: Some(VALID_SIG.into()),
                    redacted: false,
                }),
                AssistantContent::ToolCall(ToolCall::new("call_1", "bash")),
            ],
        )]);
        let contents = convert_messages(&m, &context);
        let model_turn = contents
            .iter()
            .find(|c| c.role.as_deref() == Some("model"))
            .unwrap();
        let signed: Vec<_> = model_turn
            .parts()
            .iter()
            .filter(|p| p.thought_signature.as_deref() == Some(VALID_SIG))
            .collect();
        assert_eq!(signed.len(), 1);
        assert_eq!(signed[0].thought, Some(true));

        let context = Context::new(vec![assistant(
            &m,
            vec![
                AssistantContent::Text(TextContent {
                    text: String::new(),
                    text_signature: Some(VALID_SIG.into()),
                }),
                AssistantContent::ToolCall(ToolCall::new("call_1", "bash")),
            ],
        )]);
        let contents = convert_messages(&m, &context);
        let model_turn = contents
            .iter()
            .find(|c| c.role.as_deref() == Some("model"))
            .unwrap();
        assert_eq!(
            model_turn
                .parts()
                .iter()
                .filter(|p| p.thought_signature.as_deref() == Some(VALID_SIG))
                .count(),
            1
        );

        let context = Context::new(vec![assistant(
            &m,
            vec![
                AssistantContent::thinking(""),
                AssistantContent::text("   "),
                AssistantContent::ToolCall(ToolCall::new("call_1", "bash")),
            ],
        )]);
        let contents = convert_messages(&m, &context);
        let model_turn = contents
            .iter()
            .find(|c| c.role.as_deref() == Some("model"))
            .unwrap();
        assert_eq!(model_turn.parts().len(), 1);
        assert!(model_turn.parts()[0].function_call.is_some());
    }

    #[test]
    fn drops_signed_empty_blocks_from_a_different_model() {
        let m = model("gemini-3-pro-preview");
        let mut other = m.clone();
        other.id = "other-model".into();
        let context = Context::new(vec![assistant(
            &other,
            vec![
                AssistantContent::Thinking(ThinkingContent {
                    thinking: String::new(),
                    thinking_signature: Some(VALID_SIG.into()),
                    redacted: false,
                }),
                AssistantContent::Text(TextContent {
                    text: String::new(),
                    text_signature: Some(VALID_SIG.into()),
                }),
                AssistantContent::ToolCall(ToolCall::new("call_1", "bash")),
            ],
        )]);
        let contents = convert_messages(&m, &context);
        let model_turn = contents
            .iter()
            .find(|c| c.role.as_deref() == Some("model"))
            .unwrap();
        assert_eq!(model_turn.parts().len(), 1);
        assert!(model_turn.parts()[0].function_call.is_some());
        assert!(!serde_json::to_string(model_turn)
            .unwrap()
            .contains(VALID_SIG));
    }

    // --- google-shared-image-tool-result-routing.test.ts ---

    fn image_routing_context(m: &Model) -> Context {
        Context::new(vec![
            Message::User(UserMessage::text("read the files")),
            assistant(
                m,
                vec![
                    AssistantContent::ToolCall(ToolCall::new("call_a", "read")),
                    AssistantContent::ToolCall(ToolCall::new("call_img", "read")),
                    AssistantContent::ToolCall(ToolCall::new("call_b", "read")),
                ],
            ),
            Message::ToolResult(ToolResultMessage::text(
                "call_a",
                "read",
                "alpha text",
                false,
            )),
            Message::ToolResult(ToolResultMessage {
                content: vec![InputContent::image("abc", "image/png")],
                ..ToolResultMessage::text("call_img", "read", "", false)
            }),
            Message::ToolResult(ToolResultMessage::text(
                "call_b",
                "read",
                "beta text",
                false,
            )),
        ])
    }

    #[test]
    fn separate_image_turn_for_gemini2() {
        let m = model("gemini-2.5-flash");
        let contents = convert_messages(&m, &image_routing_context(&m));
        assert_eq!(contents.len(), 5);
        assert!(contents[2]
            .parts()
            .iter()
            .all(|p| p.function_response.is_some()));
        assert_eq!(
            contents[3].parts()[0].text.as_deref(),
            Some("Tool result image:")
        );
        assert!(contents[3].parts()[1].inline_data.is_some());
        assert!(contents[4].parts()[0].function_response.is_some());
    }

    #[test]
    fn nested_image_response_for_gemini3() {
        let m = model("gemini-3-pro-preview");
        let contents = convert_messages(&m, &image_routing_context(&m));
        assert_eq!(contents.len(), 3);
        let turn = &contents[2];
        assert_eq!(turn.parts().len(), 3);
        let nested = turn.parts()[1].function_response.as_ref().unwrap();
        assert_eq!(nested.parts.as_ref().unwrap().len(), 1);
        assert!(nested.parts.as_ref().unwrap()[0].inline_data.is_some());
    }

    // --- google-shared-convert-tools.test.ts ---

    fn tool(parameters: Value) -> Tool {
        Tool::new("test_tool", "A test tool", parameters)
    }

    #[test]
    fn strips_meta_keys_when_use_parameters() {
        let tools = vec![tool(serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "$id": "urn:bash-tool",
            "$comment": "A bash tool",
            "$defs": {"commandDef": {"type": "string"}},
            "definitions": {"legacyDef": {"type": "number"}},
            "type": "object",
            "properties": {"command": {"type": "string"}},
            "required": ["command"]
        }))];
        let result = convert_tools(&tools, true, false).unwrap().unwrap();
        let declaration = &result[0]["functionDeclarations"][0];
        assert_eq!(
            declaration["parameters"],
            serde_json::json!({
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"]
            })
        );
    }

    #[test]
    fn strips_nested_meta_keys_but_keeps_ref() {
        let tools = vec![tool(serde_json::json!({
            "$schema": "s",
            "type": "object",
            "properties": {
                "deep": {"$schema": "s", "$id": "urn:nested", "type": "string"},
                "refProp": {"$ref": "#/$defs/someDef", "type": "string"}
            }
        }))];
        let result = convert_tools(&tools, true, false).unwrap().unwrap();
        let declaration = &result[0]["functionDeclarations"][0];
        assert_eq!(
            declaration["parameters"]["properties"],
            serde_json::json!({
                "deep": {"type": "string"},
                "refProp": {"$ref": "#/$defs/someDef", "type": "string"}
            })
        );
    }

    #[test]
    fn keeps_schema_in_parameters_json_schema() {
        let raw = serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {"command": {"type": "string"}},
            "required": ["command"]
        });
        let tools = vec![tool(raw.clone())];
        let result = convert_tools(&tools, false, false).unwrap().unwrap();
        assert_eq!(
            result[0]["functionDeclarations"][0]["parametersJsonSchema"],
            raw
        );
    }

    #[test]
    fn empty_tool_list_is_none() {
        assert!(convert_tools(&[], false, false).unwrap().is_none());
        assert!(convert_tools(&[], true, false).unwrap().is_none());
    }

    #[test]
    fn validated_mode_for_strict_tools_on_gemini3() {
        let mut t = tool(serde_json::json!({"type": "object", "properties": {}}));
        t.constrained_sampling = Some(ConstrainedSampling::Config(
            ConstrainedSamplingConfig::JsonSchema {
                strict: StrictMode::Require,
            },
        ));
        assert!(supports_google_strict_tool_sampling(
            "gemini-3.1-pro-preview"
        ));
        assert!(!supports_google_strict_tool_sampling("gemini-2.5-pro"));
        assert_eq!(
            resolve_google_function_calling_mode(std::slice::from_ref(&t), None, true).unwrap(),
            Some(FunctionCallingConfigMode::Validated)
        );
        let err = resolve_google_function_calling_mode(&[t], None, false).unwrap_err();
        assert!(err
            .message()
            .contains("requires JSON-schema constrained sampling"));
    }

    #[test]
    fn model_family_detection() {
        assert!(is_gemini3_pro_model("gemini-3-pro-preview"));
        assert!(is_gemini3_pro_model("gemini-3.1-pro-preview"));
        assert!(!is_gemini3_pro_model("gemini-2.5-pro"));
        assert!(is_gemini3_flash_model("gemini-3-flash-preview"));
        assert!(is_gemini3_flash_model("gemini-flash-latest"));
        assert!(is_gemini3_flash_model("gemini-flash-lite-latest"));
        assert!(!is_gemini3_flash_model("gemini-2.5-flash"));
        assert!(is_gemma4_model("gemma-4-27b"));
        assert!(is_gemma4_model("gemma4"));
    }

    #[test]
    fn stop_reason_mapping() {
        assert_eq!(map_stop_reason("STOP"), StopReason::Stop);
        assert_eq!(map_stop_reason("MAX_TOKENS"), StopReason::Length);
        assert_eq!(map_stop_reason("SAFETY"), StopReason::Error);
        assert_eq!(
            map_stop_reason("MALFORMED_FUNCTION_CALL"),
            StopReason::Error
        );
    }
}
