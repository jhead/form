//! Port of `.upstream/packages/server/src/protocol.ts`.
//!
//! Bridges `pi-core` (upstream `pi-ai`) runtime values into the protocol's
//! transcript vocabulary. Everything here is lossy in one direction only: a
//! protocol payload never carries provider replay metadata, diagnostics,
//! cache-write retention splits, transport settings, sampling defaults, pricing
//! tiers or deferred-tool handles.
//!
//! Two upstream guards are unnecessary in Rust and are noted rather than
//! ported: `toProtocolJsonValue` rejects `Infinity`, `bigint`, `undefined` and
//! circular references, none of which `serde_json::Value` can represent.
//! `sanitizeProtocolDetails` keeps its `[Circular]` fallback out for the same
//! reason, but still normalizes non-finite numbers, which a `Value` *can* hold
//! if it was built by hand.

use pi_core::{
    AssistantContent as AiAssistantContent, AssistantMessage, ImageContent as AiImageContent,
    InputContent, Model, StopReason, TextContent as AiTextContent, ToolCall, ToolResultMessage,
    Usage as AiUsage, UserContent as AiUserContent, UserMessage,
};
use pi_protocol::{
    AssistantContent, AssistantRole, AssistantStatus, AssistantStopReason, AssistantTranscriptItem,
    ImageContent, JsonValue, ModelCost, ModelMetadata, ModelRef, TextContent, ThinkingContent,
    ToolCallContent, ToolContent, ToolRole, ToolStatus, ToolTranscriptItem, Usage, UsageCost,
    UserContent, UserRole, UserTranscriptItem,
};

/// Upstream throws `TypeError`; the Rust port returns it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ProtocolBridgeError(pub String);

type Bridged<T> = Result<T, ProtocolBridgeError>;

fn fail<T>(message: impl Into<String>) -> Bridged<T> {
    Err(ProtocolBridgeError(message.into()))
}

fn identifier(value: &str, label: &str) -> Bridged<String> {
    if value.is_empty() {
        return fail(format!("{label} must be a non-empty string"));
    }
    Ok(value.to_string())
}

fn timestamp(value: i64) -> Bridged<u64> {
    if value < 0 {
        return fail("Protocol timestamps must be non-negative integers");
    }
    Ok(value as u64)
}

fn non_negative_integer(value: i64) -> u64 {
    value.max(0) as u64
}

fn non_negative_number(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

/// Validate and copy a value from an execution boundary into the protocol's
/// JSON subset.
pub fn to_protocol_json_value(value: &JsonValue) -> Bridged<JsonValue> {
    match value {
        JsonValue::Null | JsonValue::Bool(_) | JsonValue::String(_) => Ok(value.clone()),
        JsonValue::Number(number) => {
            if number.as_f64().is_some_and(f64::is_finite) || number.is_i64() || number.is_u64() {
                Ok(value.clone())
            } else {
                fail("Protocol JSON numbers must be finite")
            }
        }
        JsonValue::Array(items) => Ok(JsonValue::Array(
            items
                .iter()
                .map(to_protocol_json_value)
                .collect::<Bridged<Vec<_>>>()?,
        )),
        JsonValue::Object(entries) => {
            let mut result = serde_json::Map::with_capacity(entries.len());
            for (key, entry) in entries {
                result.insert(key.clone(), to_protocol_json_value(entry)?);
            }
            Ok(JsonValue::Object(result))
        }
    }
}

/// Lossily sanitize diagnostic tool details that must not affect execution
/// semantics.
pub fn sanitize_protocol_details(value: &JsonValue) -> JsonValue {
    match value {
        JsonValue::Number(number) => match number.as_f64() {
            Some(number) if number.is_finite() => value.clone(),
            Some(number) => JsonValue::String(number.to_string()),
            None => value.clone(),
        },
        JsonValue::Array(items) => {
            JsonValue::Array(items.iter().map(sanitize_protocol_details).collect())
        }
        JsonValue::Object(entries) => JsonValue::Object(
            entries
                .iter()
                .map(|(key, entry)| (key.clone(), sanitize_protocol_details(entry)))
                .collect(),
        ),
        other => other.clone(),
    }
}

pub fn to_protocol_usage(usage: &AiUsage) -> Usage {
    Usage {
        input: non_negative_integer(usage.input),
        output: non_negative_integer(usage.output),
        cache_read: non_negative_integer(usage.cache_read),
        cache_write: non_negative_integer(usage.cache_write),
        reasoning: usage.reasoning.map(non_negative_integer),
        total_tokens: non_negative_integer(usage.total_tokens),
        cost: UsageCost {
            input: non_negative_number(usage.cost.input),
            output: non_negative_number(usage.cost.output),
            cache_read: non_negative_number(usage.cost.cache_read),
            cache_write: non_negative_number(usage.cost.cache_write),
            total: non_negative_number(usage.cost.total),
        },
    }
}

pub fn to_protocol_model_metadata(model: &Model, authenticated: bool) -> Bridged<ModelMetadata> {
    Ok(ModelMetadata {
        provider: identifier(&model.provider, "Model provider")?,
        id: identifier(&model.id, "Model id")?,
        name: identifier(&model.name, "Model name")?,
        api: identifier(model.api.as_str(), "Model API")?,
        reasoning: model.reasoning,
        input: model.input.clone(),
        context_window: model.context_window.max(1),
        max_tokens: model.max_tokens.max(1),
        cost: ModelCost {
            input: non_negative_number(model.cost.rates.input),
            output: non_negative_number(model.cost.rates.output),
            cache_read: non_negative_number(model.cost.rates.cache_read),
            cache_write: non_negative_number(model.cost.rates.cache_write),
        },
        supported_thinking_levels: model.supported_thinking_levels(),
        authenticated,
    })
}

fn to_protocol_user_content(content: &AiUserContent) -> Vec<UserContent> {
    match content {
        AiUserContent::Text(text) => vec![UserContent::Text(TextContent { text: text.clone() })],
        AiUserContent::Blocks(blocks) => blocks.iter().map(to_protocol_input_content).collect(),
    }
}

fn to_protocol_input_content(part: &InputContent) -> UserContent {
    match part {
        InputContent::Text(text) => UserContent::Text(TextContent {
            text: text.text.clone(),
        }),
        InputContent::Image(image) => UserContent::Image(ImageContent {
            data: image.data.clone(),
            mime_type: image.mime_type.clone(),
        }),
    }
}

pub fn to_protocol_user_message(message: &UserMessage, id: &str) -> Bridged<UserTranscriptItem> {
    Ok(UserTranscriptItem {
        id: identifier(id, "Transcript item id")?,
        role: UserRole::User,
        content: to_protocol_user_content(&message.content),
        timestamp: timestamp(message.timestamp)?,
    })
}

fn to_protocol_assistant_content(message: &AssistantMessage) -> Bridged<Vec<AssistantContent>> {
    message
        .content
        .iter()
        .map(|part| match part {
            AiAssistantContent::Text(text) => Ok(AssistantContent::Text(TextContent {
                text: text.text.clone(),
            })),
            AiAssistantContent::Thinking(thinking) => {
                Ok(AssistantContent::Thinking(ThinkingContent {
                    thinking: thinking.thinking.clone(),
                    redacted: thinking.redacted.then_some(true),
                }))
            }
            AiAssistantContent::ToolCall(call) => Ok(AssistantContent::ToolCall(ToolCallContent {
                tool_call_id: identifier(&call.id, "Tool call id")?,
                tool_name: identifier(&call.name, "Tool call name")?,
                input: to_protocol_json_value(&JsonValue::Object(call.arguments.clone()))?,
            })),
        })
        .collect()
}

pub fn to_protocol_assistant_message(
    message: &AssistantMessage,
    id: &str,
) -> Bridged<AssistantTranscriptItem> {
    let content = to_protocol_assistant_content(message)?;
    let mut item = AssistantTranscriptItem {
        id: identifier(id, "Transcript item id")?,
        role: AssistantRole::Assistant,
        content,
        model: ModelRef {
            provider: identifier(&message.provider, "Assistant provider")?,
            id: identifier(&message.model, "Assistant model")?,
        },
        response_model: match &message.response_model {
            Some(model) => Some(identifier(model, "Assistant response model")?),
            None => None,
        },
        usage: Some(to_protocol_usage(&message.usage)),
        timestamp: timestamp(message.timestamp)?,
        status: AssistantStatus::Streaming,
        stop_reason: None,
        error_message: None,
    };
    match message.stop_reason {
        StopReason::Pending => {}
        StopReason::Stop => {
            item.status = AssistantStatus::Complete;
            item.stop_reason = Some(AssistantStopReason::Stop);
        }
        StopReason::Length => {
            item.status = AssistantStatus::Complete;
            item.stop_reason = Some(AssistantStopReason::Length);
        }
        StopReason::ToolUse => {
            item.status = AssistantStatus::Complete;
            item.stop_reason = Some(AssistantStopReason::ToolUse);
        }
        StopReason::Deferred => {
            return fail("Deferred assistant messages are not supported by protocol v1");
        }
        StopReason::Error => {
            if message.error_message.as_ref().is_some_and(String::is_empty) {
                return fail("Assistant error messages must not be empty");
            }
            item.status = AssistantStatus::Error;
            item.stop_reason = Some(AssistantStopReason::Error);
            item.error_message = message.error_message.clone();
        }
        StopReason::Aborted => {
            item.status = AssistantStatus::Aborted;
            item.stop_reason = Some(AssistantStopReason::Aborted);
            item.error_message = message.error_message.clone();
        }
    }
    Ok(item)
}

fn to_protocol_tool_content(content: &[InputContent]) -> Vec<ToolContent> {
    content
        .iter()
        .map(|part| match part {
            InputContent::Text(text) => ToolContent::Text(TextContent {
                text: text.text.clone(),
            }),
            InputContent::Image(image) => ToolContent::Image(ImageContent {
                data: image.data.clone(),
                mime_type: image.mime_type.clone(),
            }),
        })
        .collect()
}

pub fn to_protocol_tool_result_message(
    message: &ToolResultMessage,
    id: &str,
    call: &ToolCall,
) -> Bridged<ToolTranscriptItem> {
    let call_id = identifier(&call.id, "Tool call id")?;
    let call_name = identifier(&call.name, "Tool call name")?;
    if identifier(&message.tool_call_id, "Tool result call id")? != call_id {
        return fail(format!(
            "Tool result {} does not match tool call {call_id}",
            message.tool_call_id
        ));
    }
    if identifier(&message.tool_name, "Tool result name")? != call_name {
        return fail(format!(
            "Tool result {} does not match tool call {call_id}",
            message.tool_name
        ));
    }
    Ok(ToolTranscriptItem {
        id: identifier(id, "Transcript item id")?,
        role: ToolRole::Tool,
        tool_call_id: call_id,
        tool_name: call_name,
        input: to_protocol_json_value(&JsonValue::Object(call.arguments.clone()))?,
        content: to_protocol_tool_content(&message.content),
        details: message.details.as_ref().map(sanitize_protocol_details),
        usage: message.usage.as_ref().map(to_protocol_usage),
        timestamp: timestamp(message.timestamp)?,
        status: if message.is_error {
            ToolStatus::Error
        } else {
            ToolStatus::Complete
        },
        is_error: message.is_error,
    })
}

/// Convenience for callers that only hold the text/image halves of a tool
/// result, matching upstream's `AiTextContent | AiImageContent` union.
pub fn tool_content_from_parts(
    text: &[AiTextContent],
    images: &[AiImageContent],
) -> Vec<ToolContent> {
    text.iter()
        .map(|part| {
            ToolContent::Text(TextContent {
                text: part.text.clone(),
            })
        })
        .chain(images.iter().map(|part| {
            ToolContent::Image(ImageContent {
                data: part.data.clone(),
                mime_type: part.mime_type.clone(),
            })
        }))
        .collect()
}
