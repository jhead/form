//! `AgentMessage` — the message union sessions persist.
//!
//! Port of `harness/messages.ts`. Upstream declares `AgentMessage` in
//! `packages/agent/src/types.ts` as `Message | CustomAgentMessages[...]` and
//! grows the union through TypeScript declaration merging in `messages.ts`.
//! Rust has no declaration merging, so the closed union lives here — in the
//! crate that owns the durable format, because `Entry::Message` persists it.
//! `pi-agent` (W11) re-exports this rather than defining its own.

use pi_core::{
    now_ms, AssistantMessage, InputContent, Message, ToolResultMessage, UserContent, UserMessage,
};
use serde::de::Error as _;
use serde::ser::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

pub const COMPACTION_SUMMARY_PREFIX: &str =
    "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
pub const COMPACTION_SUMMARY_SUFFIX: &str = "\n</summary>";
pub const BRANCH_SUMMARY_PREFIX: &str =
    "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n";
pub const BRANCH_SUMMARY_SUFFIX: &str = "</summary>";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashExecutionMessage {
    pub command: String,
    pub output: String,
    /// `number | undefined` upstream: the key exists but `JSON.stringify` drops
    /// it when the command was killed before exiting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
    pub timestamp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_from_context: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessage {
    pub custom_type: String,
    pub content: UserContent,
    pub display: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    pub timestamp: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryMessage {
    pub summary: String,
    pub from_id: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSummaryMessage {
    pub summary: String,
    pub tokens_before: i64,
    pub timestamp: i64,
}

/// The full agent-visible message union, discriminated by `role` on the wire.
///
/// The variants differ a lot in size, but this is a durable wire union that has
/// to mirror the TypeScript one field for field; boxing a variant would change
/// the serde shape and the ergonomics for every consumer.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum AgentMessage {
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
    BashExecution(BashExecutionMessage),
    Custom(CustomMessage),
    BranchSummary(BranchSummaryMessage),
    CompactionSummary(CompactionSummaryMessage),
}

impl AgentMessage {
    pub fn role(&self) -> &'static str {
        match self {
            AgentMessage::User(_) => "user",
            AgentMessage::Assistant(_) => "assistant",
            AgentMessage::ToolResult(_) => "toolResult",
            AgentMessage::BashExecution(_) => "bashExecution",
            AgentMessage::Custom(_) => "custom",
            AgentMessage::BranchSummary(_) => "branchSummary",
            AgentMessage::CompactionSummary(_) => "compactionSummary",
        }
    }

    pub fn timestamp(&self) -> i64 {
        match self {
            AgentMessage::User(m) => m.timestamp,
            AgentMessage::Assistant(m) => m.timestamp,
            AgentMessage::ToolResult(m) => m.timestamp,
            AgentMessage::BashExecution(m) => m.timestamp,
            AgentMessage::Custom(m) => m.timestamp,
            AgentMessage::BranchSummary(m) => m.timestamp,
            AgentMessage::CompactionSummary(m) => m.timestamp,
        }
    }

    pub fn as_assistant(&self) -> Option<&AssistantMessage> {
        match self {
            AgentMessage::Assistant(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_tool_result(&self) -> Option<&ToolResultMessage> {
        match self {
            AgentMessage::ToolResult(m) => Some(m),
            _ => None,
        }
    }

    pub fn user_text(text: impl Into<String>) -> Self {
        AgentMessage::User(UserMessage {
            content: UserContent::Text(text.into()),
            timestamp: now_ms(),
        })
    }
}

impl From<Message> for AgentMessage {
    fn from(message: Message) -> Self {
        match message {
            Message::User(m) => AgentMessage::User(m),
            Message::Assistant(m) => AgentMessage::Assistant(m),
            Message::ToolResult(m) => AgentMessage::ToolResult(m),
        }
    }
}

impl AgentMessage {
    /// The three LLM roles, for callers that need a `pi_core::Message`.
    pub fn as_llm_message(&self) -> Option<Message> {
        match self {
            AgentMessage::User(m) => Some(Message::User(m.clone())),
            AgentMessage::Assistant(m) => Some(Message::Assistant(m.clone())),
            AgentMessage::ToolResult(m) => Some(Message::ToolResult(m.clone())),
            _ => None,
        }
    }
}

fn to_object<T: Serialize>(value: &T) -> Result<Map<String, Value>, serde_json::Error> {
    match serde_json::to_value(value)? {
        Value::Object(map) => Ok(map),
        other => Err(<serde_json::Error as serde::ser::Error>::custom(format!(
            "expected a JSON object, got {other}"
        ))),
    }
}

impl Serialize for AgentMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut fields = match self {
            AgentMessage::User(m) => to_object(m),
            AgentMessage::Assistant(m) => to_object(m),
            AgentMessage::ToolResult(m) => to_object(m),
            AgentMessage::BashExecution(m) => to_object(m),
            AgentMessage::Custom(m) => to_object(m),
            AgentMessage::BranchSummary(m) => to_object(m),
            AgentMessage::CompactionSummary(m) => to_object(m),
        }
        .map_err(S::Error::custom)?;

        // `ToolResultMessage.isError` is a *required* field upstream, but
        // `pi_core` skips it when false. Re-materialize it in its declared
        // position (immediately before `timestamp`) so JSONL written here still
        // loads in the TypeScript implementation.
        if matches!(self, AgentMessage::ToolResult(_)) && !fields.contains_key("isError") {
            let timestamp = fields.shift_remove("timestamp");
            fields.insert("isError".into(), Value::Bool(false));
            if let Some(timestamp) = timestamp {
                fields.insert("timestamp".into(), timestamp);
            }
        }

        let mut map = Map::with_capacity(fields.len() + 1);
        map.insert("role".into(), Value::String(self.role().to_string()));
        map.extend(fields);
        map.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AgentMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut map = Map::<String, Value>::deserialize(deserializer)?;
        let role = match map.shift_remove("role") {
            Some(Value::String(role)) => role,
            _ => return Err(D::Error::custom("message is missing a string role")),
        };
        let value = Value::Object(map);
        let message = match role.as_str() {
            "user" => AgentMessage::User(from_value(value)?),
            "assistant" => AgentMessage::Assistant(from_value(value)?),
            "toolResult" => AgentMessage::ToolResult(from_value(value)?),
            "bashExecution" => AgentMessage::BashExecution(from_value(value)?),
            "custom" => AgentMessage::Custom(from_value(value)?),
            "branchSummary" => AgentMessage::BranchSummary(from_value(value)?),
            "compactionSummary" => AgentMessage::CompactionSummary(from_value(value)?),
            other => return Err(D::Error::custom(format!("unknown message role {other}"))),
        };
        Ok(message)
    }
}

fn from_value<T, E>(value: Value) -> Result<T, E>
where
    T: serde::de::DeserializeOwned,
    E: serde::de::Error,
{
    serde_json::from_value(value).map_err(E::custom)
}

pub fn bash_execution_to_text(msg: &BashExecutionMessage) -> String {
    let mut text = format!("Ran `{}`\n", msg.command);
    if msg.output.is_empty() {
        text.push_str("(no output)");
    } else {
        text.push_str(&format!("```\n{}\n```", msg.output));
    }
    if msg.cancelled {
        text.push_str("\n\n(command cancelled)");
    } else if let Some(code) = msg.exit_code {
        if code != 0 {
            text.push_str(&format!("\n\nCommand exited with code {code}"));
        }
    }
    if msg.truncated {
        if let Some(path) = &msg.full_output_path {
            text.push_str(&format!("\n\n[Output truncated. Full output: {path}]"));
        }
    }
    text
}

pub fn create_branch_summary_message(
    summary: impl Into<String>,
    from_id: impl Into<String>,
    timestamp: i64,
) -> BranchSummaryMessage {
    BranchSummaryMessage {
        summary: summary.into(),
        from_id: from_id.into(),
        timestamp,
    }
}

pub fn create_compaction_summary_message(
    summary: impl Into<String>,
    tokens_before: i64,
    timestamp: i64,
) -> CompactionSummaryMessage {
    CompactionSummaryMessage {
        summary: summary.into(),
        tokens_before,
        timestamp,
    }
}

pub fn create_custom_message(
    custom_type: impl Into<String>,
    content: UserContent,
    display: bool,
    details: Option<Value>,
    timestamp: i64,
) -> CustomMessage {
    CustomMessage {
        custom_type: custom_type.into(),
        content,
        display,
        details,
        timestamp,
    }
}

/// `convertToLlm` — project agent messages down to what a provider accepts.
pub fn convert_to_llm(messages: &[AgentMessage]) -> Vec<Message> {
    let mut out = Vec::with_capacity(messages.len());
    for message in messages {
        let converted = match message {
            AgentMessage::BashExecution(m) => {
                if m.exclude_from_context == Some(true) {
                    continue;
                }
                Message::User(UserMessage {
                    content: UserContent::Blocks(vec![InputContent::text(bash_execution_to_text(
                        m,
                    ))]),
                    timestamp: m.timestamp,
                })
            }
            AgentMessage::Custom(m) => Message::User(UserMessage {
                content: UserContent::Blocks(m.content.blocks()),
                timestamp: m.timestamp,
            }),
            AgentMessage::BranchSummary(m) => Message::User(UserMessage {
                content: UserContent::Blocks(vec![InputContent::text(format!(
                    "{BRANCH_SUMMARY_PREFIX}{}{BRANCH_SUMMARY_SUFFIX}",
                    m.summary
                ))]),
                timestamp: m.timestamp,
            }),
            AgentMessage::CompactionSummary(m) => Message::User(UserMessage {
                content: UserContent::Blocks(vec![InputContent::text(format!(
                    "{COMPACTION_SUMMARY_PREFIX}{}{COMPACTION_SUMMARY_SUFFIX}",
                    m.summary
                ))]),
                timestamp: m.timestamp,
            }),
            AgentMessage::User(m) => Message::User(m.clone()),
            AgentMessage::Assistant(m) => Message::Assistant(m.clone()),
            AgentMessage::ToolResult(m) => Message::ToolResult(m.clone()),
        };
        out.push(converted);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_always_serializes_is_error() {
        let message =
            AgentMessage::ToolResult(ToolResultMessage::text("call-1", "example", "done", false));
        let json = serde_json::to_string(&message).unwrap();
        assert!(json.contains(r#""isError":false"#), "{json}");
        // isError must precede timestamp, matching the TypeScript declaration order.
        assert!(json.find("isError").unwrap() < json.find("timestamp").unwrap());
        let back: AgentMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, message);
    }

    #[test]
    fn role_is_the_first_key() {
        let message = AgentMessage::user_text("hi");
        let json = serde_json::to_string(&message).unwrap();
        assert!(json.starts_with(r#"{"role":"user""#), "{json}");
    }

    #[test]
    fn custom_roles_round_trip() {
        for message in [
            AgentMessage::BashExecution(BashExecutionMessage {
                command: "ls".into(),
                output: "a\nb".into(),
                exit_code: Some(0),
                cancelled: false,
                truncated: false,
                full_output_path: None,
                timestamp: 1,
                exclude_from_context: None,
            }),
            AgentMessage::BranchSummary(create_branch_summary_message("s", "from", 2)),
            AgentMessage::CompactionSummary(create_compaction_summary_message("s", 10, 3)),
            AgentMessage::Custom(create_custom_message(
                "note",
                UserContent::Text("body".into()),
                true,
                None,
                4,
            )),
        ] {
            let json = serde_json::to_string(&message).unwrap();
            let back: AgentMessage = serde_json::from_str(&json).unwrap();
            assert_eq!(back, message);
        }
    }

    #[test]
    fn convert_to_llm_wraps_summaries_and_drops_excluded_bash() {
        let messages = vec![
            AgentMessage::BashExecution(BashExecutionMessage {
                command: "ls".into(),
                output: String::new(),
                exit_code: Some(1),
                cancelled: false,
                truncated: false,
                full_output_path: None,
                timestamp: 1,
                exclude_from_context: Some(true),
            }),
            AgentMessage::BranchSummary(create_branch_summary_message("branch", "e1", 2)),
        ];
        let llm = convert_to_llm(&messages);
        assert_eq!(llm.len(), 1);
        let text = llm[0].as_user().unwrap().content.to_text();
        assert!(text.starts_with(BRANCH_SUMMARY_PREFIX));
        assert!(text.ends_with(BRANCH_SUMMARY_SUFFIX));
    }

    #[test]
    fn bash_execution_text_matches_upstream_shape() {
        let msg = BashExecutionMessage {
            command: "ls".into(),
            output: "out".into(),
            exit_code: Some(2),
            cancelled: false,
            truncated: true,
            full_output_path: Some("/tmp/out".into()),
            timestamp: 1,
            exclude_from_context: None,
        };
        assert_eq!(
            bash_execution_to_text(&msg),
            "Ran `ls`\n```\nout\n```\n\nCommand exited with code 2\n\n[Output truncated. Full output: /tmp/out]"
        );
    }
}
