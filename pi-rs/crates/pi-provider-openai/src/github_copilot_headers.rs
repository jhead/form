//! Port of `api/github-copilot-headers.ts`.

use pi_core::message::{Message, UserContent};
use pi_core::InputContent;

/// Copilot wants `X-Initiator` to say whether the turn was user- or
/// agent-initiated.
pub fn infer_copilot_initiator(messages: &[Message]) -> &'static str {
    match messages.last() {
        Some(Message::User(_)) | None => "user",
        Some(_) => "agent",
    }
}

/// Copilot requires `Copilot-Vision-Request` whenever the payload carries images.
pub fn has_copilot_vision_input(messages: &[Message]) -> bool {
    messages.iter().any(|msg| match msg {
        Message::User(m) => match &m.content {
            UserContent::Blocks(blocks) => {
                blocks.iter().any(|b| matches!(b, InputContent::Image(_)))
            }
            UserContent::Text(_) => false,
        },
        Message::ToolResult(m) => m
            .content
            .iter()
            .any(|b| matches!(b, InputContent::Image(_))),
        Message::Assistant(_) => false,
    })
}

pub fn build_copilot_dynamic_headers(
    messages: &[Message],
    has_images: bool,
) -> Vec<(String, String)> {
    let mut headers = vec![
        (
            "X-Initiator".to_string(),
            infer_copilot_initiator(messages).to_string(),
        ),
        (
            "Openai-Intent".to_string(),
            "conversation-edits".to_string(),
        ),
    ];
    if has_images {
        headers.push(("Copilot-Vision-Request".to_string(), "true".to_string()));
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::message::{ToolResultMessage, UserMessage};

    #[test]
    fn initiator_is_agent_after_a_tool_result() {
        let messages = vec![
            Message::User(UserMessage::text("hi")),
            Message::ToolResult(ToolResultMessage::text("1", "bash", "ok", false)),
        ];
        assert_eq!(infer_copilot_initiator(&messages), "agent");
    }

    #[test]
    fn vision_header_tracks_tool_result_images() {
        let mut result = ToolResultMessage::text("1", "screenshot", "", false);
        result.content = vec![InputContent::image("AAA", "image/png")];
        assert!(has_copilot_vision_input(&[Message::ToolResult(result)]));
    }
}
