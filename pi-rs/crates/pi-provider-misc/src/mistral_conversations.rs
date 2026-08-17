//! `mistral-conversations` API implementation.
//! Port of `packages/ai/src/api/mistral-conversations.ts` (+ the no-op
//! `.lazy.ts`) and `packages/ai/src/providers/mistral.ts`.
//!
//! Talks to Mistral's native chat-completions endpoint. The payload is built in
//! the SDK's camelCase shape (that is what `on_payload` observes, matching
//! upstream) and remapped to Mistral's snake_case wire keys immediately before
//! the request goes out.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pi_core::api::{ApiClient, ApiClientRef};
use pi_core::content::{AssistantContent, TextContent, ThinkingContent, ToolCall};
use pi_core::error::AiError;
use pi_core::event::{
    AssistantMessageEvent, AssistantMessageEventSink, AssistantMessageEventStream, DoneReason,
    ErrorReason,
};
use pi_core::message::{AssistantMessage, Message, StopReason};
use pi_core::model::{CacheRetention, Model, ModelThinkingLevel};
use pi_core::options::{ProviderResponse, SimpleStreamOptions, StreamOptions};
use pi_core::tool::{Context, Tool};
use pi_core::{InputContent, UserContent};
use pi_http::HttpClient;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::provider::{ProviderDescriptor, ProviderRegistration};
use crate::support::http_stream::{self, SseRequest, TransportFailure};
use pi_http::hash::short_hash;
use pi_http::json_parse::parse_streaming_json_object as parse_streaming_json;
use pi_provider_common::constrained_sampling::{
    json_schema_tool_parameters, resolve_json_schema_strict_sampling,
};
use pi_provider_common::cost::calculate_cost;
use pi_provider_common::sanitize_unicode::sanitize_surrogates;
use pi_provider_common::simple_options::build_base_options;
use pi_provider_common::transform_messages::transform_messages;

pub const MISTRAL_CONVERSATIONS_API: &str = "mistral-conversations";
pub const MISTRAL_BASE_URL: &str = "https://api.mistral.ai";

const MISTRAL_TOOL_CALL_ID_LENGTH: usize = 9;
const MAX_MISTRAL_ERROR_BODY_CHARS: usize = 4000;
const DEFAULT_TIMEOUT_MS: u64 = 60_000;

/// Provider-option keys understood by this adapter, read from
/// [`StreamOptions::provider_options`].
pub mod option_keys {
    /// `"auto" | "none" | "any" | "required" | {type:"function",function:{name}}`.
    pub const TOOL_CHOICE: &str = "toolChoice";
    /// `"reasoning"` to switch a Magistral-style model into reasoning mode.
    pub const PROMPT_MODE: &str = "promptMode";
    /// `"none" | "high"` for models that take a reasoning effort instead.
    pub const REASONING_EFFORT: &str = "reasoningEffort";
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// `mistral-conversations` adapter.
#[derive(Clone)]
pub struct MistralConversationsApi {
    http: Arc<HttpClient>,
}

impl std::fmt::Debug for MistralConversationsApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MistralConversationsApi")
    }
}

impl Default for MistralConversationsApi {
    fn default() -> Self {
        Self::new()
    }
}

impl MistralConversationsApi {
    pub fn new() -> Self {
        Self {
            http: HttpClient::shared(),
        }
    }

    pub fn with_http_client(http: Arc<HttpClient>) -> Self {
        Self { http }
    }

    pub fn client(&self) -> ApiClientRef {
        Arc::new(self.clone())
    }

    fn start(
        &self,
        model: &Model,
        context: &Context,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        let (sink, stream) = AssistantMessageEventStream::channel(16);
        let http = self.http.clone();
        let model = model.clone();
        let context = context.clone();
        tokio::spawn(async move { run(http, sink, model, context, options).await });
        stream
    }
}

#[async_trait]
impl ApiClient for MistralConversationsApi {
    fn api(&self) -> &str {
        MISTRAL_CONVERSATIONS_API
    }

    async fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: &StreamOptions,
    ) -> Result<AssistantMessageEventStream, AiError> {
        Ok(self.start(model, context, options.clone()))
    }

    async fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, AiError> {
        let mut base = build_base_options(model, context, options, None);

        let reasoning = options
            .reasoning
            .map(|level| model.clamp_thinking_level(ModelThinkingLevel::from(level)))
            .filter(|level| *level != ModelThinkingLevel::Off);
        let use_reasoning = model.reasoning && reasoning.is_some();

        if use_reasoning && uses_prompt_mode_reasoning(model) {
            base.provider_options
                .insert(option_keys::PROMPT_MODE.into(), json!("reasoning"));
        }
        if use_reasoning && uses_reasoning_effort(model) {
            base.provider_options.insert(
                option_keys::REASONING_EFFORT.into(),
                json!(map_reasoning_effort(model, reasoning.expect("checked"))),
            );
        }

        Ok(self.start(model, context, base))
    }
}

/// The Mistral provider descriptor. `models` is left empty: `pi-catalog` owns
/// the generated model list and fills it in at registration time.
pub fn mistral_provider() -> ProviderRegistration {
    ProviderRegistration {
        descriptor: ProviderDescriptor::new("mistral", "Mistral", MISTRAL_CONVERSATIONS_API)
            .base_url(MISTRAL_BASE_URL)
            .api_key("Mistral API key", &["MISTRAL_API_KEY"]),
        client: MistralConversationsApi::new().client(),
    }
}

// ---------------------------------------------------------------------------
// Reasoning mode selection
// ---------------------------------------------------------------------------

fn uses_reasoning_effort(model: &Model) -> bool {
    matches!(
        model.id.as_str(),
        "mistral-small-2603" | "mistral-small-latest" | "mistral-medium-3.5"
    )
}

fn uses_prompt_mode_reasoning(model: &Model) -> bool {
    model.reasoning && !uses_reasoning_effort(model)
}

fn map_reasoning_effort(model: &Model, level: ModelThinkingLevel) -> String {
    model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(&level))
        .and_then(|mapped| mapped.clone())
        .unwrap_or_else(|| "high".to_string())
}

// ---------------------------------------------------------------------------
// Tool-call id normalization
// ---------------------------------------------------------------------------

/// Mistral requires 9-character alphanumeric tool-call ids. Ids from other
/// providers are hashed down, retrying on collision, and every rewrite is
/// remembered so the matching tool results line up.
struct MistralToolCallIdNormalizer {
    forward: std::cell::RefCell<std::collections::HashMap<String, String>>,
    reverse: std::cell::RefCell<std::collections::HashMap<String, String>>,
}

impl MistralToolCallIdNormalizer {
    fn new() -> Self {
        Self {
            forward: Default::default(),
            reverse: Default::default(),
        }
    }

    fn normalize(&self, id: &str) -> String {
        if let Some(existing) = self.forward.borrow().get(id) {
            return existing.clone();
        }
        let mut attempt = 0u32;
        loop {
            let candidate = derive_tool_call_id(id, attempt);
            let owner = self.reverse.borrow().get(&candidate).cloned();
            if owner.is_none() || owner.as_deref() == Some(id) {
                self.forward
                    .borrow_mut()
                    .insert(id.to_string(), candidate.clone());
                self.reverse
                    .borrow_mut()
                    .insert(candidate.clone(), id.to_string());
                return candidate;
            }
            attempt += 1;
        }
    }
}

fn derive_tool_call_id(id: &str, attempt: u32) -> String {
    let normalized: String = id.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if attempt == 0 && normalized.chars().count() == MISTRAL_TOOL_CALL_ID_LENGTH {
        return normalized;
    }
    let seed_base = if normalized.is_empty() {
        id.to_string()
    } else {
        normalized
    };
    let seed = if attempt == 0 {
        seed_base
    } else {
        format!("{seed_base}:{attempt}")
    };
    short_hash(&seed)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(MISTRAL_TOOL_CALL_ID_LENGTH)
        .collect()
}

// ---------------------------------------------------------------------------
// Request building
// ---------------------------------------------------------------------------

fn chat_completions_url(base_url: &str) -> Result<String, String> {
    let mut base =
        url::Url::parse(base_url).map_err(|error| format!("invalid base URL: {error}"))?;
    let path = format!("{}/", base.path().trim_end_matches('/'));
    base.set_path(&path);
    base.join("v1/chat/completions")
        .map(|url| url.to_string())
        .map_err(|error| format!("invalid base URL: {error}"))
}

fn should_use_prompt_caching(options: &StreamOptions) -> Option<&String> {
    if options.cache_retention == Some(CacheRetention::None) {
        return None;
    }
    options.session_id.as_ref().filter(|id| !id.is_empty())
}

fn build_headers(
    model: &Model,
    api_key: &str,
    options: &StreamOptions,
) -> BTreeMap<String, String> {
    let mut headers: BTreeMap<String, String> = [
        ("accept".to_string(), "text/event-stream".to_string()),
        ("authorization".to_string(), format!("Bearer {api_key}")),
        ("content-type".to_string(), "application/json".to_string()),
    ]
    .into_iter()
    .collect();

    if let Some(model_headers) = &model.headers {
        let overrides = model_headers
            .iter()
            .map(|(name, value)| (name.clone(), Some(value.clone())))
            .collect();
        headers = pi_http::merge_headers(headers, &overrides);
    }
    headers = pi_http::merge_headers(headers, &options.request.headers);

    let has_explicit_affinity = model
        .headers
        .as_ref()
        .is_some_and(|h| h.keys().any(|name| name.to_lowercase() == "x-affinity"))
        || options
            .request
            .headers
            .keys()
            .any(|name| name.to_lowercase() == "x-affinity");

    if let Some(session_id) = should_use_prompt_caching(options) {
        if !has_explicit_affinity {
            headers.insert("x-affinity".to_string(), session_id.clone());
        }
    }

    headers
}

fn build_chat_payload(
    model: &Model,
    context: &Context,
    messages: &[Message],
    options: &StreamOptions,
) -> Result<Value, String> {
    let mut payload = Map::new();
    payload.insert("model".into(), json!(model.id));
    payload.insert("stream".into(), json!(true));

    let mut wire_messages = to_chat_messages(messages, model.supports_images());
    if let Some(system_prompt) = &context.system_prompt {
        wire_messages.insert(
            0,
            json!({ "role": "system", "content": sanitize_surrogates(system_prompt) }),
        );
    }
    payload.insert("messages".into(), Value::Array(wire_messages));

    let tools = context.tools();
    if !tools.is_empty() {
        payload.insert("tools".into(), Value::Array(to_function_tools(tools)?));
    }
    if let Some(temperature) = options.temperature {
        payload.insert("temperature".into(), json!(temperature));
    }
    if let Some(max_tokens) = options.max_tokens {
        payload.insert("maxTokens".into(), json!(max_tokens));
    }
    if let Some(tool_choice) =
        map_tool_choice(options.provider_options.get(option_keys::TOOL_CHOICE))
    {
        payload.insert("toolChoice".into(), tool_choice);
    }
    if let Some(prompt_mode) = options.provider_options.get(option_keys::PROMPT_MODE) {
        payload.insert("promptMode".into(), prompt_mode.clone());
    }
    if let Some(effort) = options.provider_options.get(option_keys::REASONING_EFFORT) {
        payload.insert("reasoningEffort".into(), effort.clone());
    }
    if let Some(session_id) = should_use_prompt_caching(options) {
        payload.insert("promptCacheKey".into(), json!(session_id));
    }

    Ok(Value::Object(payload))
}

fn map_tool_choice(choice: Option<&Value>) -> Option<Value> {
    let choice = choice?;
    if let Some(text) = choice.as_str() {
        return match text {
            "auto" | "none" | "any" | "required" => Some(json!(text)),
            _ => None,
        };
    }
    let name = choice.get("function")?.get("name")?;
    Some(json!({ "type": "function", "function": { "name": name } }))
}

fn to_function_tools(tools: &[Tool]) -> Result<Vec<Value>, String> {
    tools
        .iter()
        .map(|tool| {
            // `to_function_tools` threads a plain `String` because the caller
            // folds it straight into an in-stream error message.
            let strict =
                resolve_json_schema_strict_sampling(tool, true).map_err(|error| error.message())?;
            Ok(json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": json_schema_tool_parameters(tool, strict),
                    "strict": strict.unwrap_or(false),
                }
            }))
        })
        .collect()
}

fn image_data_url(mime_type: &str, data: &str) -> String {
    format!("data:{mime_type};base64,{data}")
}

fn to_chat_messages(messages: &[Message], supports_images: bool) -> Vec<Value> {
    let mut result = Vec::new();

    for message in messages {
        match message {
            Message::User(user) => {
                let blocks = match &user.content {
                    UserContent::Text(text) => {
                        result
                            .push(json!({ "role": "user", "content": sanitize_surrogates(text) }));
                        continue;
                    }
                    UserContent::Blocks(blocks) => blocks,
                };
                let had_images = blocks
                    .iter()
                    .any(|block| matches!(block, InputContent::Image(_)));
                let content: Vec<Value> = blocks
                    .iter()
                    .filter(|block| matches!(block, InputContent::Text(_)) || supports_images)
                    .map(|block| match block {
                        InputContent::Text(text) => {
                            json!({ "type": "text", "text": sanitize_surrogates(&text.text) })
                        }
                        InputContent::Image(image) => json!({
                            "type": "image_url",
                            "imageUrl": image_data_url(&image.mime_type, &image.data),
                        }),
                    })
                    .collect();
                if !content.is_empty() {
                    result.push(json!({ "role": "user", "content": content }));
                    continue;
                }
                if had_images && !supports_images {
                    result.push(json!({
                        "role": "user",
                        "content": "(image omitted: model does not support images)",
                    }));
                }
            }
            Message::Assistant(assistant) => {
                let mut content_parts = Vec::new();
                let mut tool_calls = Vec::new();

                for block in &assistant.content {
                    match block {
                        AssistantContent::Text(text) => {
                            if !text.text.trim().is_empty() {
                                content_parts.push(json!({
                                    "type": "text",
                                    "text": sanitize_surrogates(&text.text),
                                }));
                            }
                        }
                        AssistantContent::Thinking(thinking) => {
                            if !thinking.thinking.trim().is_empty() {
                                content_parts.push(json!({
                                    "type": "thinking",
                                    "thinking": [{
                                        "type": "text",
                                        "text": sanitize_surrogates(&thinking.thinking),
                                    }],
                                }));
                            }
                        }
                        AssistantContent::ToolCall(call) => tool_calls.push(json!({
                            "id": call.id,
                            "type": "function",
                            "function": {
                                "name": call.name,
                                "arguments": serde_json::to_string(&call.arguments)
                                    .unwrap_or_else(|_| "{}".to_string()),
                            },
                            "index": 0,
                        })),
                    }
                }

                if content_parts.is_empty() && tool_calls.is_empty() {
                    continue;
                }
                let mut assistant_message = Map::new();
                assistant_message.insert("role".into(), json!("assistant"));
                assistant_message.insert("prefix".into(), json!(false));
                if !content_parts.is_empty() {
                    assistant_message.insert("content".into(), Value::Array(content_parts));
                }
                if !tool_calls.is_empty() {
                    assistant_message.insert("toolCalls".into(), Value::Array(tool_calls));
                }
                result.push(Value::Object(assistant_message));
            }
            Message::ToolResult(tool_result) => {
                let text_result = tool_result
                    .content
                    .iter()
                    .filter_map(|block| block.as_text())
                    .map(|text| sanitize_surrogates(&text.text).to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                let has_images = tool_result
                    .content
                    .iter()
                    .any(|block| matches!(block, InputContent::Image(_)));
                let mut content = vec![json!({
                    "type": "text",
                    "text": build_tool_result_text(
                        &text_result,
                        has_images,
                        supports_images,
                        tool_result.is_error,
                    ),
                })];
                if supports_images {
                    for block in &tool_result.content {
                        if let InputContent::Image(image) = block {
                            content.push(json!({
                                "type": "image_url",
                                "imageUrl": image_data_url(&image.mime_type, &image.data),
                            }));
                        }
                    }
                }
                result.push(json!({
                    "role": "tool",
                    "toolCallId": tool_result.tool_call_id,
                    "name": tool_result.tool_name,
                    "content": content,
                }));
            }
        }
    }

    result
}

fn build_tool_result_text(
    text: &str,
    has_images: bool,
    supports_images: bool,
    is_error: bool,
) -> String {
    let trimmed = text.trim();
    let error_prefix = if is_error { "[tool error] " } else { "" };

    if !trimmed.is_empty() {
        let image_suffix = if has_images && !supports_images {
            "\n[tool image omitted: model does not support images]"
        } else {
            ""
        };
        return format!("{error_prefix}{trimmed}{image_suffix}");
    }
    if has_images {
        if supports_images {
            return if is_error {
                "[tool error] (see attached image)".to_string()
            } else {
                "(see attached image)".to_string()
            };
        }
        return if is_error {
            "[tool error] (image omitted: model does not support images)".to_string()
        } else {
            "(image omitted: model does not support images)".to_string()
        };
    }
    if is_error {
        "[tool error] (no tool output)".to_string()
    } else {
        "(no tool output)".to_string()
    }
}

// ---------------------------------------------------------------------------
// camelCase payload -> Mistral wire payload
// ---------------------------------------------------------------------------

const PAYLOAD_KEY_MAP: [(&str, &str); 12] = [
    ("topP", "top_p"),
    ("maxTokens", "max_tokens"),
    ("randomSeed", "random_seed"),
    ("responseFormat", "response_format"),
    ("toolChoice", "tool_choice"),
    ("presencePenalty", "presence_penalty"),
    ("frequencyPenalty", "frequency_penalty"),
    ("parallelToolCalls", "parallel_tool_calls"),
    ("reasoningEffort", "reasoning_effort"),
    ("promptMode", "prompt_mode"),
    ("promptCacheKey", "prompt_cache_key"),
    ("safePrompt", "safe_prompt"),
];

const CONTENT_CHUNK_KEY_MAP: [(&str, &str); 6] = [
    ("imageUrl", "image_url"),
    ("documentUrl", "document_url"),
    ("documentName", "document_name"),
    ("fileId", "file_id"),
    ("referenceIds", "reference_ids"),
    ("inputAudio", "input_audio"),
];

fn remap(record: &mut Map<String, Value>, source: &str, target: &str) {
    if let Some(value) = record.remove(source) {
        record.insert(target.to_string(), value);
    }
}

/// Rename the SDK's camelCase keys to Mistral's snake_case wire keys.
pub fn to_wire_payload(payload: &Value) -> Value {
    let Some(object) = payload.as_object() else {
        return payload.clone();
    };
    let mut wire = object.clone();
    for (source, target) in PAYLOAD_KEY_MAP {
        remap(&mut wire, source, target);
    }

    if let Some(Value::Array(messages)) = wire.get("messages") {
        let messages: Vec<Value> = messages.iter().map(to_wire_message).collect();
        wire.insert("messages".into(), Value::Array(messages));
    }

    if let Some(Value::Object(response_format)) = wire.get("response_format") {
        let mut response_format = response_format.clone();
        remap(&mut response_format, "jsonSchema", "json_schema");
        if let Some(Value::Object(json_schema)) = response_format.get("json_schema") {
            let mut json_schema = json_schema.clone();
            remap(&mut json_schema, "schemaDefinition", "schema");
            response_format.insert("json_schema".into(), Value::Object(json_schema));
        }
        wire.insert("response_format".into(), Value::Object(response_format));
    }

    Value::Object(wire)
}

fn to_wire_message(message: &Value) -> Value {
    let Some(object) = message.as_object() else {
        return message.clone();
    };
    let mut wire = object.clone();
    remap(&mut wire, "toolCalls", "tool_calls");
    remap(&mut wire, "toolCallId", "tool_call_id");
    if let Some(Value::Array(content)) = wire.get("content") {
        let content: Vec<Value> = content.iter().map(to_wire_content_chunk).collect();
        wire.insert("content".into(), Value::Array(content));
    }
    Value::Object(wire)
}

fn to_wire_content_chunk(chunk: &Value) -> Value {
    let Some(object) = chunk.as_object() else {
        return chunk.clone();
    };
    let mut wire = object.clone();
    for (source, target) in CONTENT_CHUNK_KEY_MAP {
        remap(&mut wire, source, target);
    }
    Value::Object(wire)
}

// ---------------------------------------------------------------------------
// Streaming wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct MistralChunk {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    usage: Option<Value>,
    choices: Vec<MistralChoice>,
}

#[derive(Debug, Deserialize)]
struct MistralChoice {
    #[serde(default)]
    finish_reason: Option<String>,
    #[serde(default)]
    delta: MistralDelta,
}

#[derive(Debug, Default, Deserialize)]
struct MistralDelta {
    #[serde(default)]
    content: Option<MistralDeltaContent>,
    #[serde(default)]
    tool_calls: Option<Vec<MistralStreamToolCall>>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum MistralDeltaContent {
    Text(String),
    Chunks(Vec<MistralStreamContentChunk>),
}

#[derive(Debug, Deserialize)]
struct MistralStreamContentChunk {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    thinking: Option<Vec<MistralThinkingPart>>,
}

#[derive(Debug, Deserialize)]
struct MistralThinkingPart {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MistralStreamToolCall {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    index: Option<i64>,
    function: MistralStreamFunction,
}

#[derive(Debug, Deserialize)]
struct MistralStreamFunction {
    #[serde(default)]
    name: String,
    #[serde(default)]
    arguments: Value,
}

fn stop_reason_for(reason: &str) -> (StopReason, Option<String>) {
    match reason {
        "stop" => (StopReason::Stop, None),
        "length" | "model_length" => (StopReason::Length, None),
        "tool_calls" => (StopReason::ToolUse, None),
        "error" => (
            StopReason::Error,
            Some("Provider stopped with: error".to_string()),
        ),
        other => (
            StopReason::Error,
            Some(format!("Provider stopped with: {other}")),
        ),
    }
}

/// Mistral reports cached prompt tokens under a handful of spellings.
fn cached_prompt_tokens(usage: &Value, prompt_tokens: i64) -> i64 {
    const PATHS: [(&str, &str); 4] = [
        ("promptTokensDetails", "cachedTokens"),
        ("prompt_tokens_details", "cached_tokens"),
        ("promptTokenDetails", "cachedTokens"),
        ("prompt_token_details", "cached_tokens"),
    ];
    let mut cached: Option<f64> = None;
    for (outer, inner) in PATHS {
        if let Some(value) = usage
            .get(outer)
            .and_then(|d| d.get(inner))
            .and_then(Value::as_f64)
        {
            cached = Some(value);
            break;
        }
    }
    let cached = cached
        .or_else(|| usage.get("numCachedTokens").and_then(Value::as_f64))
        .or_else(|| usage.get("num_cached_tokens").and_then(Value::as_f64))
        .unwrap_or(0.0);
    if !cached.is_finite() || cached <= 0.0 {
        return 0;
    }
    // Upstream is `Math.min(promptTokens, Math.max(0, cachedTokens))`; the
    // `cached <= 0.0` guard above already covers the inner clamp.
    (cached as i64).min(prompt_tokens)
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

fn format_error(failure: &TransportFailure) -> String {
    match failure {
        TransportFailure::Status {
            status,
            status_text,
            body,
            ..
        } => {
            let trimmed = body.trim();
            if !trimmed.is_empty() {
                format!(
                    "Mistral API error ({status}): {}",
                    truncate_error_text(trimmed)
                )
            } else if status_text.is_empty() {
                format!("Mistral API error ({status}): Request failed with status {status}")
            } else {
                format!("Mistral API error ({status}): {status_text}")
            }
        }
        other => other.message(),
    }
}

fn truncate_error_text(text: &str) -> String {
    let count = text.chars().count();
    if count <= MAX_MISTRAL_ERROR_BODY_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_MISTRAL_ERROR_BODY_CHARS).collect();
    format!(
        "{head}... [truncated {} chars]",
        count - MAX_MISTRAL_ERROR_BODY_CHARS
    )
}

async fn run(
    http: Arc<HttpClient>,
    sink: AssistantMessageEventSink,
    model: Model,
    context: Context,
    options: StreamOptions,
) {
    let mut output = AssistantMessage::pending(model.api.as_str(), &model.provider, &model.id);

    match drive(&http, &sink, &model, &context, &options, &mut output).await {
        Ok(reason) => {
            output.stop_reason = reason.into();
            sink.send(AssistantMessageEvent::Done {
                reason,
                message: output,
            })
            .await;
        }
        Err(error) => {
            let aborted = options.request.is_aborted() || error.aborted;
            output.stop_reason = if aborted {
                StopReason::Aborted
            } else {
                StopReason::Error
            };
            output.error_message = Some(error.message);
            sink.send(AssistantMessageEvent::Error {
                reason: if aborted {
                    ErrorReason::Aborted
                } else {
                    ErrorReason::Error
                },
                error: output,
            })
            .await;
        }
    }
}

struct StreamError {
    message: String,
    aborted: bool,
}

impl StreamError {
    fn plain(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            aborted: false,
        }
    }
}

async fn drive(
    http: &HttpClient,
    sink: &AssistantMessageEventSink,
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    output: &mut AssistantMessage,
) -> Result<DoneReason, StreamError> {
    let Some(api_key) = options.request.api_key.clone() else {
        return Err(StreamError::plain(format!(
            "No API key for provider: {}",
            model.provider
        )));
    };

    // Scoped so the (non-`Sync`) normalizer is not held across an await point.
    let messages = {
        let normalizer = MistralToolCallIdNormalizer::new();
        // Upstream's normalizer is `(id, model, source)`; Mistral only needs the id.
        let normalize = |id: &str, _model: &Model, _source: &pi_core::AssistantMessage| {
            normalizer.normalize(id)
        };
        transform_messages(&context.messages, model, Some(&normalize))
    };

    let mut payload =
        build_chat_payload(model, context, &messages, options).map_err(StreamError::plain)?;
    if let Some(on_payload) = &options.request.on_payload {
        if let Some(replacement) = on_payload(&payload, model) {
            payload = replacement;
        }
    }

    let url = chat_completions_url(&model.base_url).map_err(StreamError::plain)?;
    let request = SseRequest {
        url,
        headers: build_headers(model, &api_key, options),
        body: to_wire_payload(&payload),
        signal: options.request.signal.clone(),
        timeout: Some(Duration::from_millis(
            options.request.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS),
        )),
    };

    let mut response = match http_stream::post_sse(http, request).await {
        Ok(response) => response,
        Err(failure) => {
            notify_response(options, model, &failure);
            return Err(StreamError {
                message: format_error(&failure),
                aborted: failure.is_aborted(),
            });
        }
    };

    if let Some(on_response) = &options.request.on_response {
        on_response(
            &ProviderResponse {
                status: response.status,
                headers: response.headers.clone(),
            },
            model,
        );
    }

    sink.send(AssistantMessageEvent::Start {
        partial: output.clone(),
    })
    .await;

    consume(&mut response, sink, model, output).await?;

    if options.request.is_aborted() {
        return Err(StreamError {
            message: "Request was aborted".to_string(),
            aborted: true,
        });
    }
    match output.stop_reason {
        StopReason::Pending => Err(StreamError::plain(
            "Mistral stream ended without a finish reason",
        )),
        StopReason::Aborted | StopReason::Error => Err(StreamError {
            message: output
                .error_message
                .clone()
                .unwrap_or_else(|| "An unknown error occurred".to_string()),
            aborted: output.stop_reason == StopReason::Aborted,
        }),
        StopReason::Stop => Ok(DoneReason::Stop),
        StopReason::Length => Ok(DoneReason::Length),
        StopReason::ToolUse => Ok(DoneReason::ToolUse),
        StopReason::Deferred => Ok(DoneReason::Deferred),
    }
}

fn notify_response(options: &StreamOptions, model: &Model, failure: &TransportFailure) {
    let Some(on_response) = &options.request.on_response else {
        return;
    };
    if let TransportFailure::Status {
        status, headers, ..
    } = failure
    {
        on_response(
            &ProviderResponse {
                status: *status,
                headers: headers.clone(),
            },
            model,
        );
    }
}

/// Consume the SSE stream, translating Mistral chunks into protocol events and
/// accumulating `output`.
async fn consume(
    response: &mut http_stream::SseStreamResponse,
    sink: &AssistantMessageEventSink,
    model: &Model,
    output: &mut AssistantMessage,
) -> Result<(), StreamError> {
    // Index of the open text/thinking block, if any.
    let mut current: Option<usize> = None;
    // Insertion-ordered `${id}:${index}` -> content index.
    let mut tool_blocks: Vec<(String, usize)> = Vec::new();
    let mut tool_args: Vec<String> = Vec::new();

    while let Some(event) = response.next_event().await {
        let event = match event {
            Ok(event) => event,
            Err(failure) => {
                return Err(StreamError {
                    message: format_error(&failure),
                    aborted: failure.is_aborted(),
                })
            }
        };
        if event.is_done_sentinel() {
            break;
        }
        if event.data.trim().is_empty() {
            continue;
        }
        let chunk: MistralChunk = event
            .json()
            .map_err(|_| StreamError::plain("Invalid Mistral streaming event"))?;

        // Mistral's streamed CompletionChunk carries an id; keep the first
        // non-empty one as the stable response identifier.
        if output.response_id.as_deref().unwrap_or("").is_empty() {
            if let Some(id) = chunk.id.filter(|id| !id.is_empty()) {
                output.response_id = Some(id);
            }
        }

        if let Some(usage) = &chunk.usage {
            // Token counts are signed: corrective records carry negative deltas.
            let prompt_tokens = usage
                .get("prompt_tokens")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let cached = cached_prompt_tokens(usage, prompt_tokens);
            // Upstream is `Math.max(0, promptTokens - cachedPromptTokens)`: an
            // explicit clamp at zero, not signed saturation.
            output.usage.input = (prompt_tokens - cached).max(0);
            output.usage.output = usage
                .get("completion_tokens")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            output.usage.cache_read = cached;
            output.usage.cache_write = 0;
            output.usage.total_tokens = usage
                .get("total_tokens")
                .and_then(Value::as_i64)
                // Upstream's `||` fallback treats 0 as absent; a negative total
                // is a real corrective value and must be kept.
                .filter(|total| *total != 0)
                .unwrap_or(
                    output.usage.input
                        + output.usage.output
                        + output.usage.cache_read
                        + output.usage.cache_write,
                );
            calculate_cost(model, &mut output.usage);
        }

        let Some(choice) = chunk.choices.into_iter().next() else {
            continue;
        };

        if let Some(finish_reason) = choice.finish_reason.filter(|reason| !reason.is_empty()) {
            let (stop_reason, error_message) = stop_reason_for(&finish_reason);
            output.raw_stop_reason = Some(finish_reason);
            output.stop_reason = stop_reason;
            if let Some(message) = error_message {
                output.error_message = Some(message);
            }
        }

        if let Some(content) = choice.delta.content {
            let items: Vec<MistralStreamContentChunk> = match content {
                MistralDeltaContent::Text(text) => vec![MistralStreamContentChunk {
                    kind: "text".to_string(),
                    text: Some(text),
                    thinking: None,
                }],
                MistralDeltaContent::Chunks(chunks) => chunks,
            };

            for item in items {
                match item.kind.as_str() {
                    "thinking" => {
                        let delta: String = item
                            .thinking
                            .unwrap_or_default()
                            .into_iter()
                            .filter_map(|part| part.text)
                            .filter(|text| !text.is_empty())
                            .collect();
                        if delta.is_empty() {
                            continue;
                        }
                        let is_thinking = current.is_some_and(|index| {
                            matches!(
                                output.content.get(index),
                                Some(AssistantContent::Thinking(_))
                            )
                        });
                        if !is_thinking {
                            finish_block(sink, output, current.take()).await;
                            output
                                .content
                                .push(AssistantContent::Thinking(ThinkingContent::default()));
                            current = Some(output.content.len() - 1);
                            sink.send(AssistantMessageEvent::ThinkingStart {
                                content_index: output.content.len() - 1,
                                partial: output.clone(),
                            })
                            .await;
                        }
                        let index = current.expect("thinking block open");
                        if let Some(AssistantContent::Thinking(block)) =
                            output.content.get_mut(index)
                        {
                            block.thinking.push_str(&delta);
                        }
                        sink.send(AssistantMessageEvent::ThinkingDelta {
                            content_index: index,
                            delta,
                            partial: output.clone(),
                        })
                        .await;
                    }
                    "text" => {
                        let delta = item.text.unwrap_or_default();
                        let is_text = current.is_some_and(|index| {
                            matches!(output.content.get(index), Some(AssistantContent::Text(_)))
                        });
                        if !is_text {
                            finish_block(sink, output, current.take()).await;
                            output
                                .content
                                .push(AssistantContent::Text(TextContent::default()));
                            current = Some(output.content.len() - 1);
                            sink.send(AssistantMessageEvent::TextStart {
                                content_index: output.content.len() - 1,
                                partial: output.clone(),
                            })
                            .await;
                        }
                        let index = current.expect("text block open");
                        if let Some(AssistantContent::Text(block)) = output.content.get_mut(index) {
                            block.text.push_str(&delta);
                        }
                        sink.send(AssistantMessageEvent::TextDelta {
                            content_index: index,
                            delta,
                            partial: output.clone(),
                        })
                        .await;
                    }
                    _ => {}
                }
            }
        }

        for tool_call in choice.delta.tool_calls.unwrap_or_default() {
            finish_block(sink, output, current.take()).await;

            let call_id = match tool_call.id.as_deref() {
                Some(id) if !id.is_empty() && id != "null" => id.to_string(),
                _ => derive_tool_call_id(&format!("toolcall:{}", tool_call.index.unwrap_or(0)), 0),
            };
            let key = format!("{call_id}:{}", tool_call.index.unwrap_or(0));

            let slot = match tool_blocks.iter().position(|(k, _)| *k == key) {
                Some(slot) => slot,
                None => {
                    output
                        .content
                        .push(AssistantContent::ToolCall(ToolCall::new(
                            call_id.clone(),
                            tool_call.function.name.clone(),
                        )));
                    tool_blocks.push((key, output.content.len() - 1));
                    tool_args.push(String::new());
                    sink.send(AssistantMessageEvent::ToolCallStart {
                        content_index: output.content.len() - 1,
                        partial: output.clone(),
                    })
                    .await;
                    tool_blocks.len() - 1
                }
            };
            let content_index = tool_blocks[slot].1;

            let args_delta = match &tool_call.function.arguments {
                Value::String(text) => text.clone(),
                Value::Null => "{}".to_string(),
                other => serde_json::to_string(other).unwrap_or_else(|_| "{}".to_string()),
            };
            tool_args[slot].push_str(&args_delta);
            let parsed = parse_streaming_json(Some(&tool_args[slot]));
            if let Some(AssistantContent::ToolCall(block)) = output.content.get_mut(content_index) {
                block.arguments = parsed;
            }
            sink.send(AssistantMessageEvent::ToolCallDelta {
                content_index,
                delta: args_delta,
                partial: output.clone(),
            })
            .await;
        }
    }

    finish_block(sink, output, current.take()).await;

    for (slot, (_, content_index)) in tool_blocks.iter().enumerate() {
        let parsed = parse_streaming_json(Some(&tool_args[slot]));
        let Some(AssistantContent::ToolCall(block)) = output.content.get_mut(*content_index) else {
            continue;
        };
        block.arguments = parsed;
        let tool_call = block.clone();
        sink.send(AssistantMessageEvent::ToolCallEnd {
            content_index: *content_index,
            tool_call,
            partial: output.clone(),
        })
        .await;
    }

    Ok(())
}

async fn finish_block(
    sink: &AssistantMessageEventSink,
    output: &AssistantMessage,
    index: Option<usize>,
) {
    let Some(index) = index else {
        return;
    };
    match output.content.get(index) {
        Some(AssistantContent::Text(text)) => {
            sink.send(AssistantMessageEvent::TextEnd {
                content_index: index,
                content: text.text.clone(),
                partial: output.clone(),
            })
            .await;
        }
        Some(AssistantContent::Thinking(thinking)) => {
            sink.send(AssistantMessageEvent::ThinkingEnd {
                content_index: index,
                content: thinking.thinking.clone(),
                partial: output.clone(),
            })
            .await;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::model::Api;

    fn model() -> Model {
        Model::new(
            "mistral-large-latest",
            Api::MistralConversations,
            "mistral",
            MISTRAL_BASE_URL,
        )
    }

    #[test]
    fn builds_the_chat_completions_url() {
        assert_eq!(
            chat_completions_url("https://api.mistral.ai").unwrap(),
            "https://api.mistral.ai/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://gateway.example/mistral/").unwrap(),
            "https://gateway.example/mistral/v1/chat/completions"
        );
    }

    #[test]
    fn remaps_camel_case_payload_keys() {
        let payload = json!({
            "model": "m",
            "maxTokens": 10,
            "promptMode": "reasoning",
            "promptCacheKey": "s",
            "responseFormat": {
                "type": "json_schema",
                "jsonSchema": { "name": "r", "schemaDefinition": { "type": "object" } }
            },
            "messages": [
                { "role": "assistant", "toolCalls": [], "content": [{"type": "image_url", "imageUrl": "data:x"}] },
                { "role": "tool", "toolCallId": "abc" }
            ]
        });
        let wire = to_wire_payload(&payload);
        assert_eq!(wire["max_tokens"], 10);
        assert_eq!(wire["prompt_mode"], "reasoning");
        assert_eq!(wire["prompt_cache_key"], "s");
        assert!(wire.get("maxTokens").is_none());
        assert_eq!(
            wire["response_format"]["json_schema"]["schema"],
            json!({ "type": "object" })
        );
        assert_eq!(wire["messages"][0]["tool_calls"], json!([]));
        assert_eq!(wire["messages"][0]["content"][0]["image_url"], "data:x");
        assert_eq!(wire["messages"][1]["tool_call_id"], "abc");
    }

    #[test]
    fn derives_nine_character_tool_call_ids() {
        assert_eq!(derive_tool_call_id("abc123456", 0), "abc123456");
        let derived = derive_tool_call_id("fc_68a4b|very-long-openai-id", 0);
        assert_eq!(derived.chars().count(), MISTRAL_TOOL_CALL_ID_LENGTH);
        assert!(derived.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn normalizer_is_stable_and_collision_free() {
        let normalizer = MistralToolCallIdNormalizer::new();
        let first = normalizer.normalize("call_one");
        assert_eq!(normalizer.normalize("call_one"), first);
        assert_ne!(normalizer.normalize("call_two"), first);
    }

    #[test]
    fn maps_finish_reasons() {
        assert_eq!(stop_reason_for("stop").0, StopReason::Stop);
        assert_eq!(stop_reason_for("model_length").0, StopReason::Length);
        assert_eq!(stop_reason_for("tool_calls").0, StopReason::ToolUse);
        assert_eq!(
            stop_reason_for("weird"),
            (
                StopReason::Error,
                Some("Provider stopped with: weird".to_string())
            )
        );
    }

    #[test]
    fn reads_cached_tokens_under_every_spelling() {
        assert_eq!(
            cached_prompt_tokens(&json!({"prompt_tokens_details": {"cached_tokens": 3}}), 10),
            3
        );
        assert_eq!(
            cached_prompt_tokens(&json!({"promptTokensDetails": {"cachedTokens": 4}}), 10),
            4
        );
        assert_eq!(
            cached_prompt_tokens(&json!({"num_cached_tokens": 5}), 10),
            5
        );
        // Never exceeds the prompt total, never goes negative.
        assert_eq!(
            cached_prompt_tokens(&json!({"num_cached_tokens": 50}), 10),
            10
        );
        assert_eq!(
            cached_prompt_tokens(&json!({"num_cached_tokens": -1}), 10),
            0
        );
        assert_eq!(cached_prompt_tokens(&json!({}), 10), 0);
    }

    #[test]
    fn tool_result_text_covers_the_placeholder_matrix() {
        assert_eq!(build_tool_result_text("ok", false, true, false), "ok");
        assert_eq!(
            build_tool_result_text("ok", true, false, true),
            "[tool error] ok\n[tool image omitted: model does not support images]"
        );
        assert_eq!(
            build_tool_result_text("", true, true, false),
            "(see attached image)"
        );
        assert_eq!(
            build_tool_result_text("", false, true, true),
            "[tool error] (no tool output)"
        );
    }

    #[test]
    fn formats_http_errors_like_upstream() {
        let failure = TransportFailure::Status {
            status: 403,
            status_text: "Forbidden".into(),
            headers: Default::default(),
            body: r#"{"message":"blocked by gateway"}"#.into(),
        };
        assert_eq!(
            format_error(&failure),
            r#"Mistral API error (403): {"message":"blocked by gateway"}"#
        );
    }

    #[test]
    fn session_id_drives_prompt_caching() {
        let mut options = StreamOptions {
            session_id: Some("session-1".into()),
            ..Default::default()
        };
        assert_eq!(
            should_use_prompt_caching(&options),
            Some(&"session-1".to_string())
        );
        options.cache_retention = Some(CacheRetention::None);
        assert_eq!(should_use_prompt_caching(&options), None);
    }

    #[test]
    fn header_overrides_are_case_insensitive_and_deletable() {
        let mut model = model();
        model.headers = Some(
            [
                ("Authorization".to_string(), "Bearer model-key".to_string()),
                ("X-Affinity".to_string(), "model-affinity".to_string()),
            ]
            .into_iter()
            .collect(),
        );
        let mut options = StreamOptions {
            session_id: Some("automatic".into()),
            ..Default::default()
        };
        options.request.headers.insert("authorization".into(), None);
        options.request.headers.insert("x-affinity".into(), None);

        let headers = build_headers(&model, "request-key", &options);
        assert!(!headers.keys().any(|k| k.to_lowercase() == "authorization"));
        assert!(!headers.keys().any(|k| k.to_lowercase() == "x-affinity"));
    }

    #[test]
    fn sets_affinity_from_session_id() {
        let options = StreamOptions {
            session_id: Some("session-1".into()),
            ..Default::default()
        };
        let headers = build_headers(&model(), "key", &options);
        assert_eq!(headers["x-affinity"], "session-1");
        assert_eq!(headers["authorization"], "Bearer key");
        assert_eq!(headers["accept"], "text/event-stream");
    }
}
