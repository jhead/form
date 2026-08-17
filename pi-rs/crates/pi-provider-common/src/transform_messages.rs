//! Port of `api/transform-messages.ts`.
//!
//! Normalizes a conversation before it is converted to a provider wire format:
//! downgrades images for text-only models, drops or flattens thinking blocks
//! that cannot be replayed, rewrites tool-call ids, skips errored/aborted
//! assistant turns, and synthesizes tool results for orphaned tool calls.
//!
//! # Consolidated from four copies
//!
//! All four adapter crates carried one. They agreed on behaviour with a single
//! exception: the id-normalizer callback. Upstream's is
//! `(id, model, source) => string` — the Responses adapter needs the *source*
//! assistant message to decide whether an item id can be replayed at all — and
//! only the OpenAI copy had all three parameters. Google, misc and Anthropic
//! had narrowed it to `(id) => string`, which is enough for what those three do
//! but cannot express what Responses needs. The three-parameter form is
//! upstream's and is what survives; the narrower callers pass a closure that
//! ignores the extra arguments.
//!
//! Upstream also maps `content == null` to `[]` up front, for histories written
//! by untyped callers. `Message::content` is not optional in this port, so
//! there is nothing to normalize and that step has no Rust equivalent.

use std::collections::{HashMap, HashSet};

use pi_core::content::{AssistantContent, TextContent};
use pi_core::message::{now_ms, Message, ToolResultMessage, UserContent};
use pi_core::{AssistantMessage, InputContent, Model, StopReason, ToolCall};

const NON_VISION_USER_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str =
    "(tool image omitted: model does not support images)";

/// Rewrites a tool-call id for the target model. Receives the original id and
/// the assistant message it came from (Responses needs the source provider to
/// decide whether an item id can be replayed).
pub type ToolCallIdNormalizer<'a> = &'a dyn Fn(&str, &Model, &AssistantMessage) -> String;

fn replace_images_with_placeholder(
    content: &[InputContent],
    placeholder: &str,
) -> Vec<InputContent> {
    let mut result = Vec::with_capacity(content.len());
    let mut previous_was_placeholder = false;

    for block in content {
        match block {
            InputContent::Image(_) => {
                if !previous_was_placeholder {
                    result.push(InputContent::text(placeholder));
                }
                previous_was_placeholder = true;
            }
            InputContent::Text(text) => {
                previous_was_placeholder = text.text == placeholder;
                result.push(block.clone());
            }
        }
    }

    result
}

fn downgrade_unsupported_images(messages: &[Message], model: &Model) -> Vec<Message> {
    if model.supports_images() {
        return messages.to_vec();
    }

    messages
        .iter()
        .map(|msg| match msg {
            Message::User(user) => match &user.content {
                UserContent::Blocks(blocks) => {
                    let mut next = user.clone();
                    next.content = UserContent::Blocks(replace_images_with_placeholder(
                        blocks,
                        NON_VISION_USER_IMAGE_PLACEHOLDER,
                    ));
                    Message::User(next)
                }
                UserContent::Text(_) => msg.clone(),
            },
            Message::ToolResult(result) => {
                let mut next = result.clone();
                next.content = replace_images_with_placeholder(
                    &result.content,
                    NON_VISION_TOOL_IMAGE_PLACEHOLDER,
                );
                Message::ToolResult(next)
            }
            Message::Assistant(_) => msg.clone(),
        })
        .collect()
}

/// Port of `transformMessages`.
pub fn transform_messages(
    messages: &[Message],
    model: &Model,
    normalize_tool_call_id: Option<ToolCallIdNormalizer<'_>>,
) -> Vec<Message> {
    let mut tool_call_id_map: HashMap<String, String> = HashMap::new();
    let image_aware = downgrade_unsupported_images(messages, model);

    // First pass: image downgrade already applied; now thinking blocks and
    // tool-call id normalization.
    let mut transformed: Vec<Message> = Vec::with_capacity(image_aware.len());
    for msg in image_aware {
        match msg {
            Message::User(_) => transformed.push(msg),
            Message::ToolResult(ref result) => match tool_call_id_map.get(&result.tool_call_id) {
                Some(normalized) if normalized != &result.tool_call_id => {
                    let mut next = result.clone();
                    next.tool_call_id = normalized.clone();
                    transformed.push(Message::ToolResult(next));
                }
                _ => transformed.push(msg),
            },
            Message::Assistant(assistant) => {
                let is_same_model = assistant.provider == model.provider
                    && assistant.api == model.api.as_str()
                    && assistant.model == model.id;

                let mut content = Vec::with_capacity(assistant.content.len());
                for block in &assistant.content {
                    match block {
                        AssistantContent::Thinking(thinking) => {
                            // Redacted thinking is opaque encrypted content valid only for the
                            // same model; drop it cross-model to avoid API errors.
                            if thinking.redacted {
                                if is_same_model {
                                    content.push(block.clone());
                                }
                                continue;
                            }
                            // Same model: keep signed thinking even when the text is empty
                            // (OpenAI encrypted reasoning).
                            if is_same_model && thinking.thinking_signature.is_some() {
                                content.push(block.clone());
                                continue;
                            }
                            if thinking.thinking.trim().is_empty() {
                                continue;
                            }
                            if is_same_model {
                                content.push(block.clone());
                            } else {
                                content.push(AssistantContent::text(thinking.thinking.clone()));
                            }
                        }
                        AssistantContent::Text(text) => {
                            if is_same_model {
                                content.push(block.clone());
                            } else {
                                content.push(AssistantContent::Text(TextContent {
                                    text: text.text.clone(),
                                    text_signature: None,
                                }));
                            }
                        }
                        AssistantContent::ToolCall(tool_call) => {
                            let mut normalized = tool_call.clone();
                            if !is_same_model && normalized.thought_signature.is_some() {
                                normalized.thought_signature = None;
                            }
                            if !is_same_model {
                                if let Some(normalize) = normalize_tool_call_id {
                                    let new_id = normalize(&tool_call.id, model, &assistant);
                                    if new_id != tool_call.id {
                                        tool_call_id_map
                                            .insert(tool_call.id.clone(), new_id.clone());
                                        normalized.id = new_id;
                                    }
                                }
                            }
                            content.push(AssistantContent::ToolCall(normalized));
                        }
                    }
                }

                let mut next = assistant.clone();
                next.content = content;
                transformed.push(Message::Assistant(next));
            }
        }
    }

    // Second pass: insert synthetic tool results for orphaned tool calls. This
    // keeps thinking signatures replayable and satisfies provider validation.
    let mut result: Vec<Message> = Vec::with_capacity(transformed.len());
    let mut pending_tool_calls: Vec<ToolCall> = Vec::new();
    let mut existing_tool_result_ids: HashSet<String> = HashSet::new();

    fn flush(
        result: &mut Vec<Message>,
        pending: &mut Vec<ToolCall>,
        existing: &mut HashSet<String>,
    ) {
        if pending.is_empty() {
            return;
        }
        for call in pending.iter() {
            if !existing.contains(&call.id) {
                result.push(Message::ToolResult(ToolResultMessage {
                    tool_call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    content: vec![InputContent::text("No result provided")],
                    details: None,
                    usage: None,
                    added_tool_names: None,
                    is_error: true,
                    timestamp: now_ms(),
                }));
            }
        }
        pending.clear();
        existing.clear();
    }

    for msg in transformed {
        match &msg {
            Message::Assistant(assistant) => {
                flush(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                );

                // Errored/aborted assistant turns are incomplete and must not be
                // replayed: they can carry reasoning without a following item,
                // which several providers reject outright.
                if matches!(
                    assistant.stop_reason,
                    StopReason::Error | StopReason::Aborted
                ) {
                    continue;
                }

                let tool_calls: Vec<ToolCall> = assistant.tool_calls().cloned().collect();
                if !tool_calls.is_empty() {
                    pending_tool_calls = tool_calls;
                    existing_tool_result_ids = HashSet::new();
                }
                result.push(msg);
            }
            Message::ToolResult(tool_result) => {
                existing_tool_result_ids.insert(tool_result.tool_call_id.clone());
                result.push(msg);
            }
            Message::User(_) => {
                flush(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                );
                result.push(msg);
            }
        }
    }

    flush(
        &mut result,
        &mut pending_tool_calls,
        &mut existing_tool_result_ids,
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::message::UserMessage;
    use pi_core::{Api, Modality};
    use serde_json::Map;

    fn model() -> Model {
        Model::new("m1", Api::OpenAiCompletions, "prov", "https://x")
    }

    fn assistant_with(content: Vec<AssistantContent>, stop: StopReason) -> AssistantMessage {
        let mut msg = AssistantMessage::pending("openai-completions", "prov", "m1");
        msg.content = content;
        msg.stop_reason = stop;
        msg
    }

    #[test]
    fn drops_images_for_text_only_models_collapsing_runs() {
        let mut user = UserMessage::text("");
        user.content = UserContent::Blocks(vec![
            InputContent::image("a", "image/png"),
            InputContent::image("b", "image/png"),
            InputContent::text("caption"),
        ]);
        let out = transform_messages(&[Message::User(user)], &model(), None);
        let Message::User(user) = &out[0] else {
            panic!()
        };
        let UserContent::Blocks(blocks) = &user.content else {
            panic!()
        };
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            blocks[0].as_text().unwrap().text,
            NON_VISION_USER_IMAGE_PLACEHOLDER
        );
    }

    #[test]
    fn keeps_images_for_vision_models() {
        let mut m = model();
        m.input = vec![Modality::Text, Modality::Image];
        let mut user = UserMessage::text("");
        user.content = UserContent::Blocks(vec![InputContent::image("a", "image/png")]);
        let out = transform_messages(&[Message::User(user)], &m, None);
        let Message::User(user) = &out[0] else {
            panic!()
        };
        let UserContent::Blocks(blocks) = &user.content else {
            panic!()
        };
        assert!(matches!(blocks[0], InputContent::Image(_)));
    }

    #[test]
    fn errored_assistant_turns_are_skipped() {
        let messages = vec![
            Message::User(UserMessage::text("hi")),
            Message::Assistant(assistant_with(
                vec![AssistantContent::text("partial")],
                StopReason::Error,
            )),
            Message::User(UserMessage::text("again")),
        ];
        let out = transform_messages(&messages, &model(), None);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|m| m.as_assistant().is_none()));
    }

    #[test]
    fn orphaned_tool_calls_get_synthetic_results() {
        let messages = vec![Message::Assistant(assistant_with(
            vec![AssistantContent::ToolCall(ToolCall::new("call_1", "bash"))],
            StopReason::ToolUse,
        ))];
        let out = transform_messages(&messages, &model(), None);
        assert_eq!(out.len(), 2);
        let result = out[1].as_tool_result().unwrap();
        assert_eq!(result.tool_call_id, "call_1");
        assert!(result.is_error);
    }

    #[test]
    fn cross_model_thinking_becomes_text_and_ids_are_rewritten() {
        let mut foreign = assistant_with(
            vec![
                AssistantContent::thinking("deep thoughts"),
                AssistantContent::ToolCall(ToolCall::new("call_LONG", "bash")),
            ],
            StopReason::ToolUse,
        );
        foreign.provider = "other".into();
        let messages = vec![
            Message::Assistant(foreign),
            Message::ToolResult(ToolResultMessage::text("call_LONG", "bash", "ok", false)),
        ];
        let normalize = |id: &str, _m: &Model, _s: &AssistantMessage| format!("n_{id}");
        let out = transform_messages(&messages, &model(), Some(&normalize));
        let assistant = out[0].as_assistant().unwrap();
        assert_eq!(
            assistant.content[0].as_text().unwrap().text,
            "deep thoughts"
        );
        assert_eq!(
            assistant.content[1].as_tool_call().unwrap().id,
            "n_call_LONG"
        );
        assert_eq!(out[1].as_tool_result().unwrap().tool_call_id, "n_call_LONG");
    }

    // --- cases carried over from the google, misc and anthropic copies ---

    /// From the google copy: the placeholder must land *before* the trailing
    /// text, not be appended after the surviving blocks.
    #[test]
    fn image_placeholder_keeps_its_position_in_the_block_run() {
        let mut m = model();
        m.input = vec![Modality::Text];
        let mut user = UserMessage::text("");
        user.content = UserContent::Blocks(vec![
            InputContent::image("abc", "image/png"),
            InputContent::image("def", "image/png"),
            InputContent::text("after"),
        ]);
        let out = transform_messages(&[Message::User(user)], &m, None);
        let UserContent::Blocks(blocks) = &out[0].as_user().unwrap().content else {
            panic!("expected blocks")
        };
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            blocks[0].as_text().unwrap().text,
            NON_VISION_USER_IMAGE_PLACEHOLDER
        );
        assert_eq!(blocks[1].as_text().unwrap().text, "after");
    }

    /// From the misc copy: a placeholder inserted *after* real text still
    /// collapses the run behind it.
    #[test]
    fn a_trailing_image_run_collapses_to_one_placeholder() {
        let mut m = model();
        m.input = vec![Modality::Text];
        let mut user = UserMessage::text("");
        user.content = UserContent::Blocks(vec![
            InputContent::text("look"),
            InputContent::image("aGk=", "image/png"),
            InputContent::image("aGk=", "image/png"),
        ]);
        let out = transform_messages(&[Message::User(user)], &m, None);
        let UserContent::Blocks(blocks) = &out[0].as_user().unwrap().content else {
            panic!("expected blocks")
        };
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].as_text().unwrap().text, "look");
        assert_eq!(
            blocks[1].as_text().unwrap().text,
            NON_VISION_USER_IMAGE_PLACEHOLDER
        );
    }

    #[test]
    fn tool_result_images_get_their_own_placeholder() {
        let mut m = model();
        m.input = vec![Modality::Text];
        let mut result = ToolResultMessage::text("call_1", "screenshot", "", false);
        result.content = vec![InputContent::image("aGk=", "image/png")];
        let out = transform_messages(&[Message::ToolResult(result)], &m, None);
        assert_eq!(
            out[0].as_tool_result().unwrap().content[0]
                .as_text()
                .unwrap()
                .text,
            NON_VISION_TOOL_IMAGE_PLACEHOLDER
        );
    }

    /// From the google copy: an orphaned call in the *middle* of a history
    /// still gets its synthetic result, and the user turn stays after it.
    #[test]
    fn synthetic_results_are_inserted_before_the_next_user_turn() {
        let messages = vec![
            Message::User(UserMessage::text("hi")),
            Message::Assistant(assistant_with(
                vec![AssistantContent::ToolCall(ToolCall::new("call_1", "bash"))],
                StopReason::ToolUse,
            )),
            Message::User(UserMessage::text("never mind")),
        ];
        let out = transform_messages(&messages, &model(), None);
        assert_eq!(out.len(), 4);
        let synthetic = out[2].as_tool_result().unwrap();
        assert_eq!(synthetic.tool_call_id, "call_1");
        assert!(synthetic.is_error);
        assert_eq!(
            synthetic.content[0].as_text().unwrap().text,
            "No result provided"
        );
        assert!(out[3].as_user().is_some());
    }

    #[test]
    fn a_real_tool_result_suppresses_the_synthetic_one() {
        let messages = vec![
            Message::Assistant(assistant_with(
                vec![AssistantContent::ToolCall(ToolCall::new("call_1", "bash"))],
                StopReason::ToolUse,
            )),
            Message::ToolResult(ToolResultMessage::text("call_1", "bash", "ok", false)),
        ];
        let out = transform_messages(&messages, &model(), None);
        assert_eq!(out.len(), 2);
        assert!(!out[1].as_tool_result().unwrap().is_error);
    }

    /// From the misc copy: a cross-model tool call loses its Google thought
    /// signature as well as gaining a normalized id.
    #[test]
    fn cross_model_tool_calls_drop_their_thought_signature() {
        let mut foreign = assistant_with(vec![], StopReason::ToolUse);
        foreign.provider = "openai".into();
        foreign.api = "openai-responses".into();
        foreign.content = vec![AssistantContent::ToolCall(ToolCall {
            id: "fc_long|id".into(),
            name: "lookup".into(),
            arguments: Map::new(),
            thought_signature: Some("sig".into()),
            namespace: None,
        })];
        let messages = vec![
            Message::Assistant(foreign),
            Message::ToolResult(ToolResultMessage::text("fc_long|id", "lookup", "ok", false)),
        ];
        let normalize = |_id: &str, _m: &Model, _s: &AssistantMessage| "short1234".to_string();
        let out = transform_messages(&messages, &model(), Some(&normalize));

        let call = out[0].as_assistant().unwrap().tool_calls().next().unwrap();
        assert_eq!(call.id, "short1234");
        assert_eq!(call.thought_signature, None);
        assert_eq!(out[1].as_tool_result().unwrap().tool_call_id, "short1234");
    }

    #[test]
    fn same_model_tool_calls_keep_their_id_and_signature() {
        let mut same = assistant_with(vec![], StopReason::ToolUse);
        same.content = vec![AssistantContent::ToolCall(ToolCall {
            id: "call|1".into(),
            name: "lookup".into(),
            arguments: Map::new(),
            thought_signature: Some("sig".into()),
            namespace: None,
        })];
        let normalize = |id: &str, _m: &Model, _s: &AssistantMessage| id.replace('|', "_");
        let out = transform_messages(&[Message::Assistant(same)], &model(), Some(&normalize));
        let call = out[0].as_assistant().unwrap().tool_calls().next().unwrap();
        assert_eq!(call.id, "call|1");
        assert_eq!(call.thought_signature.as_deref(), Some("sig"));
    }

    /// From the misc copy: a whole assistant turn of cross-model thinking is
    /// flattened rather than dropped.
    #[test]
    fn converts_cross_model_thinking_to_text() {
        let mut foreign = assistant_with(
            vec![AssistantContent::thinking("reasoned")],
            StopReason::Stop,
        );
        foreign.provider = "anthropic".into();
        let out = transform_messages(&[Message::Assistant(foreign)], &model(), None);
        let content = &out[0].as_assistant().unwrap().content;
        assert_eq!(content[0].as_text().unwrap().text, "reasoned");
    }

    #[test]
    fn redacted_thinking_survives_only_for_the_same_model() {
        let redacted = pi_core::content::ThinkingContent {
            thinking: String::new(),
            thinking_signature: None,
            redacted: true,
        };
        let same = assistant_with(
            vec![AssistantContent::Thinking(redacted.clone())],
            StopReason::Stop,
        );
        let out = transform_messages(&[Message::Assistant(same)], &model(), None);
        assert_eq!(out[0].as_assistant().unwrap().content.len(), 1);

        let mut foreign =
            assistant_with(vec![AssistantContent::Thinking(redacted)], StopReason::Stop);
        foreign.provider = "other".into();
        let out = transform_messages(&[Message::Assistant(foreign)], &model(), None);
        assert!(out[0].as_assistant().unwrap().content.is_empty());
    }

    #[test]
    fn empty_thinking_is_dropped_unless_it_carries_a_signature() {
        let signed = pi_core::content::ThinkingContent {
            thinking: String::new(),
            thinking_signature: Some("sig".into()),
            redacted: false,
        };
        let same = assistant_with(
            vec![
                AssistantContent::Thinking(signed),
                AssistantContent::thinking("   "),
            ],
            StopReason::Stop,
        );
        let out = transform_messages(&[Message::Assistant(same)], &model(), None);
        let content = &out[0].as_assistant().unwrap().content;
        assert_eq!(content.len(), 1);
        assert!(matches!(content[0], AssistantContent::Thinking(_)));
    }

    #[test]
    fn aborted_assistant_turns_are_skipped_too() {
        let messages = vec![Message::Assistant(assistant_with(
            vec![AssistantContent::text("partial")],
            StopReason::Aborted,
        ))];
        assert!(transform_messages(&messages, &model(), None).is_empty());
    }
}
