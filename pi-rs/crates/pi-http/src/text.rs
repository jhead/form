//! Text extraction from message content.
//! Port of `packages/ai/src/utils/text.ts`.
//!
//! Rust has no default arguments, so the separator is always explicit. Upstream
//! defaults to `"\n"`; [`DEFAULT_SEPARATOR`] is that value.
//!
//! Note this differs from [`pi_core::AssistantMessage::text`], which joins with
//! the empty string because it reassembles one streamed message.

use pi_core::content::{AssistantContent, InputContent};
use pi_core::message::{Message, UserContent};

pub const DEFAULT_SEPARATOR: &str = "\n";

/// Join the text blocks of assistant content, skipping thinking and tool calls.
pub fn assistant_content_text(content: &[AssistantContent], separator: &str) -> String {
    content
        .iter()
        .filter_map(|block| block.as_text().map(|t| t.text.as_str()))
        .collect::<Vec<_>>()
        .join(separator)
}

/// Join the text blocks of user / tool-result content, skipping images.
pub fn input_content_text(content: &[InputContent], separator: &str) -> String {
    content
        .iter()
        .filter_map(|block| block.as_text().map(|t| t.text.as_str()))
        .collect::<Vec<_>>()
        .join(separator)
}

/// A bare string passes through; block content is joined.
pub fn user_content_text(content: &UserContent, separator: &str) -> String {
    match content {
        UserContent::Text(text) => text.clone(),
        UserContent::Blocks(blocks) => input_content_text(blocks, separator),
    }
}

/// Text of any message, whatever its role.
pub fn message_text(message: &Message, separator: &str) -> String {
    match message {
        Message::User(m) => user_content_text(&m.content, separator),
        Message::Assistant(m) => assistant_content_text(&m.content, separator),
        Message::ToolResult(m) => input_content_text(&m.content, separator),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::content::ToolCall;
    use pi_core::message::{now_ms, ToolResultMessage, UserMessage};

    fn assistant_content() -> Vec<AssistantContent> {
        vec![
            AssistantContent::thinking("reasoning"),
            AssistantContent::text("first"),
            AssistantContent::ToolCall(ToolCall::new("1", "read")),
            AssistantContent::text("second"),
        ]
    }

    #[test]
    fn extracts_assistant_text_blocks() {
        assert_eq!(
            assistant_content_text(&assistant_content(), DEFAULT_SEPARATOR),
            "first\nsecond"
        );
    }

    #[test]
    fn supports_custom_separators() {
        assert_eq!(
            assistant_content_text(&assistant_content(), ""),
            "firstsecond"
        );
    }

    #[test]
    fn passes_string_content_through() {
        assert_eq!(
            user_content_text(&UserContent::Text("hello".into()), DEFAULT_SEPARATOR),
            "hello"
        );
    }

    #[test]
    fn extracts_text_from_tool_result_content() {
        let content = vec![
            InputContent::text("first"),
            InputContent::image("...", "image/png"),
            InputContent::text("second"),
        ];
        assert_eq!(input_content_text(&content, ""), "firstsecond");
    }

    #[test]
    fn message_text_covers_every_role() {
        let user = Message::User(UserMessage::text("hi"));
        assert_eq!(message_text(&user, DEFAULT_SEPARATOR), "hi");

        let tool = Message::ToolResult(ToolResultMessage::text("id", "read", "body", false));
        assert_eq!(message_text(&tool, DEFAULT_SEPARATOR), "body");

        let mut assistant = pi_core::message::AssistantMessage::pending("a", "p", "m");
        assistant.content = assistant_content();
        assistant.timestamp = now_ms();
        assert_eq!(
            message_text(&Message::Assistant(assistant), "|"),
            "first|second"
        );
    }

    #[test]
    fn empty_content_yields_an_empty_string() {
        assert_eq!(assistant_content_text(&[], DEFAULT_SEPARATOR), "");
        assert_eq!(input_content_text(&[], DEFAULT_SEPARATOR), "");
    }
}
