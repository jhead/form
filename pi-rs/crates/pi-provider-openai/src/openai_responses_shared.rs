//! Port of `api/openai-responses-shared.ts`.
//!
//! Message conversion, tool conversion and the SSE→[`AssistantMessageEvent`]
//! mapping shared by `openai-responses`, `azure-openai-responses` and
//! `openai-codex-responses`.

use std::collections::{HashMap, HashSet};

use pi_core::content::{AssistantContent, TextContent, TextPhase, ThinkingContent};
use pi_core::event::AssistantMessageEventSink;
use pi_core::message::{Message, UserContent};
use pi_core::{
    AiError, AssistantMessage, AssistantMessageEvent, Context, InputContent, Model, StopReason,
    Tool, ToolCall, Usage,
};
use pi_http::json_parse::parse_streaming_json_object;
use serde_json::{json, Map, Value};

use crate::compat::DeferredToolsMode;
use crate::util::{calculate_cost, sanitize_surrogates, short_hash};
use pi_provider_common::constrained_sampling::{
    append_grammar_tool_input_json_delta, grammar_tool_input, json_schema_tool_parameters,
    resolve_grammar_constrained_sampling, resolve_json_schema_strict_sampling,
    GrammarToolInputJsonBuffer, GrammarToolInputProperties,
};
use pi_provider_common::transform_messages::transform_messages;

// ============================================================================
// Text signatures
// ============================================================================

fn encode_text_signature_v1(id: &str, phase: Option<TextPhase>) -> String {
    let mut payload = Map::new();
    payload.insert("v".into(), json!(1));
    payload.insert("id".into(), json!(id));
    if let Some(phase) = phase {
        payload.insert(
            "phase".into(),
            json!(match phase {
                TextPhase::Commentary => "commentary",
                TextPhase::FinalAnswer => "final_answer",
            }),
        );
    }
    Value::Object(payload).to_string()
}

struct ParsedTextSignature {
    id: String,
    phase: Option<TextPhase>,
}

fn parse_text_signature(signature: Option<&str>) -> Option<ParsedTextSignature> {
    let signature = signature?;
    if signature.starts_with('{') {
        if let Ok(parsed) = serde_json::from_str::<Value>(signature) {
            if parsed.get("v").and_then(Value::as_u64) == Some(1) {
                if let Some(id) = parsed.get("id").and_then(Value::as_str) {
                    let phase = match parsed.get("phase").and_then(Value::as_str) {
                        Some("commentary") => Some(TextPhase::Commentary),
                        Some("final_answer") => Some(TextPhase::FinalAnswer),
                        _ => None,
                    };
                    return Some(ParsedTextSignature {
                        id: id.to_string(),
                        phase,
                    });
                }
            }
        }
        // Fall through to legacy plain-string handling.
    }
    Some(ParsedTextSignature {
        id: signature.to_string(),
        phase: None,
    })
}

// ============================================================================
// Message conversion
// ============================================================================

#[derive(Debug, Clone, Default)]
pub struct ConvertResponsesToolsOptions {
    /// Default `strict` when no per-tool constrained sampling applies.
    /// `None` mirrors upstream's `strict: null` (Codex), which serializes.
    pub strict: Option<bool>,
    pub strict_is_explicit_null: bool,
    pub supports_strict_mode: bool,
    pub supports_openai_grammar_tools: bool,
    pub defer_loading: bool,
}

impl ConvertResponsesToolsOptions {
    pub fn new(supports_strict_mode: bool, supports_openai_grammar_tools: bool) -> Self {
        Self {
            strict: None,
            strict_is_explicit_null: false,
            supports_strict_mode,
            supports_openai_grammar_tools,
            defer_loading: false,
        }
    }

    /// Codex passes `strict: null`, which is neither `true` nor the `false`
    /// default — it disables the field's default while still emitting it.
    pub fn with_explicit_null_strict(mut self) -> Self {
        self.strict = None;
        self.strict_is_explicit_null = true;
        self
    }
}

#[derive(Default)]
pub struct ConvertResponsesMessagesOptions<'a> {
    pub include_system_prompt: bool,
    pub grammar_tool_input_properties: Option<&'a GrammarToolInputProperties>,
    pub deferred_tools: Option<&'a HashMap<String, Tool>>,
    pub deferred_tools_mode: Option<DeferredToolsMode>,
    pub tool_options: ConvertResponsesToolsOptions,
}

impl ConvertResponsesMessagesOptions<'_> {
    pub fn defaults() -> Self {
        Self {
            include_system_prompt: true,
            ..Default::default()
        }
    }
}

fn normalize_id_part(part: &str) -> String {
    let sanitized: String = part
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let normalized: String = if sanitized.chars().count() > 64 {
        sanitized.chars().take(64).collect()
    } else {
        sanitized
    };
    normalized.trim_end_matches('_').to_string()
}

fn build_foreign_responses_item_id(item_id: &str) -> String {
    let normalized = format!("fc_{}", short_hash(item_id));
    if normalized.chars().count() > 64 {
        normalized.chars().take(64).collect()
    } else {
        normalized
    }
}

fn tool_result_output(model: &Model, content: &[InputContent]) -> Value {
    let text_result = content
        .iter()
        .filter_map(|b| b.as_text().map(|t| t.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    let images: Vec<&pi_core::ImageContent> = content
        .iter()
        .filter_map(|b| match b {
            InputContent::Image(image) => Some(image),
            InputContent::Text(_) => None,
        })
        .collect();
    let has_text = !text_result.is_empty();

    if images.is_empty() || !model.supports_images() {
        let text = if has_text {
            text_result
        } else if !images.is_empty() {
            "(see attached image)".to_string()
        } else {
            "(no tool output)".to_string()
        };
        return json!(sanitize_surrogates(&text));
    }

    let mut output = Vec::new();
    if has_text {
        output.push(json!({ "type": "input_text", "text": sanitize_surrogates(&text_result) }));
    }
    for image in images {
        output.push(json!({
            "type": "input_image",
            "detail": "auto",
            "image_url": format!("data:{};base64,{}", image.mime_type, image.data)
        }));
    }
    Value::Array(output)
}

/// Port of `convertResponsesMessages`.
pub fn convert_responses_messages(
    model: &Model,
    context: &Context,
    allowed_tool_call_providers: &HashSet<&str>,
    options: &ConvertResponsesMessagesOptions<'_>,
) -> Result<Vec<Value>, AiError> {
    let mut messages: Vec<Value> = Vec::new();
    let mut loaded_tool_names: HashSet<String> = HashSet::new();

    let normalize_tool_call_id = |id: &str, _target: &Model, source: &AssistantMessage| -> String {
        if !allowed_tool_call_providers.contains(model.provider.as_str()) {
            return normalize_id_part(id);
        }
        let Some(separator) = id.find('|') else {
            return normalize_id_part(id);
        };
        let call_id = normalize_id_part(&id[..separator]);
        let item_id_raw = &id[separator + 1..];
        // A tool call minted by another provider/api cannot keep its item id:
        // OpenAI validates that fc_* ids were paired with an rs_* reasoning item.
        let is_foreign = source.provider != model.provider || source.api != model.api.as_str();
        let mut item_id = if is_foreign {
            build_foreign_responses_item_id(item_id_raw)
        } else {
            normalize_id_part(item_id_raw)
        };
        if !item_id.starts_with("fc_") {
            item_id = normalize_id_part(&format!("fc_{item_id}"));
        }
        format!("{call_id}|{item_id}")
    };

    let transformed = transform_messages(&context.messages, model, Some(&normalize_tool_call_id));

    if options.include_system_prompt {
        if let Some(system_prompt) = &context.system_prompt {
            let supports_developer_role = model
                .compat
                .as_ref()
                .and_then(|c| c.supports_developer_role)
                .unwrap_or(true);
            let role = if model.reasoning && supports_developer_role {
                "developer"
            } else {
                "system"
            };
            messages.push(json!({ "role": role, "content": sanitize_surrogates(system_prompt) }));
        }
    }

    let empty_props = GrammarToolInputProperties::new();
    let grammar_props = options
        .grammar_tool_input_properties
        .unwrap_or(&empty_props);

    let mut msg_index = 0usize;
    for msg in &transformed {
        match msg {
            Message::User(user) => match &user.content {
                UserContent::Text(text) => messages.push(json!({
                    "role": "user",
                    "content": [{ "type": "input_text", "text": sanitize_surrogates(text) }]
                })),
                UserContent::Blocks(blocks) => {
                    if blocks.is_empty() {
                        msg_index += 1;
                        continue;
                    }
                    let content: Vec<Value> = blocks
                        .iter()
                        .map(|block| match block {
                            InputContent::Text(text) => {
                                json!({ "type": "input_text", "text": sanitize_surrogates(&text.text) })
                            }
                            InputContent::Image(image) => json!({
                                "type": "input_image",
                                "detail": "auto",
                                "image_url": format!("data:{};base64,{}", image.mime_type, image.data)
                            }),
                        })
                        .collect();
                    messages.push(json!({ "role": "user", "content": content }));
                }
            },
            Message::Assistant(assistant) => {
                let mut output: Vec<Value> = Vec::new();
                let same_provider_and_api =
                    assistant.provider == model.provider && assistant.api == model.api.as_str();
                let is_same_model = same_provider_and_api && assistant.model == model.id;
                let is_different_model = same_provider_and_api && assistant.model != model.id;
                let mut text_block_index = 0usize;

                for block in &assistant.content {
                    match block {
                        AssistantContent::Thinking(thinking) => {
                            if let Some(signature) = &thinking.thinking_signature {
                                if let Ok(item) = serde_json::from_str::<Value>(signature) {
                                    output.push(item);
                                }
                            }
                        }
                        AssistantContent::Text(text) => {
                            let parsed = parse_text_signature(text.text_signature.as_deref());
                            let fallback = if text_block_index == 0 {
                                format!("msg_pi_{msg_index}")
                            } else {
                                format!("msg_pi_{msg_index}_{text_block_index}")
                            };
                            text_block_index += 1;
                            // OpenAI caps ids at 64 characters.
                            let mut msg_id =
                                parsed.as_ref().map(|p| p.id.clone()).unwrap_or(fallback);
                            if msg_id.is_empty() {
                                msg_id = format!("msg_pi_{msg_index}");
                            } else if msg_id.chars().count() > 64 {
                                msg_id = format!("msg_{}", short_hash(&msg_id));
                            }
                            let mut item = Map::new();
                            item.insert("type".into(), json!("message"));
                            item.insert("role".into(), json!("assistant"));
                            item.insert(
                                "content".into(),
                                json!([{
                                    "type": "output_text",
                                    "text": sanitize_surrogates(&text.text),
                                    "annotations": []
                                }]),
                            );
                            item.insert("status".into(), json!("completed"));
                            item.insert("id".into(), json!(msg_id));
                            if let Some(phase) = parsed.and_then(|p| p.phase) {
                                item.insert(
                                    "phase".into(),
                                    json!(match phase {
                                        TextPhase::Commentary => "commentary",
                                        TextPhase::FinalAnswer => "final_answer",
                                    }),
                                );
                            }
                            output.push(Value::Object(item));
                        }
                        AssistantContent::ToolCall(tool_call) => {
                            let (call_id, item_id_raw) = match tool_call.id.split_once('|') {
                                Some((call_id, item_id)) => (call_id, Some(item_id)),
                                None => (tool_call.id.as_str(), None),
                            };
                            let custom_property = grammar_props.get(&tool_call.name);
                            let mut item_id = item_id_raw.map(str::to_string);

                            // Same provider, different model: drop the id so OpenAI does not
                            // run fc_*/rs_* pairing validation. Also drop non-fc_* ids (ctc_*
                            // custom-tool ids) when replaying as a function_call.
                            let starts_with_fc =
                                item_id.as_deref().is_some_and(|id| id.starts_with("fc_"));
                            if (is_different_model && starts_with_fc)
                                || (custom_property.is_none() && !starts_with_fc)
                            {
                                item_id = None;
                            }

                            let can_replay_namespace = is_same_model
                                || options
                                    .deferred_tools
                                    .is_some_and(|d| d.contains_key(&tool_call.name));

                            let mut item = Map::new();
                            match custom_property {
                                Some(property) => {
                                    item.insert("type".into(), json!("custom_tool_call"));
                                    item.insert(
                                        "id".into(),
                                        item_id.map(Value::from).unwrap_or(Value::Null),
                                    );
                                    item.insert("call_id".into(), json!(call_id));
                                    item.insert("name".into(), json!(tool_call.name));
                                    item.insert(
                                        "input".into(),
                                        json!(sanitize_surrogates(&grammar_tool_input(
                                            &tool_call.name,
                                            &tool_call.arguments,
                                            property
                                        )?)),
                                    );
                                }
                                None => {
                                    item.insert("type".into(), json!("function_call"));
                                    item.insert(
                                        "id".into(),
                                        item_id.map(Value::from).unwrap_or(Value::Null),
                                    );
                                    item.insert("call_id".into(), json!(call_id));
                                    item.insert("name".into(), json!(tool_call.name));
                                    item.insert(
                                        "arguments".into(),
                                        json!(serde_json::to_string(&tool_call.arguments)
                                            .unwrap_or_else(|_| "{}".into())),
                                    );
                                }
                            }
                            if can_replay_namespace {
                                if let Some(namespace) = &tool_call.namespace {
                                    item.insert("namespace".into(), json!(namespace));
                                }
                            }
                            output.push(Value::Object(item));
                        }
                    }
                }
                if output.is_empty() {
                    msg_index += 1;
                    continue;
                }
                messages.extend(output);
            }
            Message::ToolResult(result) => {
                let call_id = result
                    .tool_call_id
                    .split_once('|')
                    .map(|(id, _)| id)
                    .unwrap_or(&result.tool_call_id);
                let output = tool_result_output(model, &result.content);

                if grammar_props.contains_key(&result.tool_name) {
                    messages.push(json!({
                        "type": "custom_tool_call_output",
                        "call_id": call_id,
                        "output": output
                    }));
                } else {
                    messages.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": output
                    }));
                }

                let mut deferred_tools: Vec<Tool> = Vec::new();
                for name in result.added_tool_names.clone().unwrap_or_default() {
                    let Some(tool) = options.deferred_tools.and_then(|d| d.get(&name)) else {
                        continue;
                    };
                    if loaded_tool_names.contains(&name) {
                        continue;
                    }
                    loaded_tool_names.insert(name);
                    deferred_tools.push(tool.clone());
                }
                if !deferred_tools.is_empty() {
                    match options.deferred_tools_mode {
                        Some(DeferredToolsMode::AdditionalTools) => {
                            messages.push(json!({
                                "type": "additional_tools",
                                "role": "developer",
                                "tools": convert_responses_tools(&deferred_tools, &options.tool_options)?
                            }));
                        }
                        Some(DeferredToolsMode::ToolSearch) => {
                            let names: Vec<String> =
                                deferred_tools.iter().map(|t| t.name.clone()).collect();
                            let search_call_id = format!(
                                "pi_tool_load_{}",
                                short_hash(&format!("{}:{}", result.tool_call_id, names.join(",")))
                            );
                            messages.push(json!({
                                "type": "tool_search_call",
                                "call_id": search_call_id,
                                "execution": "client",
                                "status": "completed",
                                "arguments": { "query": names.join(" "), "limit": names.len() }
                            }));
                            let mut tool_options = options.tool_options.clone();
                            tool_options.defer_loading = true;
                            messages.push(json!({
                                "type": "tool_search_output",
                                "call_id": search_call_id,
                                "execution": "client",
                                "status": "completed",
                                "tools": convert_responses_tools(&deferred_tools, &tool_options)?
                            }));
                        }
                        None => {}
                    }
                }
            }
        }
        msg_index += 1;
    }

    Ok(messages)
}

// ============================================================================
// Tool conversion
// ============================================================================

/// Port of `convertResponsesTools`.
pub fn convert_responses_tools(
    tools: &[Tool],
    options: &ConvertResponsesToolsOptions,
) -> Result<Vec<Value>, AiError> {
    // `strict === undefined ? false : strict` — an explicit null stays null.
    let default_strict: Option<bool> = if options.strict_is_explicit_null {
        None
    } else {
        Some(options.strict.unwrap_or(false))
    };

    let mut out = Vec::with_capacity(tools.len());
    for tool in tools {
        if let Some(grammar) =
            resolve_grammar_constrained_sampling(tool, options.supports_openai_grammar_tools)?
        {
            let mut entry = Map::new();
            entry.insert("type".into(), json!("custom"));
            entry.insert("name".into(), json!(tool.name));
            entry.insert("description".into(), json!(tool.description));
            entry.insert(
                "format".into(),
                json!({
                    "type": "grammar",
                    "syntax": grammar.syntax(),
                    "definition": grammar.definition
                }),
            );
            if options.defer_loading {
                entry.insert("defer_loading".into(), json!(true));
            }
            out.push(Value::Object(entry));
            continue;
        }

        let constrained = resolve_json_schema_strict_sampling(tool, options.supports_strict_mode)?;
        let strict = constrained.or(default_strict);
        let mut entry = Map::new();
        entry.insert("type".into(), json!("function"));
        entry.insert("name".into(), json!(tool.name));
        entry.insert("description".into(), json!(tool.description));
        entry.insert(
            "parameters".into(),
            json_schema_tool_parameters(tool, Some(strict == Some(true))),
        );
        if options.defer_loading {
            entry.insert("defer_loading".into(), json!(true));
        }
        if options.supports_strict_mode {
            entry.insert(
                "strict".into(),
                strict.map(Value::Bool).unwrap_or(Value::Null),
            );
        }
        out.push(Value::Object(entry));
    }
    Ok(out)
}

// ============================================================================
// Stream processing
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotKind {
    Thinking,
    Text,
    ToolCall,
}

#[derive(Debug)]
struct Slot {
    kind: SlotKind,
    content_index: usize,
    partial_json: Option<String>,
    custom_input: Option<(String, GrammarToolInputJsonBuffer)>,
}

/// Knobs the three Responses adapters differ on.
#[derive(Debug, Clone, Default)]
pub struct ResponsesStreamOptions {
    pub service_tier: Option<String>,
    pub grammar_tool_input_properties: GrammarToolInputProperties,
    /// Codex: a response reporting `default` while `flex`/`priority` was
    /// requested still bills at the requested tier.
    pub codex_service_tier_resolution: bool,
    /// Whether to scale the computed cost by the service-tier multiplier.
    pub apply_service_tier_pricing: bool,
}

/// Incremental port of `processResponsesStream`.
///
/// Upstream consumes an async iterator; here the caller pumps events in so the
/// same machinery serves the plain SSE path and Codex's remapped event stream.
pub struct ResponsesStreamProcessor {
    options: ResponsesStreamOptions,
    slots: HashMap<i64, Slot>,
    reasoning_blocks_by_id: HashMap<String, usize>,
    saw_terminal_response_event: bool,
}

impl ResponsesStreamProcessor {
    pub fn new(options: ResponsesStreamOptions) -> Self {
        Self {
            options,
            slots: HashMap::new(),
            reasoning_blocks_by_id: HashMap::new(),
            saw_terminal_response_event: false,
        }
    }

    pub fn saw_terminal_response_event(&self) -> bool {
        self.saw_terminal_response_event
    }

    /// Feed one decoded Responses event.
    pub async fn handle_event(
        &mut self,
        event: &Value,
        output: &mut AssistantMessage,
        model: &Model,
        sink: &AssistantMessageEventSink,
    ) -> Result<(), AiError> {
        let Some(event_type) = event.get("type").and_then(Value::as_str) else {
            return Ok(());
        };
        let output_index = event
            .get("output_index")
            .and_then(Value::as_i64)
            .unwrap_or(0);

        match event_type {
            "response.created" => {
                if let Some(id) = event.pointer("/response/id").and_then(Value::as_str) {
                    output.response_id = Some(id.to_string());
                }
            }
            "response.output_item.added" => {
                if let Some(item) = event.get("item") {
                    self.create_slot(output_index, item, output, sink).await;
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let delta = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.push_thinking_delta(output_index, delta, output, sink)
                    .await;
            }
            "response.reasoning_summary_part.done" => {
                self.push_thinking_delta(output_index, "\n\n", output, sink)
                    .await;
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                let Some(slot) = self.slot(output_index, SlotKind::Text) else {
                    return Ok(());
                };
                let content_index = slot.content_index;
                let delta = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if let AssistantContent::Text(text) = &mut output.content[content_index] {
                    text.text.push_str(&delta);
                }
                let _ = sink
                    .send(AssistantMessageEvent::TextDelta {
                        content_index,
                        delta,
                        partial: output.clone(),
                    })
                    .await;
            }
            "response.function_call_arguments.delta" => {
                let Some(slot) = self.slots.get_mut(&output_index) else {
                    return Ok(());
                };
                if slot.kind != SlotKind::ToolCall || slot.partial_json.is_none() {
                    return Ok(());
                }
                let delta = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let content_index = slot.content_index;
                let partial = slot.partial_json.get_or_insert_with(String::new);
                partial.push_str(&delta);
                let parsed = parse_streaming_json_object(Some(partial.as_str()));
                if let AssistantContent::ToolCall(block) = &mut output.content[content_index] {
                    block.arguments = parsed;
                }
                let _ = sink
                    .send(AssistantMessageEvent::ToolCallDelta {
                        content_index,
                        delta,
                        partial: output.clone(),
                    })
                    .await;
            }
            "response.function_call_arguments.done" => {
                let Some(slot) = self.slots.get_mut(&output_index) else {
                    return Ok(());
                };
                if slot.kind != SlotKind::ToolCall {
                    return Ok(());
                }
                let Some(previous) = slot.partial_json.clone() else {
                    return Ok(());
                };
                let arguments = event
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let content_index = slot.content_index;
                slot.partial_json = Some(arguments.clone());
                let parsed = parse_streaming_json_object(Some(arguments.as_str()));
                if let AssistantContent::ToolCall(block) = &mut output.content[content_index] {
                    block.arguments = parsed;
                }
                if let Some(delta) = arguments.strip_prefix(previous.as_str()) {
                    if !delta.is_empty() {
                        let _ = sink
                            .send(AssistantMessageEvent::ToolCallDelta {
                                content_index,
                                delta: delta.to_string(),
                                partial: output.clone(),
                            })
                            .await;
                    }
                }
            }
            "response.custom_tool_call_input.delta" => {
                let Some(current) = self.custom_input(output_index, output) else {
                    return Ok(());
                };
                let delta = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let next = format!("{current}{delta}");
                self.push_custom_tool_delta(output_index, &next, false, output, sink)
                    .await?;
            }
            "response.custom_tool_call_input.done" => {
                if self.custom_input(output_index, output).is_none() {
                    return Ok(());
                }
                let input = event
                    .get("input")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.push_custom_tool_delta(output_index, &input, true, output, sink)
                    .await?;
            }
            "response.output_item.done" => {
                let Some(item) = event.get("item") else {
                    return Ok(());
                };
                self.handle_item_done(output_index, item, output, sink)
                    .await?;
            }
            "response.completed" | "response.incomplete" => {
                let response = event.get("response").cloned().unwrap_or(Value::Null);
                self.finalize_response(&response, output, model);
            }
            "error" => {
                let code = event
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let message = event
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown error");
                return Err(AiError::provider(
                    0,
                    format!("Error Code {code}: {message}"),
                ));
            }
            "response.failed" => {
                self.saw_terminal_response_event = true;
                output.raw_stop_reason = event
                    .pointer("/response/status")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let error = event.pointer("/response/error");
                let reason = event
                    .pointer("/response/incomplete_details/reason")
                    .and_then(Value::as_str);
                let message = match (error, reason) {
                    (Some(error), _) if !error.is_null() => format!(
                        "{}: {}",
                        error
                            .get("code")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown"),
                        error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("no message")
                    ),
                    (_, Some(reason)) => format!("incomplete: {reason}"),
                    _ => "Unknown error (no error details in response)".to_string(),
                };
                return Err(AiError::other(message));
            }
            _ => {}
        }
        Ok(())
    }

    fn slot(&self, output_index: i64, kind: SlotKind) -> Option<&Slot> {
        self.slots.get(&output_index).filter(|s| s.kind == kind)
    }

    async fn push_thinking_delta(
        &mut self,
        output_index: i64,
        delta: &str,
        output: &mut AssistantMessage,
        sink: &AssistantMessageEventSink,
    ) {
        let Some(slot) = self.slot(output_index, SlotKind::Thinking) else {
            return;
        };
        let content_index = slot.content_index;
        if let AssistantContent::Thinking(thinking) = &mut output.content[content_index] {
            thinking.thinking.push_str(delta);
        }
        let _ = sink
            .send(AssistantMessageEvent::ThinkingDelta {
                content_index,
                delta: delta.to_string(),
                partial: output.clone(),
            })
            .await;
    }

    fn custom_input(&self, output_index: i64, output: &AssistantMessage) -> Option<String> {
        let slot = self.slots.get(&output_index)?;
        if slot.kind != SlotKind::ToolCall {
            return None;
        }
        let (property, _) = slot.custom_input.as_ref()?;
        match &output.content[slot.content_index] {
            AssistantContent::ToolCall(block) => Some(
                block
                    .arguments
                    .get(property)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            ),
            _ => Some(String::new()),
        }
    }

    async fn push_custom_tool_delta(
        &mut self,
        output_index: i64,
        next_input: &str,
        close: bool,
        output: &mut AssistantMessage,
        sink: &AssistantMessageEventSink,
    ) -> Result<(), AiError> {
        let Some(slot) = self.slots.get_mut(&output_index) else {
            return Ok(());
        };
        let Some((property, buffer)) = &mut slot.custom_input else {
            return Ok(());
        };
        let property = property.clone();
        let content_index = slot.content_index;
        let delta = append_grammar_tool_input_json_delta(buffer, &property, next_input, close)?;
        if let AssistantContent::ToolCall(block) = &mut output.content[content_index] {
            let mut arguments = Map::new();
            arguments.insert(property, json!(next_input));
            block.arguments = arguments;
        }
        if let Some(delta) = delta {
            let _ = sink
                .send(AssistantMessageEvent::ToolCallDelta {
                    content_index,
                    delta,
                    partial: output.clone(),
                })
                .await;
        }
        Ok(())
    }

    fn apply_message_phase_stop_reason(item: &Value, output: &mut AssistantMessage) {
        if item.get("type").and_then(Value::as_str) == Some("message")
            && item.get("phase").and_then(Value::as_str) == Some("final_answer")
        {
            output.stop_reason = StopReason::Stop;
        }
    }

    async fn create_slot(
        &mut self,
        output_index: i64,
        item: &Value,
        output: &mut AssistantMessage,
        sink: &AssistantMessageEventSink,
    ) -> Option<usize> {
        let item_type = item.get("type").and_then(Value::as_str)?;
        match item_type {
            "reasoning" => {
                output
                    .content
                    .push(AssistantContent::Thinking(ThinkingContent::default()));
                let content_index = output.content.len() - 1;
                self.slots.insert(
                    output_index,
                    Slot {
                        kind: SlotKind::Thinking,
                        content_index,
                        partial_json: None,
                        custom_input: None,
                    },
                );
                let _ = sink
                    .send(AssistantMessageEvent::ThinkingStart {
                        content_index,
                        partial: output.clone(),
                    })
                    .await;
                Some(content_index)
            }
            "message" => {
                Self::apply_message_phase_stop_reason(item, output);
                output
                    .content
                    .push(AssistantContent::Text(TextContent::default()));
                let content_index = output.content.len() - 1;
                self.slots.insert(
                    output_index,
                    Slot {
                        kind: SlotKind::Text,
                        content_index,
                        partial_json: None,
                        custom_input: None,
                    },
                );
                let _ = sink
                    .send(AssistantMessageEvent::TextStart {
                        content_index,
                        partial: output.clone(),
                    })
                    .await;
                Some(content_index)
            }
            "function_call" => {
                let call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
                let block = ToolCall {
                    id: format!("{call_id}|{id}"),
                    name: item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    arguments: Map::new(),
                    thought_signature: None,
                    namespace: item
                        .get("namespace")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                };
                output.content.push(AssistantContent::ToolCall(block));
                let content_index = output.content.len() - 1;
                self.slots.insert(
                    output_index,
                    Slot {
                        kind: SlotKind::ToolCall,
                        content_index,
                        partial_json: Some(
                            item.get("arguments")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        ),
                        custom_input: None,
                    },
                );
                let _ = sink
                    .send(AssistantMessageEvent::ToolCallStart {
                        content_index,
                        partial: output.clone(),
                    })
                    .await;
                Some(content_index)
            }
            "custom_tool_call" => {
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let property = self
                    .options
                    .grammar_tool_input_properties
                    .get(&name)
                    .cloned()
                    .unwrap_or_else(|| "input".to_string());
                let input = item
                    .get("input")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
                let mut arguments = Map::new();
                arguments.insert(property.clone(), json!(input));
                output.content.push(AssistantContent::ToolCall(ToolCall {
                    id: format!("{call_id}|{id}"),
                    name,
                    arguments,
                    thought_signature: None,
                    namespace: item
                        .get("namespace")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                }));
                let content_index = output.content.len() - 1;
                self.slots.insert(
                    output_index,
                    Slot {
                        kind: SlotKind::ToolCall,
                        content_index,
                        partial_json: None,
                        custom_input: Some((property, GrammarToolInputJsonBuffer::default())),
                    },
                );
                let _ = sink
                    .send(AssistantMessageEvent::ToolCallStart {
                        content_index,
                        partial: output.clone(),
                    })
                    .await;
                Some(content_index)
            }
            _ => None,
        }
    }

    async fn handle_item_done(
        &mut self,
        output_index: i64,
        item: &Value,
        output: &mut AssistantMessage,
        sink: &AssistantMessageEventSink,
    ) -> Result<(), AiError> {
        Self::apply_message_phase_stop_reason(item, output);
        if !self.slots.contains_key(&output_index) {
            self.create_slot(output_index, item, output, sink).await;
        }
        let Some(slot) = self.slots.get(&output_index) else {
            return Ok(());
        };
        let kind = slot.kind;
        let content_index = slot.content_index;
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();

        match (item_type, kind) {
            ("reasoning", SlotKind::Thinking) => {
                let summary_text = join_text_parts(item.get("summary"));
                let content_text = join_text_parts(item.get("content"));
                let thinking = {
                    let AssistantContent::Thinking(block) = &mut output.content[content_index]
                    else {
                        return Ok(());
                    };
                    if !summary_text.is_empty() {
                        block.thinking = summary_text;
                    } else if !content_text.is_empty() {
                        block.thinking = content_text;
                    }
                    block.thinking_signature = Some(item.to_string());
                    block.thinking.clone()
                };
                if let Some(id) = item.get("id").and_then(Value::as_str) {
                    self.reasoning_blocks_by_id
                        .insert(id.to_string(), content_index);
                }
                let _ = sink
                    .send(AssistantMessageEvent::ThinkingEnd {
                        content_index,
                        content: thinking,
                        partial: output.clone(),
                    })
                    .await;
                self.slots.remove(&output_index);
            }
            ("message", SlotKind::Text) => {
                let text = item
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|parts| {
                        parts
                            .iter()
                            .map(|part| {
                                if part.get("type").and_then(Value::as_str) == Some("output_text") {
                                    part.get("text").and_then(Value::as_str).unwrap_or_default()
                                } else {
                                    part.get("refusal")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default();
                let phase = match item.get("phase").and_then(Value::as_str) {
                    Some("commentary") => Some(TextPhase::Commentary),
                    Some("final_answer") => Some(TextPhase::FinalAnswer),
                    _ => None,
                };
                let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
                if let AssistantContent::Text(block) = &mut output.content[content_index] {
                    block.text = text.clone();
                    block.text_signature = Some(encode_text_signature_v1(id, phase));
                }
                let _ = sink
                    .send(AssistantMessageEvent::TextEnd {
                        content_index,
                        content: text,
                        partial: output.clone(),
                    })
                    .await;
                self.slots.remove(&output_index);
            }
            ("function_call", SlotKind::ToolCall) => {
                let Some(slot) = self.slots.get(&output_index) else {
                    return Ok(());
                };
                let Some(previous) = slot.partial_json.clone() else {
                    return Ok(());
                };
                let arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .filter(|a| !a.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        if previous.is_empty() {
                            "{}".to_string()
                        } else {
                            previous
                        }
                    });
                let parsed = parse_streaming_json_object(Some(arguments.as_str()));
                let namespace = item
                    .get("namespace")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let tool_call = {
                    let AssistantContent::ToolCall(block) = &mut output.content[content_index]
                    else {
                        return Ok(());
                    };
                    block.arguments = parsed;
                    if namespace.is_some() {
                        block.namespace = namespace;
                    }
                    block.clone()
                };
                // Drop the scratch buffer so replay only carries parsed arguments.
                if let Some(slot) = self.slots.get_mut(&output_index) {
                    slot.partial_json = None;
                }
                let _ = sink
                    .send(AssistantMessageEvent::ToolCallEnd {
                        content_index,
                        tool_call,
                        partial: output.clone(),
                    })
                    .await;
                self.slots.remove(&output_index);
            }
            ("custom_tool_call", SlotKind::ToolCall) => {
                if self
                    .slots
                    .get(&output_index)
                    .is_none_or(|s| s.custom_input.is_none())
                {
                    return Ok(());
                }
                let input = item
                    .get("input")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| self.custom_input(output_index, output).unwrap_or_default());
                self.push_custom_tool_delta(output_index, &input, true, output, sink)
                    .await?;
                let namespace = item
                    .get("namespace")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let tool_call = {
                    let AssistantContent::ToolCall(block) = &mut output.content[content_index]
                    else {
                        return Ok(());
                    };
                    if namespace.is_some() {
                        block.namespace = namespace;
                    }
                    block.clone()
                };
                if let Some(slot) = self.slots.get_mut(&output_index) {
                    slot.custom_input = None;
                }
                let _ = sink
                    .send(AssistantMessageEvent::ToolCallEnd {
                        content_index,
                        tool_call,
                        partial: output.clone(),
                    })
                    .await;
                self.slots.remove(&output_index);
            }
            _ => {}
        }
        Ok(())
    }

    /// Azure can omit `reasoning.encrypted_content` from `output_item.done` and
    /// only include it in the terminal response. Backfill it so `store:false`
    /// multi-turn replay stays stateless.
    fn backfill_reasoning_signatures(
        &self,
        response_output: &Value,
        output: &mut AssistantMessage,
    ) {
        let Some(items) = response_output.as_array() else {
            return;
        };
        for item in items {
            if item.get("type").and_then(Value::as_str) != Some("reasoning") {
                continue;
            }
            let Some(encrypted) = item.get("encrypted_content").filter(|v| !v.is_null()) else {
                continue;
            };
            let Some(id) = item.get("id").and_then(Value::as_str) else {
                continue;
            };
            let Some(&content_index) = self.reasoning_blocks_by_id.get(id) else {
                continue;
            };
            let AssistantContent::Thinking(block) = &mut output.content[content_index] else {
                continue;
            };
            let Some(signature) = block.thinking_signature.clone() else {
                continue;
            };
            let Ok(mut stored) = serde_json::from_str::<Value>(&signature) else {
                continue;
            };
            if stored
                .get("encrypted_content")
                .is_some_and(|v| !v.is_null())
            {
                continue;
            }
            if let Some(obj) = stored.as_object_mut() {
                obj.insert("encrypted_content".into(), encrypted.clone());
            }
            block.thinking_signature = Some(stored.to_string());
        }
    }

    fn finalize_response(
        &mut self,
        response: &Value,
        output: &mut AssistantMessage,
        model: &Model,
    ) {
        self.saw_terminal_response_event = true;
        self.backfill_reasoning_signatures(response.get("output").unwrap_or(&Value::Null), output);
        if let Some(id) = response.get("id").and_then(Value::as_str) {
            output.response_id = Some(id.to_string());
        }
        if let Some(usage) = response.get("usage").filter(|u| !u.is_null()) {
            // Token counts are signed: corrective usage records carry negative deltas.
            let num = |v: Option<&Value>| v.and_then(Value::as_i64).unwrap_or(0);
            let details = usage.get("input_tokens_details");
            let cached = num(details.and_then(|d| d.get("cached_tokens")));
            let cache_write = num(details.and_then(|d| d.get("cache_write_tokens")));
            let input_tokens = num(usage.get("input_tokens"));
            output.usage = Usage {
                // OpenAI counts cached and cache-write tokens inside input_tokens.
                // Upstream clamps the remainder at zero explicitly.
                input: (input_tokens - cached - cache_write).max(0),
                output: num(usage.get("output_tokens")),
                cache_read: cached,
                cache_write,
                cache_write_1h: None,
                reasoning: Some(num(usage
                    .get("output_tokens_details")
                    .and_then(|d| d.get("reasoning_tokens")))),
                total_tokens: num(usage.get("total_tokens")),
                cost: Default::default(),
            };
        }
        calculate_cost(model, &mut output.usage);

        if self.options.apply_service_tier_pricing {
            let response_tier = response.get("service_tier").and_then(Value::as_str);
            let requested = self.options.service_tier.as_deref();
            let tier = if self.options.codex_service_tier_resolution {
                resolve_codex_service_tier(response_tier, requested)
            } else {
                response_tier.or(requested)
            };
            apply_service_tier_pricing(&mut output.usage, tier, &model.id);
        }

        // Keep the provider's specific incomplete reason so max-output truncation
        // and content filtering stay distinguishable.
        let status = response.get("status").and_then(Value::as_str);
        let incomplete_reason = response
            .get("incomplete_details")
            .filter(|v| !v.is_null())
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str);
        output.raw_stop_reason = match (status, incomplete_reason) {
            (Some(status), Some(reason)) => Some(format!("{status}.{reason}")),
            (Some(status), None) => Some(status.to_string()),
            (None, _) => None,
        };
        let (stop_reason, error_message) = map_responses_stop_reason(status, incomplete_reason);
        output.stop_reason = stop_reason;
        output.error_message = error_message;
        if output.tool_calls().next().is_some() && output.stop_reason == StopReason::Stop {
            output.stop_reason = StopReason::ToolUse;
        }
    }
}

fn join_text_parts(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .map(|p| p.get("text").and_then(Value::as_str).unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default()
}

/// Port of `mapStopReason` in `openai-responses-shared.ts`.
pub fn map_responses_stop_reason(
    status: Option<&str>,
    incomplete_reason: Option<&str>,
) -> (StopReason, Option<String>) {
    let Some(status) = status else {
        return (StopReason::Stop, None);
    };
    match status {
        "completed" => (StopReason::Stop, None),
        "incomplete" => {
            if incomplete_reason == Some("max_output_tokens") {
                (StopReason::Length, None)
            } else {
                (
                    StopReason::Error,
                    Some(match incomplete_reason {
                        Some(reason) => format!("Response incomplete: {reason}"),
                        None => "Response incomplete without a provider reason".to_string(),
                    }),
                )
            }
        }
        "failed" | "cancelled" => (StopReason::Error, None),
        // These two are wonky, but upstream treats them as a clean stop.
        "in_progress" | "queued" => (StopReason::Stop, None),
        other => (
            StopReason::Error,
            Some(format!("Unhandled stop reason: {other}")),
        ),
    }
}

/// Port of `resolveCodexServiceTier`.
pub fn resolve_codex_service_tier<'a>(
    response_tier: Option<&'a str>,
    request_tier: Option<&'a str>,
) -> Option<&'a str> {
    if response_tier == Some("default") && matches!(request_tier, Some("flex") | Some("priority")) {
        return request_tier;
    }
    response_tier.or(request_tier)
}

fn service_tier_cost_multiplier(model_id: &str, service_tier: Option<&str>) -> f64 {
    match service_tier {
        Some("flex") => 0.5,
        Some("priority") => {
            if model_id == "gpt-5.5" {
                2.5
            } else {
                2.0
            }
        }
        _ => 1.0,
    }
}

/// Port of `applyServiceTierPricing`, shared verbatim by responses and codex.
pub fn apply_service_tier_pricing(usage: &mut Usage, service_tier: Option<&str>, model_id: &str) {
    let multiplier = service_tier_cost_multiplier(model_id, service_tier);
    if multiplier == 1.0 {
        return;
    }
    usage.cost.input *= multiplier;
    usage.cost.output *= multiplier;
    usage.cost.cache_read *= multiplier;
    usage.cost.cache_write *= multiplier;
    usage.cost.total =
        usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
}

/// What an adapter's event pre-processor decided to do with a raw SSE payload.
pub enum MappedEvent {
    /// Drop it.
    Skip,
    /// Feed it to the processor.
    Emit(Value),
    /// Feed it to the processor, then stop reading the stream.
    EmitAndStop(Value),
}

/// An adapter's SSE pre-processor. Codex is the only implementor.
pub type CodexEventMapper =
    dyn Fn(Value, &mut AssistantMessage) -> Result<MappedEvent, AiError> + Send + Sync;

/// Drive an SSE response through a [`ResponsesStreamProcessor`].
///
/// `map_event` lets Codex rewrite its dialect (`response.done`, bare `error`
/// frames) into the standard Responses event shape before it reaches the shared
/// processor, exactly as upstream's `mapCodexEvents` generator does.
#[allow(clippy::too_many_arguments)]
pub async fn run_responses_sse(
    response: &mut pi_http::SseResponse,
    signal: &Option<pi_core::options::AbortSignal>,
    processor: &mut ResponsesStreamProcessor,
    output: &mut AssistantMessage,
    model: &Model,
    sink: &AssistantMessageEventSink,
    map_event: Option<&CodexEventMapper>,
) -> Result<(), AiError> {
    use crate::transport::{next_sse, SsePump};

    loop {
        match next_sse(response, signal).await {
            SsePump::Event(event) => {
                if event.is_done_sentinel() || event.data.trim().is_empty() {
                    continue;
                }
                let Ok(payload) = serde_json::from_str::<Value>(&event.data) else {
                    return Err(AiError::protocol(format!(
                        "Invalid Responses SSE JSON: {}",
                        event.data
                    )));
                };
                let (payload, stop) = match map_event {
                    Some(map) => match map(payload, output)? {
                        MappedEvent::Skip => continue,
                        MappedEvent::Emit(value) => (value, false),
                        MappedEvent::EmitAndStop(value) => (value, true),
                    },
                    None => (payload, false),
                };
                processor
                    .handle_event(&payload, output, model, sink)
                    .await?;
                if stop {
                    break;
                }
            }
            SsePump::Done => break,
            SsePump::Aborted => return Err(AiError::Aborted),
            SsePump::Failed(err) => return Err(err),
        }
    }

    if !processor.saw_terminal_response_event() {
        return Err(AiError::protocol(
            "OpenAI Responses stream ended before a terminal response event",
        ));
    }
    Ok(())
}

/// Port of `utils/deferred-tools.ts#splitDeferredTools`.
pub fn split_deferred_tools(
    context: &Context,
    enabled: bool,
) -> (Vec<Tool>, HashMap<String, Tool>) {
    let mut unique: Vec<(String, Tool)> = Vec::new();
    for tool in context.tools() {
        match unique.iter_mut().find(|(name, _)| name == &tool.name) {
            Some(entry) => entry.1 = tool.clone(),
            None => unique.push((tool.name.clone(), tool.clone())),
        }
    }
    if !enabled {
        return (unique.into_iter().map(|(_, t)| t).collect(), HashMap::new());
    }

    let mut deferred_names: HashSet<String> = HashSet::new();
    let mut used_names: HashSet<String> = HashSet::new();
    for message in &context.messages {
        match message {
            Message::Assistant(assistant) => {
                for call in assistant.tool_calls() {
                    used_names.insert(call.name.clone());
                }
            }
            Message::ToolResult(result) => {
                for name in result.added_tool_names.clone().unwrap_or_default() {
                    if !used_names.contains(&name) {
                        deferred_names.insert(name);
                    }
                }
            }
            Message::User(_) => {}
        }
    }

    let mut immediate = Vec::new();
    let mut deferred = HashMap::new();
    for (name, tool) in unique {
        if deferred_names.contains(&name) {
            deferred.insert(name, tool);
        } else {
            immediate.push(tool);
        }
    }
    (immediate, deferred)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::message::{ToolResultMessage, UserMessage};
    use pi_core::Api;

    fn model() -> Model {
        let mut model = Model::new(
            "gpt-5",
            Api::OpenAiResponses,
            "openai",
            "https://api.openai.com/v1",
        );
        model.reasoning = true;
        model
    }

    #[test]
    fn text_signature_round_trips_v1_and_legacy() {
        let encoded = encode_text_signature_v1("msg_1", Some(TextPhase::FinalAnswer));
        assert_eq!(encoded, r#"{"v":1,"id":"msg_1","phase":"final_answer"}"#);
        let parsed = parse_text_signature(Some(&encoded)).unwrap();
        assert_eq!(parsed.id, "msg_1");
        assert_eq!(parsed.phase, Some(TextPhase::FinalAnswer));

        let legacy = parse_text_signature(Some("msg_legacy")).unwrap();
        assert_eq!(legacy.id, "msg_legacy");
        assert_eq!(legacy.phase, None);
    }

    #[test]
    fn tool_result_ids_are_split_on_the_pipe() {
        let context = Context::new(vec![Message::ToolResult(ToolResultMessage::text(
            "call_1|fc_2",
            "bash",
            "ok",
            false,
        ))]);
        let messages = convert_responses_messages(
            &model(),
            &context,
            &HashSet::from(["openai"]),
            &ConvertResponsesMessagesOptions::defaults(),
        )
        .unwrap();
        assert_eq!(messages[0]["type"], "function_call_output");
        assert_eq!(messages[0]["call_id"], "call_1");
    }

    #[test]
    fn system_prompt_uses_developer_role_for_reasoning_models() {
        let context = Context::new(vec![Message::User(UserMessage::text("hi"))])
            .with_system_prompt("be brief");
        let messages = convert_responses_messages(
            &model(),
            &context,
            &HashSet::from(["openai"]),
            &ConvertResponsesMessagesOptions::defaults(),
        )
        .unwrap();
        assert_eq!(messages[0]["role"], "developer");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"][0]["type"], "input_text");
    }

    #[test]
    fn strict_mode_off_omits_the_strict_key() {
        let tools = vec![Tool::no_params("t", "d")];
        let options = ConvertResponsesToolsOptions::new(false, false);
        let converted = convert_responses_tools(&tools, &options).unwrap();
        assert!(converted[0].get("strict").is_none());

        let options = ConvertResponsesToolsOptions::new(true, false);
        let converted = convert_responses_tools(&tools, &options).unwrap();
        assert_eq!(converted[0]["strict"], json!(false));
    }

    #[test]
    fn codex_explicit_null_strict_serializes_as_null() {
        let tools = vec![Tool::no_params("t", "d")];
        let options = ConvertResponsesToolsOptions::new(true, false).with_explicit_null_strict();
        let converted = convert_responses_tools(&tools, &options).unwrap();
        assert_eq!(converted[0]["strict"], Value::Null);
    }

    #[test]
    fn service_tier_pricing_scales_every_component() {
        let mut usage = Usage {
            cost: pi_core::Cost {
                input: 1.0,
                output: 2.0,
                cache_read: 0.5,
                cache_write: 0.5,
                total: 4.0,
            },
            ..Default::default()
        };
        apply_service_tier_pricing(&mut usage, Some("flex"), "gpt-5");
        assert_eq!(usage.cost.total, 2.0);
        assert_eq!(usage.cost.output, 1.0);
    }

    #[test]
    fn codex_tier_resolution_keeps_the_requested_tier() {
        assert_eq!(
            resolve_codex_service_tier(Some("default"), Some("flex")),
            Some("flex")
        );
        assert_eq!(
            resolve_codex_service_tier(Some("flex"), Some("default")),
            Some("flex")
        );
        assert_eq!(
            resolve_codex_service_tier(None, Some("priority")),
            Some("priority")
        );
    }

    #[test]
    fn incomplete_max_output_tokens_maps_to_length() {
        assert_eq!(
            map_responses_stop_reason(Some("incomplete"), Some("max_output_tokens")).0,
            StopReason::Length
        );
        let (reason, message) =
            map_responses_stop_reason(Some("incomplete"), Some("content_filter"));
        assert_eq!(reason, StopReason::Error);
        assert_eq!(message.unwrap(), "Response incomplete: content_filter");
    }
}
