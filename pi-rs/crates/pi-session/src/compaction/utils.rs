//! Shared compaction helpers. Port of `harness/compaction/utils.ts`.

use std::collections::BTreeSet;

use pi_core::{AssistantContent, InputContent, Message, UserContent};

use crate::messages::AgentMessage;

/// File paths touched by a session branch or compaction range.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileOperations {
    /// Files read but not necessarily modified.
    pub read: BTreeSet<String>,
    /// Files written by full-file write operations.
    pub written: BTreeSet<String>,
    /// Files modified by edit operations.
    pub edited: BTreeSet<String>,
}

pub fn create_file_ops() -> FileOperations {
    FileOperations::default()
}

/// Accumulate file operations from an assistant message's tool calls.
pub fn extract_file_ops_from_message(message: &AgentMessage, file_ops: &mut FileOperations) {
    let AgentMessage::Assistant(assistant) = message else {
        return;
    };
    for call in assistant.tool_calls() {
        let Some(path) = call.arguments.get("path").and_then(|value| value.as_str()) else {
            continue;
        };
        match call.name.as_str() {
            "read" => {
                file_ops.read.insert(path.to_string());
            }
            "write" => {
                file_ops.written.insert(path.to_string());
            }
            "edit" => {
                file_ops.edited.insert(path.to_string());
            }
            _ => {}
        }
    }
}

/// Sorted read-only and modified file lists. A file that was both read and
/// modified counts only as modified.
pub fn compute_file_lists(file_ops: &FileOperations) -> (Vec<String>, Vec<String>) {
    let modified: BTreeSet<&String> = file_ops
        .edited
        .iter()
        .chain(file_ops.written.iter())
        .collect();
    let read_files: Vec<String> = file_ops
        .read
        .iter()
        .filter(|path| !modified.contains(*path))
        .cloned()
        .collect();
    let modified_files: Vec<String> = modified.into_iter().cloned().collect();
    (read_files, modified_files)
}

/// Format file lists as the summary metadata tags appended to a summary.
pub fn format_file_operations(read_files: &[String], modified_files: &[String]) -> String {
    let mut sections: Vec<String> = Vec::new();
    if !read_files.is_empty() {
        sections.push(format!(
            "<read-files>\n{}\n</read-files>",
            read_files.join("\n")
        ));
    }
    if !modified_files.is_empty() {
        sections.push(format!(
            "<modified-files>\n{}\n</modified-files>",
            modified_files.join("\n")
        ));
    }
    if sections.is_empty() {
        return String::new();
    }
    format!("\n\n{}", sections.join("\n\n"))
}

const TOOL_RESULT_MAX_CHARS: usize = 2000;

fn safe_json_stringify(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[unserializable]".into())
}

fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    // Upstream measures in UTF-16 code units; chars is the closest sane Rust
    // equivalent and matches for the ASCII-dominant content this sees.
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars).collect();
    format!(
        "{head}\n\n[... {} more characters truncated]",
        count - max_chars
    )
}

fn content_text(content: &[InputContent], separator: &str) -> String {
    content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.as_str()))
        .collect::<Vec<_>>()
        .join(separator)
}

fn user_content_text(content: &UserContent, separator: &str) -> String {
    match content {
        UserContent::Text(text) => text.clone(),
        UserContent::Blocks(blocks) => content_text(blocks, separator),
    }
}

fn assistant_content_text(content: &[AssistantContent], separator: &str) -> String {
    content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.as_str()))
        .collect::<Vec<_>>()
        .join(separator)
}

/// Serialize LLM messages to plain text for a summarization prompt.
pub fn serialize_conversation(messages: &[Message]) -> String {
    let mut parts: Vec<String> = Vec::new();

    for message in messages {
        match message {
            Message::User(user) => {
                let content = user_content_text(&user.content, "");
                if !content.is_empty() {
                    parts.push(format!("[User]: {content}"));
                }
            }
            Message::Assistant(assistant) => {
                let mut thinking_parts: Vec<&str> = Vec::new();
                let mut tool_calls: Vec<String> = Vec::new();
                for block in &assistant.content {
                    match block {
                        AssistantContent::Thinking(thinking) => {
                            thinking_parts.push(&thinking.thinking)
                        }
                        AssistantContent::ToolCall(call) => {
                            let args = call
                                .arguments
                                .iter()
                                .map(|(key, value)| format!("{key}={}", safe_json_stringify(value)))
                                .collect::<Vec<_>>()
                                .join(", ");
                            tool_calls.push(format!("{}({args})", call.name));
                        }
                        AssistantContent::Text(_) => {}
                    }
                }
                if !thinking_parts.is_empty() {
                    parts.push(format!(
                        "[Assistant thinking]: {}",
                        thinking_parts.join("\n")
                    ));
                }
                if assistant
                    .content
                    .iter()
                    .any(|block| block.as_text().is_some())
                {
                    parts.push(format!(
                        "[Assistant]: {}",
                        assistant_content_text(&assistant.content, "\n")
                    ));
                }
                if !tool_calls.is_empty() {
                    parts.push(format!("[Assistant tool calls]: {}", tool_calls.join("; ")));
                }
            }
            Message::ToolResult(result) => {
                let content = content_text(&result.content, "");
                if !content.is_empty() {
                    parts.push(format!(
                        "[Tool result]: {}",
                        truncate_for_summary(&content, TOOL_RESULT_MAX_CHARS)
                    ));
                }
            }
        }
    }

    parts.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::{ToolCall, UserMessage};
    use serde_json::json;

    fn assistant_with_call(name: &str, path: &str) -> AgentMessage {
        let mut call = ToolCall::new("call-1", name);
        call.arguments.insert("path".into(), json!(path));
        AgentMessage::Assistant(pi_core::AssistantMessage {
            content: vec![AssistantContent::ToolCall(call)],
            ..pi_core::AssistantMessage::pending("api", "provider", "model")
        })
    }

    #[test]
    fn modified_files_win_over_reads() {
        let mut ops = create_file_ops();
        extract_file_ops_from_message(&assistant_with_call("read", "/a"), &mut ops);
        extract_file_ops_from_message(&assistant_with_call("read", "/b"), &mut ops);
        extract_file_ops_from_message(&assistant_with_call("edit", "/a"), &mut ops);
        extract_file_ops_from_message(&assistant_with_call("write", "/c"), &mut ops);
        let (read, modified) = compute_file_lists(&ops);
        assert_eq!(read, vec!["/b"]);
        assert_eq!(modified, vec!["/a", "/c"]);
    }

    #[test]
    fn formats_only_non_empty_sections() {
        assert_eq!(format_file_operations(&[], &[]), "");
        assert_eq!(
            format_file_operations(&["/a".into()], &[]),
            "\n\n<read-files>\n/a\n</read-files>"
        );
        assert_eq!(
            format_file_operations(&["/a".into()], &["/b".into()]),
            "\n\n<read-files>\n/a\n</read-files>\n\n<modified-files>\n/b\n</modified-files>"
        );
    }

    #[test]
    fn serializes_a_conversation_with_thinking_and_tool_calls() {
        let mut call = ToolCall::new("call-1", "bash");
        call.arguments.insert("cmd".into(), json!("ls"));
        let messages = vec![
            Message::User(UserMessage {
                content: UserContent::Text("hi".into()),
                timestamp: 1,
            }),
            Message::Assistant(pi_core::AssistantMessage {
                content: vec![
                    AssistantContent::thinking("pondering"),
                    AssistantContent::text("sure"),
                    AssistantContent::ToolCall(call),
                ],
                ..pi_core::AssistantMessage::pending("api", "provider", "model")
            }),
            Message::ToolResult(pi_core::ToolResultMessage::text(
                "call-1", "bash", "out", false,
            )),
        ];
        assert_eq!(
            serialize_conversation(&messages),
            "[User]: hi\n\n[Assistant thinking]: pondering\n\n[Assistant]: sure\n\n[Assistant tool calls]: bash(cmd=\"ls\")\n\n[Tool result]: out"
        );
    }

    #[test]
    fn truncates_long_tool_results() {
        let long = "x".repeat(TOOL_RESULT_MAX_CHARS + 10);
        let messages = vec![Message::ToolResult(pi_core::ToolResultMessage::text(
            "call-1", "bash", long, false,
        ))];
        let serialized = serialize_conversation(&messages);
        assert!(
            serialized.ends_with("[... 10 more characters truncated]"),
            "{serialized}"
        );
    }
}
