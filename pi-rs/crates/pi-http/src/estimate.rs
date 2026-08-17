//! Context token estimation. Port of `packages/ai/src/utils/estimate.ts`.
//!
//! A crude four-characters-per-token heuristic, used to decide when to compact
//! and to size `max_tokens`. Accuracy is not the point: the estimate must be
//! *cheap* and must never under-report by much.
//!
//! The one subtle rule is which assistant `usage` block may be trusted. Usage
//! describes the prefix the provider actually saw; if a message with a newer
//! timestamp was inserted *before* an assistant response (which is what
//! compaction does when it splices a summary in), that response's usage no
//! longer describes the current prefix and has to be ignored.

use pi_core::content::{AssistantContent, InputContent};
use pi_core::message::{Message, StopReason, Usage, UserContent};
use pi_core::tool::{Context, Tool};
use serde::{Deserialize, Serialize};

const CHARS_PER_TOKEN: i64 = 4;
const ESTIMATED_IMAGE_CHARS: i64 = 4800;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsageEstimate {
    /// Estimated total context tokens.
    pub tokens: i64,
    /// Tokens reported by the most recent applicable assistant usage block.
    pub usage_tokens: i64,
    /// Estimated tokens after that usage block.
    pub trailing_tokens: i64,
    /// Index of the message that provided usage, or `None` when none applies.
    pub last_usage_index: Option<usize>,
}

/// Total context tokens a usage record represents.
///
/// Thin alias for [`Usage::context_tokens`], which is canonical — it reads
/// nothing but the usage record, so it lives in `pi-core` where every crate can
/// reach it. The name is kept because it is upstream's (`calculateContextTokens`).
pub fn calculate_context_tokens(usage: &Usage) -> i64 {
    usage.context_tokens()
}

/// JS `String.length`, i.e. UTF-16 code units — the unit upstream counts in.
fn char_len(text: &str) -> i64 {
    text.encode_utf16().count() as i64
}

/// Ceiling division. `i64::div_ceil` is still unstable, and these counts are
/// always non-negative (they come from string lengths, not from `Usage`).
fn ceil_div(chars: i64, per_token: i64) -> i64 {
    debug_assert!(chars >= 0 && per_token > 0);
    (chars + per_token - 1) / per_token
}

fn safe_json_len<T: Serialize>(value: &T) -> i64 {
    // Upstream falls back to the literal `"[unserializable]"` and measures it,
    // which is 16 characters. Unreachable here — serializing a `Map` or a
    // `Vec<Tool>` cannot fail — but match the number rather than carry a
    // comment that disagrees with the constant beside it.
    const UNSERIALIZABLE_LEN: i64 = "[unserializable]".len() as i64;
    serde_json::to_string(value)
        .map(|s| char_len(&s))
        .unwrap_or(UNSERIALIZABLE_LEN)
}

pub fn estimate_text_tokens(text: &str) -> i64 {
    ceil_div(char_len(text), CHARS_PER_TOKEN)
}

fn input_content_chars(content: &[InputContent]) -> i64 {
    content
        .iter()
        .map(|block| match block {
            InputContent::Text(t) => char_len(&t.text),
            InputContent::Image(_) => ESTIMATED_IMAGE_CHARS,
        })
        .sum()
}

pub fn estimate_input_content_tokens(content: &[InputContent]) -> i64 {
    ceil_div(input_content_chars(content), CHARS_PER_TOKEN)
}

fn user_content_chars(content: &UserContent) -> i64 {
    match content {
        UserContent::Text(text) => char_len(text),
        UserContent::Blocks(blocks) => input_content_chars(blocks),
    }
}

pub fn estimate_message_tokens(message: &Message) -> i64 {
    let chars = match message {
        Message::User(m) => user_content_chars(&m.content),
        Message::ToolResult(m) => input_content_chars(&m.content),
        Message::Assistant(m) => m
            .content
            .iter()
            .map(|block| match block {
                AssistantContent::Text(t) => char_len(&t.text),
                AssistantContent::Thinking(t) => char_len(&t.thinking),
                AssistantContent::ToolCall(c) => char_len(&c.name) + safe_json_len(&c.arguments),
            })
            .sum(),
    };
    ceil_div(chars, CHARS_PER_TOKEN)
}

/// The last assistant usage that still describes the current prefix.
fn last_assistant_usage(messages: &[Message]) -> Option<(Usage, usize)> {
    let mut latest_prefix_timestamp = i64::MIN;
    let mut found: Option<(Usage, usize)> = None;

    for (index, message) in messages.iter().enumerate() {
        if let Message::Assistant(assistant) = message {
            let applies = assistant.timestamp >= latest_prefix_timestamp;
            if applies
                && assistant.stop_reason != StopReason::Aborted
                && assistant.stop_reason != StopReason::Error
                && calculate_context_tokens(&assistant.usage) > 0
            {
                found = Some((assistant.usage.clone(), index));
            }
        }
        latest_prefix_timestamp = latest_prefix_timestamp.max(message.timestamp());
    }

    found
}

/// Estimate over a bare message list, with no system prompt or tools.
pub fn estimate_messages(messages: &[Message]) -> ContextUsageEstimate {
    if let Some((usage, index)) = last_assistant_usage(messages) {
        let usage_tokens = calculate_context_tokens(&usage);
        let trailing_tokens: i64 = messages[index + 1..]
            .iter()
            .map(estimate_message_tokens)
            .sum();
        return ContextUsageEstimate {
            tokens: usage_tokens + trailing_tokens,
            usage_tokens,
            trailing_tokens,
            last_usage_index: Some(index),
        };
    }

    let tokens: i64 = messages.iter().map(estimate_message_tokens).sum();
    ContextUsageEstimate {
        tokens,
        usage_tokens: 0,
        trailing_tokens: tokens,
        last_usage_index: None,
    }
}

fn estimate_tools_tokens(tools: &[Tool]) -> i64 {
    if tools.is_empty() {
        return 0;
    }
    ceil_div(safe_json_len(&tools), CHARS_PER_TOKEN)
}

/// Estimate the tokens a whole [`Context`] will occupy.
///
/// When a usable assistant usage block exists, the system prompt and tool
/// definitions are already counted inside it — only tools that became available
/// *after* it (via `ToolResultMessage::added_tool_names`) are added on top.
pub fn estimate_context_tokens(context: &Context) -> ContextUsageEstimate {
    let estimate = estimate_messages(&context.messages);

    if let Some(last_index) = estimate.last_usage_index {
        let added_names: Vec<&str> = context.messages[last_index + 1..]
            .iter()
            .filter_map(|m| m.as_tool_result())
            .filter_map(|m| m.added_tool_names.as_ref())
            .flatten()
            .map(String::as_str)
            .collect();
        let added_tools: Vec<Tool> = context
            .tools()
            .iter()
            .filter(|tool| added_names.contains(&tool.name.as_str()))
            .cloned()
            .collect();
        let added_tool_tokens = estimate_tools_tokens(&added_tools);
        return ContextUsageEstimate {
            tokens: estimate.tokens + added_tool_tokens,
            usage_tokens: estimate.usage_tokens,
            trailing_tokens: estimate.trailing_tokens + added_tool_tokens,
            last_usage_index: estimate.last_usage_index,
        };
    }

    let prefix_tokens = context
        .system_prompt
        .as_deref()
        .map(estimate_text_tokens)
        .unwrap_or(0)
        + estimate_tools_tokens(context.tools());

    ContextUsageEstimate {
        tokens: estimate.tokens + prefix_tokens,
        usage_tokens: estimate.usage_tokens,
        trailing_tokens: estimate.trailing_tokens + prefix_tokens,
        last_usage_index: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::content::ToolCall;
    use pi_core::message::{AssistantMessage, ToolResultMessage, UserMessage};
    use serde_json::json;

    fn usage(total: i64) -> Usage {
        Usage {
            input: total,
            total_tokens: total,
            ..Default::default()
        }
    }

    fn user(text: &str, timestamp: i64) -> Message {
        Message::User(UserMessage {
            content: UserContent::Text(text.to_string()),
            timestamp,
        })
    }

    fn assistant(timestamp: i64, total: i64) -> Message {
        let mut message = AssistantMessage::pending("openai-responses", "openai", "test-model");
        message.content = vec![AssistantContent::text("kept")];
        message.usage = usage(total);
        message.stop_reason = StopReason::Stop;
        message.timestamp = timestamp;
        Message::Assistant(message)
    }

    #[test]
    fn text_tokens_round_up() {
        assert_eq!(estimate_text_tokens(""), 0);
        assert_eq!(estimate_text_tokens("a"), 1);
        assert_eq!(estimate_text_tokens("abcd"), 1);
        assert_eq!(estimate_text_tokens("abcde"), 2);
        assert_eq!(estimate_text_tokens("system"), 2);
    }

    #[test]
    fn total_tokens_wins_over_the_component_sum() {
        let mut u = Usage {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
            total_tokens: 100,
            ..Default::default()
        };
        assert_eq!(calculate_context_tokens(&u), 100);
        u.total_tokens = 0;
        assert_eq!(calculate_context_tokens(&u), 10);
    }

    #[test]
    fn images_count_as_a_fixed_character_budget() {
        let content = vec![
            InputContent::text("abcd"),
            InputContent::image("...", "image/png"),
        ];
        assert_eq!(
            estimate_input_content_tokens(&content),
            ceil_div(4 + 4800, 4)
        );
    }

    #[test]
    fn assistant_tool_calls_count_name_plus_serialized_arguments() {
        let mut call = ToolCall::new("1", "read");
        call.arguments = json!({ "path": "a.txt" }).as_object().cloned().unwrap();
        let mut message = AssistantMessage::pending("a", "p", "m");
        message.content = vec![AssistantContent::ToolCall(call)];
        // "read" (4) + {"path":"a.txt"} (16) = 20 chars.
        assert_eq!(estimate_message_tokens(&Message::Assistant(message)), 5);
    }

    #[test]
    fn thinking_blocks_are_counted() {
        let mut message = AssistantMessage::pending("a", "p", "m");
        message.content = vec![AssistantContent::thinking("12345678")];
        assert_eq!(estimate_message_tokens(&Message::Assistant(message)), 2);
    }

    /// Upstream `context-estimate.test.ts`, case 1.
    #[test]
    fn ignores_stale_assistant_usage_after_a_newer_message_is_inserted_before_it() {
        let context = Context {
            system_prompt: Some("system".to_string()),
            messages: vec![
                user("summary", 200),
                assistant(100, 9_500),
                user(&"x".repeat(4_000), 300),
            ],
            tools: None,
        };
        assert_eq!(
            estimate_context_tokens(&context),
            ContextUsageEstimate {
                tokens: 1_005,
                usage_tokens: 0,
                trailing_tokens: 1_005,
                last_usage_index: None,
            }
        );
    }

    /// Upstream `context-estimate.test.ts`, case 2.
    #[test]
    fn uses_assistant_usage_again_after_a_response_to_the_inserted_context() {
        let context = Context {
            system_prompt: None,
            messages: vec![
                user("summary", 200),
                assistant(100, 9_500),
                user("new prompt", 300),
                assistant(400, 2_000),
                user("tail", 500),
            ],
            tools: None,
        };
        assert_eq!(
            estimate_context_tokens(&context),
            ContextUsageEstimate {
                tokens: 2_001,
                usage_tokens: 2_000,
                trailing_tokens: 1,
                last_usage_index: Some(3),
            }
        );
    }

    #[test]
    fn errored_and_aborted_responses_never_supply_usage() {
        for stop_reason in [StopReason::Error, StopReason::Aborted] {
            let Message::Assistant(mut a) = assistant(100, 9_500) else {
                unreachable!()
            };
            a.stop_reason = stop_reason;
            let context = Context::new(vec![user("hi", 50), Message::Assistant(a)]);
            assert_eq!(estimate_context_tokens(&context).last_usage_index, None);
        }
    }

    #[test]
    fn tools_are_counted_in_the_prefix_when_no_usage_applies() {
        let tools = vec![Tool::no_params("read", "Read a file")];
        let without = estimate_context_tokens(&Context::new(vec![user("hi", 1)]));
        let with = estimate_context_tokens(&Context::new(vec![user("hi", 1)]).with_tools(tools));
        assert!(with.tokens > without.tokens);
    }

    #[test]
    fn only_newly_added_tools_are_charged_after_a_usage_block() {
        let tools = vec![
            Tool::no_params("read", "Read a file"),
            Tool::no_params("write", "Write a file"),
        ];
        let mut result = ToolResultMessage::text("1", "read", "ok", false);
        result.timestamp = 500;
        result.added_tool_names = Some(vec!["write".to_string()]);

        let context = Context {
            system_prompt: Some("system prompt that is long enough to matter".to_string()),
            messages: vec![
                user("hi", 100),
                assistant(200, 1_000),
                Message::ToolResult(result),
            ],
            tools: Some(tools.clone()),
        };
        let estimate = estimate_context_tokens(&context);
        assert_eq!(estimate.last_usage_index, Some(1));
        // Only "write" is charged, and the system prompt is already inside usage.
        let only_write = estimate_tools_tokens(&tools[1..]);
        assert_eq!(
            estimate.trailing_tokens,
            estimate_message_tokens(&context.messages[2]) + only_write
        );
    }

    #[test]
    fn an_empty_context_is_zero() {
        assert_eq!(
            estimate_context_tokens(&Context::default()),
            ContextUsageEstimate::default()
        );
    }
}
