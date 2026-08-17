//! Request-body construction for `POST /v1/messages`.
//!
//! Port of `buildParams`, `convertMessages`, `convertTools` and the cache
//! control helpers in `packages/ai/src/api/anthropic-messages.ts`.

use std::collections::HashSet;

use serde_json::{json, Map, Value};

use pi_core::content::{AssistantContent, InputContent};
use pi_core::message::{Message, ToolResultMessage};
use pi_core::model::{CacheRetention, Model, ModelCompat};
use pi_core::options::{ProviderEnv, StreamOptions};
use pi_core::tool::{Context, Tool};
use pi_core::AiError;

use crate::deferred_tools::split_deferred_tools;
use crate::options::{AnthropicOptions, AnthropicThinkingDisplay};
use pi_provider_common::constrained_sampling::{
    json_schema_tool_parameters, resolve_json_schema_strict_sampling,
};
use pi_provider_common::transform_messages::transform_messages;

pub const FINE_GRAINED_TOOL_STREAMING_BETA: &str = "fine-grained-tool-streaming-2025-05-14";
pub const INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-05-14";

/// Stealth mode: mimic Claude Code's identity exactly for OAuth tokens.
pub const CLAUDE_CODE_VERSION: &str = "2.1.75";

/// Claude Code 2.x tool names (canonical casing).
/// Source: <https://cchistory.mariozechner.at/data/prompts-2.1.11.md>
const CLAUDE_CODE_TOOLS: [&str; 17] = [
    "Read",
    "Write",
    "Edit",
    "Bash",
    "Grep",
    "Glob",
    "AskUserQuestion",
    "EnterPlanMode",
    "ExitPlanMode",
    "KillShell",
    "NotebookEdit",
    "Skill",
    "Task",
    "TaskOutput",
    "TodoWrite",
    "WebFetch",
    "WebSearch",
];

/// Convert a tool name to Claude Code canonical casing if it matches.
pub fn to_claude_code_name(name: &str) -> String {
    CLAUDE_CODE_TOOLS
        .iter()
        .find(|candidate| candidate.eq_ignore_ascii_case(name))
        .map(|candidate| (*candidate).to_string())
        .unwrap_or_else(|| name.to_string())
}

/// Map a Claude Code canonical name back to the caller's spelling.
pub fn from_claude_code_name(name: &str, tools: &[Tool]) -> String {
    tools
        .iter()
        .find(|tool| tool.name.eq_ignore_ascii_case(name))
        .map(|tool| tool.name.clone())
        .unwrap_or_else(|| name.to_string())
}

pub fn is_oauth_token(api_key: &str) -> bool {
    api_key.contains("sk-ant-oat")
}

/// Resolved `AnthropicMessagesCompat` with upstream's defaults applied.
#[derive(Debug, Clone, Copy)]
pub struct AnthropicCompat {
    pub supports_eager_tool_input_streaming: bool,
    pub supports_long_cache_retention: bool,
    pub send_session_affinity_headers: bool,
    pub supports_cache_control_on_tools: bool,
    pub supports_temperature: bool,
    pub allow_empty_signature: bool,
    pub supports_strict_tools: bool,
    pub supports_tool_references: bool,
    pub force_adaptive_thinking: bool,
}

pub fn anthropic_compat(model: &Model) -> AnthropicCompat {
    let compat: Option<&ModelCompat> = model.compat.as_ref();
    AnthropicCompat {
        supports_eager_tool_input_streaming: compat
            .and_then(|c| c.supports_eager_tool_input_streaming)
            .unwrap_or(true),
        supports_long_cache_retention: compat
            .and_then(|c| c.supports_long_cache_retention)
            .unwrap_or(true),
        send_session_affinity_headers: compat
            .and_then(|c| c.send_session_affinity_headers)
            .unwrap_or(false),
        supports_cache_control_on_tools: compat
            .and_then(|c| c.supports_cache_control_on_tools)
            .unwrap_or(true),
        supports_temperature: compat.and_then(|c| c.supports_temperature).unwrap_or(true),
        allow_empty_signature: compat
            .and_then(|c| c.allow_empty_signature)
            .unwrap_or(false),
        supports_strict_tools: compat
            .and_then(|c| c.supports_strict_tools)
            .unwrap_or(false),
        supports_tool_references: compat
            .and_then(|c| c.supports_tool_references)
            .unwrap_or_else(|| default_supports_tool_references(model)),
        force_adaptive_thinking: compat
            .and_then(|c| c.force_adaptive_thinking)
            .unwrap_or(false),
    }
}

/// Default for `supportsToolReferences`: first-party Anthropic models except
/// Haiku (rejects client-side `tool_reference` blocks) and models that predate
/// tool search (Claude 3.x, Opus/Sonnet 4.0, Opus 4.1).
fn default_supports_tool_references(model: &Model) -> bool {
    if model.provider != "anthropic" || model.id.contains("haiku") {
        return false;
    }
    // Equivalent of /^claude-(?:opus|sonnet|fable)-(\d+)(?:-(\d+))?(?:-|$)/.
    let Some(rest) = model.id.strip_prefix("claude-") else {
        return false;
    };
    let rest = ["opus-", "sonnet-", "fable-"]
        .iter()
        .find_map(|family| rest.strip_prefix(family));
    let Some(rest) = rest else { return false };

    let major_str: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if major_str.is_empty() {
        return false;
    }
    let after_major = &rest[major_str.len()..];
    let (minor_str, tail) = match after_major.strip_prefix('-') {
        Some(after_dash) => {
            let digits: String = after_dash
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if digits.is_empty() {
                // `claude-opus-4-` with a non-numeric segment: the optional group
                // does not match, so the regex needs the `-` boundary, which we have.
                (String::new(), after_dash)
            } else {
                (digits.clone(), &after_dash[digits.len()..])
            }
        }
        None => (String::new(), after_major),
    };
    // The regex requires the match to end at a `-` or end-of-string.
    if !(tail.is_empty() || tail.starts_with('-')) {
        return false;
    }
    let Ok(major) = major_str.parse::<u32>() else {
        return false;
    };
    // Date-stamped ids (`claude-opus-4-20250514`) do not count as a minor version.
    let minor = if !minor_str.is_empty() && minor_str.len() < 8 {
        minor_str.parse::<u32>().unwrap_or(0)
    } else {
        0
    };
    major > 4 || (major == 4 && minor >= 5)
}

/// Resolve cache retention: explicit option, then `PI_CACHE_RETENTION`.
pub fn resolve_cache_retention(
    cache_retention: Option<CacheRetention>,
    env: &ProviderEnv,
) -> CacheRetention {
    if let Some(retention) = cache_retention {
        return retention;
    }
    if provider_env_value("PI_CACHE_RETENTION", env).as_deref() == Some("long") {
        return CacheRetention::Long;
    }
    CacheRetention::Short
}

/// Scoped override first, then the process environment. `pi-http` owns the
/// port of `utils/provider-env.ts`; re-exported so the call sites here stay
/// diffable against upstream.
pub use pi_http::provider_env::get_provider_env_value as provider_env_value;

/// The `cache_control` block for this request, if caching is on.
pub fn cache_control_for(
    model: &Model,
    cache_retention: Option<CacheRetention>,
    env: &ProviderEnv,
) -> (CacheRetention, Option<Value>) {
    let retention = resolve_cache_retention(cache_retention, env);
    if retention == CacheRetention::None {
        return (retention, None);
    }
    let long =
        retention == CacheRetention::Long && anthropic_compat(model).supports_long_cache_retention;
    let control = if long {
        json!({ "type": "ephemeral", "ttl": "1h" })
    } else {
        json!({ "type": "ephemeral" })
    };
    (retention, Some(control))
}

/// Anthropic requires tool call ids to match `^[a-zA-Z0-9_-]+$`, max 64 chars.
pub fn normalize_tool_call_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

/// Whether the legacy fine-grained tool streaming beta header is needed.
pub fn should_use_fine_grained_tool_streaming_beta(model: &Model, context: &Context) -> bool {
    !context.tools().is_empty() && !anthropic_compat(model).supports_eager_tool_input_streaming
}

fn convert_content_blocks(content: &[InputContent]) -> Value {
    let has_images = content
        .iter()
        .any(|block| matches!(block, InputContent::Image(_)));
    if !has_images {
        let text = content
            .iter()
            .map(|block| match block {
                InputContent::Text(text) => text.text.as_str(),
                InputContent::Image(_) => "",
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Value::String(text);
    }

    let mut blocks: Vec<Value> = content
        .iter()
        .map(|block| match block {
            InputContent::Text(text) => json!({ "type": "text", "text": text.text }),
            InputContent::Image(image) => json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": image.mime_type,
                    "data": image.data,
                }
            }),
        })
        .collect();

    let has_text = blocks
        .iter()
        .any(|block| block.get("type") == Some(&Value::String("text".into())));
    if !has_text {
        blocks.insert(0, json!({ "type": "text", "text": "(see attached image)" }));
    }
    Value::Array(blocks)
}

struct ConvertedToolResult {
    tool_result: Value,
    sibling_content: Vec<Value>,
}

fn convert_tool_result(
    msg: &ToolResultMessage,
    is_oauth: bool,
    deferred_tool_names: &HashSet<String>,
    loaded_tool_names: &mut HashSet<String>,
    normalize_tool_name: &dyn Fn(&str) -> String,
) -> ConvertedToolResult {
    let mut references: Vec<Value> = Vec::new();
    for name in msg.added_tool_names.iter().flatten() {
        let normalized = normalize_tool_name(name);
        if !deferred_tool_names.contains(&normalized) || loaded_tool_names.contains(&normalized) {
            continue;
        }
        loaded_tool_names.insert(normalized);
        references.push(json!({
            "type": "tool_reference",
            "tool_name": if is_oauth { to_claude_code_name(name) } else { name.clone() },
        }));
    }

    let converted_content = convert_content_blocks(&msg.content);
    // Anthropic rejects tool references mixed with ordinary tool-result content,
    // so the real output is displaced into sibling blocks.
    let sibling_content = if references.is_empty() {
        Vec::new()
    } else {
        match &converted_content {
            Value::String(text) => vec![json!({ "type": "text", "text": text })],
            Value::Array(blocks) => blocks.clone(),
            _ => Vec::new(),
        }
    };
    let content = if references.is_empty() {
        converted_content
    } else {
        Value::Array(references)
    };

    ConvertedToolResult {
        tool_result: json!({
            "type": "tool_result",
            "tool_use_id": msg.tool_call_id,
            "content": content,
            "is_error": msg.is_error,
        }),
        sibling_content,
    }
}

fn convert_messages(
    transformed: &[Message],
    is_oauth: bool,
    cache_control: Option<&Value>,
    allow_empty_signature: bool,
    deferred_tool_names: &HashSet<String>,
    normalize_tool_name: &dyn Fn(&str) -> String,
) -> Vec<Value> {
    let mut params: Vec<Value> = Vec::new();
    let mut loaded_tool_names: HashSet<String> = HashSet::new();

    let mut i = 0usize;
    while i < transformed.len() {
        match &transformed[i] {
            Message::User(user) => match &user.content {
                pi_core::UserContent::Text(text) => {
                    if !text.trim().is_empty() {
                        params.push(json!({ "role": "user", "content": text }));
                    }
                }
                pi_core::UserContent::Blocks(blocks) => {
                    let filtered: Vec<Value> = blocks
                        .iter()
                        .filter(|block| match block {
                            InputContent::Text(text) => !text.text.trim().is_empty(),
                            InputContent::Image(_) => true,
                        })
                        .map(|block| match block {
                            InputContent::Text(text) => {
                                json!({ "type": "text", "text": text.text })
                            }
                            InputContent::Image(image) => json!({
                                "type": "image",
                                "source": {
                                    "type": "base64",
                                    "media_type": image.mime_type,
                                    "data": image.data,
                                }
                            }),
                        })
                        .collect();
                    if filtered.is_empty() {
                        i += 1;
                        continue;
                    }
                    params.push(json!({ "role": "user", "content": filtered }));
                }
            },
            Message::Assistant(assistant) => {
                let mut blocks: Vec<Value> = Vec::new();
                for block in &assistant.content {
                    match block {
                        AssistantContent::Text(text) => {
                            if text.text.trim().is_empty() {
                                continue;
                            }
                            blocks.push(json!({ "type": "text", "text": text.text }));
                        }
                        AssistantContent::Thinking(thinking) => {
                            if thinking.redacted {
                                blocks.push(json!({
                                    "type": "redacted_thinking",
                                    "data": thinking.thinking_signature.clone().unwrap_or_default(),
                                }));
                                continue;
                            }
                            let signature = thinking.thinking_signature.as_deref().unwrap_or("");
                            let has_signature = !signature.trim().is_empty();
                            if thinking.thinking.trim().is_empty() && !has_signature {
                                continue;
                            }
                            if !has_signature {
                                // An empty signature (aborted stream) is rejected by
                                // Anthropic, so downgrade to text unless the model is
                                // marked as tolerating it.
                                if allow_empty_signature {
                                    blocks.push(json!({
                                        "type": "thinking",
                                        "thinking": thinking.thinking,
                                        "signature": "",
                                    }));
                                } else {
                                    blocks.push(json!({
                                        "type": "text",
                                        "text": thinking.thinking,
                                    }));
                                }
                            } else {
                                blocks.push(json!({
                                    "type": "thinking",
                                    "thinking": thinking.thinking,
                                    "signature": signature,
                                }));
                            }
                        }
                        AssistantContent::ToolCall(tool_call) => {
                            blocks.push(json!({
                                "type": "tool_use",
                                "id": tool_call.id,
                                "name": if is_oauth { to_claude_code_name(&tool_call.name) } else { tool_call.name.clone() },
                                "input": Value::Object(tool_call.arguments.clone()),
                            }));
                        }
                    }
                }
                if blocks.is_empty() {
                    i += 1;
                    continue;
                }
                params.push(json!({ "role": "assistant", "content": blocks }));
            }
            Message::ToolResult(_) => {
                // Collect consecutive tool results into one user message.
                let mut tool_results: Vec<Value> = Vec::new();
                let mut sibling_content: Vec<Value> = Vec::new();
                let mut j = i;
                while let Some(Message::ToolResult(result)) = transformed.get(j) {
                    let converted = convert_tool_result(
                        result,
                        is_oauth,
                        deferred_tool_names,
                        &mut loaded_tool_names,
                        normalize_tool_name,
                    );
                    tool_results.push(converted.tool_result);
                    sibling_content.extend(converted.sibling_content);
                    j += 1;
                }
                i = j - 1;

                // Displaced reference-bearing results must follow every tool_result.
                tool_results.extend(sibling_content);
                params.push(json!({ "role": "user", "content": tool_results }));
            }
        }
        i += 1;
    }

    // Cache the conversation history at the last user block.
    if let Some(cache_control) = cache_control {
        if let Some(last) = params.last_mut() {
            if last.get("role") == Some(&Value::String("user".into())) {
                match last.get_mut("content") {
                    Some(Value::Array(blocks)) => {
                        if let Some(last_block) = blocks.last_mut() {
                            let kind = last_block.get("type").and_then(|t| t.as_str());
                            if matches!(kind, Some("text") | Some("image") | Some("tool_result")) {
                                if let Some(obj) = last_block.as_object_mut() {
                                    obj.insert("cache_control".into(), cache_control.clone());
                                }
                            }
                        }
                    }
                    Some(content @ Value::String(_)) => {
                        let text = content.as_str().unwrap_or_default().to_string();
                        *content = json!([{
                            "type": "text",
                            "text": text,
                            "cache_control": cache_control,
                        }]);
                    }
                    _ => {}
                }
            }
        }
    }

    params
}

#[allow(clippy::too_many_arguments)]
/// `{ type, properties, required }` — the legacy `input_schema` shape Anthropic
/// requires alongside (or instead of) a strict JSON Schema.
///
/// Anthropic-only, which is why it stayed here when the rest of
/// `constrained-sampling.ts` moved to `pi-provider-common`.
fn legacy_input_schema(parameters: &Value) -> Map<String, Value> {
    let mut legacy = Map::new();
    legacy.insert("type".into(), Value::String("object".into()));
    legacy.insert(
        "properties".into(),
        parameters
            .get("properties")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new())),
    );
    legacy.insert(
        "required".into(),
        parameters
            .get("required")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
    );
    legacy
}

fn convert_tools(
    tools: &[Tool],
    is_oauth: bool,
    supports_eager_tool_input_streaming: bool,
    supports_strict_tools: bool,
    cache_control: Option<&Value>,
    defer_loading: bool,
) -> Result<Vec<Value>, AiError> {
    let mut out = Vec::with_capacity(tools.len());
    for (index, tool) in tools.iter().enumerate() {
        let strict = resolve_json_schema_strict_sampling(tool, supports_strict_tools)?;
        let parameters = json_schema_tool_parameters(tool, strict);
        let legacy = legacy_input_schema(&parameters);
        let input_schema = if strict == Some(true) {
            let mut merged = parameters.as_object().cloned().unwrap_or_default();
            for (key, value) in legacy {
                merged.insert(key, value);
            }
            Value::Object(merged)
        } else {
            Value::Object(legacy)
        };

        let mut entry = Map::new();
        entry.insert(
            "name".into(),
            Value::String(if is_oauth {
                to_claude_code_name(&tool.name)
            } else {
                tool.name.clone()
            }),
        );
        entry.insert(
            "description".into(),
            Value::String(tool.description.clone()),
        );
        if supports_eager_tool_input_streaming {
            entry.insert("eager_input_streaming".into(), Value::Bool(true));
        }
        if strict == Some(true) {
            entry.insert("strict".into(), Value::Bool(true));
        }
        entry.insert("input_schema".into(), input_schema);
        if defer_loading {
            entry.insert("defer_loading".into(), Value::Bool(true));
        }
        if let Some(cache_control) = cache_control {
            if index == tools.len() - 1 {
                entry.insert("cache_control".into(), cache_control.clone());
            }
        }
        out.push(Value::Object(entry));
    }
    Ok(out)
}

/// The fully built `messages.create` body.
pub struct BuiltRequest {
    pub body: Value,
    /// `anthropic-beta` values this request needs.
    pub beta_features: Vec<String>,
}

/// Port of `buildParams` (plus the beta-header decision from `createClient`).
pub fn build_params(
    model: &Model,
    context: &Context,
    is_oauth: bool,
    options: &StreamOptions,
    anthropic: &AnthropicOptions,
) -> Result<BuiltRequest, AiError> {
    let compat = anthropic_compat(model);
    let (_, cache_control) =
        cache_control_for(model, options.cache_retention, &options.request.env);
    let cache_control = cache_control.as_ref();

    // Upstream's normalizer signature is `(id, model, source)`; Anthropic only
    // needs the id, so the extra arguments are ignored here.
    let normalize =
        |id: &str, _model: &Model, _source: &pi_core::AssistantMessage| normalize_tool_call_id(id);
    let transformed = transform_messages(&context.messages, model, Some(&normalize));

    let normalize_tool_name: Box<dyn Fn(&str) -> String> = if is_oauth {
        Box::new(|name: &str| to_claude_code_name(name))
    } else {
        Box::new(|name: &str| name.to_string())
    };

    let placement = split_deferred_tools(
        context,
        &transformed,
        compat.supports_tool_references,
        normalize_tool_name.as_ref(),
    );
    let mut immediate_tools = placement.immediate;
    let mut deferred_tools: Vec<Tool> = placement
        .deferred
        .into_iter()
        .map(|(_, tool)| tool)
        .collect();
    // Anthropic requires at least one non-deferred tool.
    if immediate_tools.is_empty() && !deferred_tools.is_empty() {
        immediate_tools = std::mem::take(&mut deferred_tools);
    }
    let deferred_tool_names: HashSet<String> = deferred_tools
        .iter()
        .map(|tool| normalize_tool_name(&tool.name))
        .collect();

    let mut params = Map::new();
    params.insert("model".into(), Value::String(model.id.clone()));
    params.insert(
        "messages".into(),
        Value::Array(convert_messages(
            &transformed,
            is_oauth,
            cache_control,
            compat.allow_empty_signature,
            &deferred_tool_names,
            normalize_tool_name.as_ref(),
        )),
    );
    params.insert(
        "max_tokens".into(),
        Value::from(options.max_tokens.unwrap_or(model.max_tokens)),
    );
    params.insert("stream".into(), Value::Bool(true));

    let system_block = |text: &str| -> Value {
        let mut block = Map::new();
        block.insert("type".into(), Value::String("text".into()));
        block.insert("text".into(), Value::String(text.to_string()));
        if let Some(cache_control) = cache_control {
            block.insert("cache_control".into(), cache_control.clone());
        }
        Value::Object(block)
    };

    if is_oauth {
        // OAuth tokens must carry the Claude Code identity.
        let mut system = vec![system_block(
            "You are Claude Code, Anthropic's official CLI for Claude.",
        )];
        if let Some(prompt) = &context.system_prompt {
            system.push(system_block(prompt));
        }
        params.insert("system".into(), Value::Array(system));
    } else if let Some(prompt) = &context.system_prompt {
        params.insert("system".into(), Value::Array(vec![system_block(prompt)]));
    }

    // Temperature is incompatible with extended thinking and unsupported on
    // Claude Opus 4.7+.
    if let Some(temperature) = options.temperature {
        if anthropic.thinking_enabled != Some(true) && compat.supports_temperature {
            params.insert(
                "temperature".into(),
                serde_json::Number::from_f64(temperature as f64)
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
            );
        }
    }

    if !immediate_tools.is_empty() || !deferred_tools.is_empty() {
        let mut tools = convert_tools(
            &immediate_tools,
            is_oauth,
            compat.supports_eager_tool_input_streaming,
            compat.supports_strict_tools,
            if compat.supports_cache_control_on_tools {
                cache_control
            } else {
                None
            },
            false,
        )?;
        tools.extend(convert_tools(
            &deferred_tools,
            is_oauth,
            compat.supports_eager_tool_input_streaming,
            compat.supports_strict_tools,
            None,
            true,
        )?);
        params.insert("tools".into(), Value::Array(tools));
    }

    // Thinking: adaptive, budget-based, or explicitly disabled.
    if model.reasoning {
        if anthropic.thinking_enabled == Some(true) {
            let display = anthropic
                .thinking_display
                .unwrap_or(AnthropicThinkingDisplay::Summarized);
            if compat.force_adaptive_thinking {
                params.insert(
                    "thinking".into(),
                    json!({ "type": "adaptive", "display": display.as_str() }),
                );
                if let Some(effort) = anthropic.effort {
                    params.insert("output_config".into(), json!({ "effort": effort.as_str() }));
                }
            } else {
                params.insert(
                    "thinking".into(),
                    json!({
                        "type": "enabled",
                        "budget_tokens": anthropic.thinking_budget_tokens.filter(|b| *b > 0).unwrap_or(1024),
                        "display": display.as_str(),
                    }),
                );
            }
        } else if anthropic.thinking_enabled == Some(false) && !thinking_off_unsupported(model) {
            params.insert("thinking".into(), json!({ "type": "disabled" }));
        }
    }

    if let Some(metadata) = &options.metadata {
        if let Some(Value::String(user_id)) = metadata.get("user_id") {
            params.insert("metadata".into(), json!({ "user_id": user_id }));
        }
    }

    if let Some(tool_choice) = &anthropic.tool_choice {
        params.insert("tool_choice".into(), tool_choice.to_wire());
    }

    let mut beta_features = Vec::new();
    if should_use_fine_grained_tool_streaming_beta(model, context) {
        beta_features.push(FINE_GRAINED_TOOL_STREAMING_BETA.to_string());
    }
    // Adaptive-thinking models have interleaved thinking built in.
    if anthropic.interleaved_thinking_enabled() && !compat.force_adaptive_thinking {
        beta_features.push(INTERLEAVED_THINKING_BETA.to_string());
    }

    Ok(BuiltRequest {
        body: Value::Object(params),
        beta_features,
    })
}

/// `model.thinkingLevelMap.off === null` marks a model that rejects
/// `thinking: { type: "disabled" }`.
fn thinking_off_unsupported(model: &Model) -> bool {
    model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(&pi_core::ModelThinkingLevel::Off))
        .is_some_and(|value| value.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::model::Api;

    fn model(id: &str, provider: &str) -> Model {
        Model::new(id, Api::AnthropicMessages, provider, "https://example.test")
    }

    #[test]
    fn tool_reference_defaults_follow_model_family() {
        assert!(default_supports_tool_references(&model(
            "claude-opus-4-5",
            "anthropic"
        )));
        assert!(default_supports_tool_references(&model(
            "claude-opus-4-6",
            "anthropic"
        )));
        assert!(default_supports_tool_references(&model(
            "claude-sonnet-5",
            "anthropic"
        )));
        assert!(default_supports_tool_references(&model(
            "claude-fable-5",
            "anthropic"
        )));
        assert!(!default_supports_tool_references(&model(
            "claude-haiku-4-5",
            "anthropic"
        )));
        assert!(!default_supports_tool_references(&model(
            "claude-sonnet-4-20250514",
            "anthropic"
        )));
        assert!(!default_supports_tool_references(&model(
            "claude-opus-4-1",
            "anthropic"
        )));
        assert!(!default_supports_tool_references(&model(
            "claude-opus-4-6",
            "anthropic-proxy"
        )));
    }

    #[test]
    fn normalizes_tool_call_ids() {
        assert_eq!(normalize_tool_call_id("fc_abc|123"), "fc_abc_123");
        assert_eq!(normalize_tool_call_id(&"x".repeat(100)).len(), 64);
    }

    #[test]
    fn claude_code_names_round_trip() {
        assert_eq!(to_claude_code_name("todowrite"), "TodoWrite");
        assert_eq!(to_claude_code_name("find"), "find");
        let tools = vec![Tool::no_params("todowrite", "")];
        assert_eq!(from_claude_code_name("TodoWrite", &tools), "todowrite");
        assert_eq!(from_claude_code_name("Glob", &tools), "Glob");
    }
}
