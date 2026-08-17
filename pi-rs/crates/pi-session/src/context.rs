//! Projecting a branch of entries into an LLM context.
//!
//! Port of `harness/session/context.ts`.

use std::collections::HashMap;
use std::sync::Arc;

use pi_core::StopReason;

use crate::messages::{
    create_branch_summary_message, create_compaction_summary_message, AgentMessage,
};
use crate::types::{Entry, EntryPayload, EntryType};

#[derive(Debug, Clone, PartialEq)]
pub struct ModelSelection {
    pub provider: String,
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionContext {
    pub messages: Vec<AgentMessage>,
    pub thinking_level: String,
    pub model: Option<ModelSelection>,
    pub active_tool_names: Option<Vec<String>>,
}

/// Rewrites the entry path before it becomes messages.
pub type ContextEntryTransform = Arc<dyn Fn(&[Entry]) -> Vec<Entry> + Send + Sync>;

/// Turns a `custom` entry into context messages. Registered per `customType`.
pub type CustomEntryContextMessageProjector =
    Arc<dyn Fn(&Entry, usize, &[Entry]) -> Option<Vec<AgentMessage>> + Send + Sync>;

#[derive(Clone, Default)]
pub struct SessionContextBuildOptions {
    pub entry_transforms: Vec<ContextEntryTransform>,
    pub entry_projectors: HashMap<String, CustomEntryContextMessageProjector>,
}

impl std::fmt::Debug for SessionContextBuildOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionContextBuildOptions")
            .field("entry_transforms", &self.entry_transforms.len())
            .field(
                "entry_projectors",
                &self.entry_projectors.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

fn derive_state(path_entries: &[Entry]) -> (String, Option<ModelSelection>, Option<Vec<String>>) {
    let mut thinking_level = "off".to_string();
    let mut model = None;
    let mut active_tool_names = None;

    for entry in path_entries {
        match &entry.payload {
            EntryPayload::ThinkingLevelChange(change) => {
                thinking_level = change.thinking_level.clone()
            }
            EntryPayload::ModelChange(change) => {
                model = Some(ModelSelection {
                    provider: change.provider.clone(),
                    model_id: change.model_id.clone(),
                })
            }
            EntryPayload::Message(message) => {
                if let AgentMessage::Assistant(assistant) = &message.message {
                    model = Some(ModelSelection {
                        provider: assistant.provider.clone(),
                        model_id: assistant.model.clone(),
                    });
                }
            }
            EntryPayload::ActiveToolsChange(change) => {
                active_tool_names = Some(change.active_tool_names.clone())
            }
            _ => {}
        }
    }
    (thinking_level, model, active_tool_names)
}

/// Everything from the newest compaction onward; the whole path when there is none.
pub fn default_context_entry_transform(path_entries: &[Entry]) -> Vec<Entry> {
    match path_entries
        .iter()
        .rposition(|entry| entry.entry_type() == EntryType::Compaction)
    {
        Some(index) => path_entries[index..].to_vec(),
        None => path_entries.to_vec(),
    }
}

pub fn build_context_entries(
    path_entries: &[Entry],
    options: &SessionContextBuildOptions,
) -> Vec<Entry> {
    let mut entries = default_context_entry_transform(path_entries);
    for transform in &options.entry_transforms {
        entries = transform(&entries);
    }
    entries
}

pub fn session_entry_to_context_messages(
    entry: &Entry,
    index: usize,
    entries: &[Entry],
    options: &SessionContextBuildOptions,
) -> Vec<AgentMessage> {
    match &entry.payload {
        EntryPayload::Message(message) => {
            // A deferred assistant turn has no content yet; including it would
            // send an empty assistant message to the provider.
            if let AgentMessage::Assistant(assistant) = &message.message {
                if assistant.stop_reason == StopReason::Deferred {
                    return Vec::new();
                }
            }
            vec![message.message.clone()]
        }
        EntryPayload::Compaction(compaction) => {
            let mut messages = vec![AgentMessage::CompactionSummary(
                create_compaction_summary_message(
                    compaction.summary.clone(),
                    compaction.tokens_before,
                    entry.timestamp,
                ),
            )];
            messages.extend(compaction.retained_tail.iter().cloned());
            messages
        }
        EntryPayload::BranchSummary(summary) if !summary.summary.is_empty() => {
            vec![AgentMessage::BranchSummary(create_branch_summary_message(
                summary.summary.clone(),
                summary.from_id.clone(),
                entry.timestamp,
            ))]
        }
        EntryPayload::Custom(custom) => options
            .entry_projectors
            .get(&custom.custom_type)
            .and_then(|projector| projector(entry, index, entries))
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

pub fn build_session_context(
    path_entries: &[Entry],
    options: &SessionContextBuildOptions,
) -> SessionContext {
    let (thinking_level, model, active_tool_names) = derive_state(path_entries);
    let context_entries = build_context_entries(path_entries, options);
    let mut messages = Vec::new();
    for (index, entry) in context_entries.iter().enumerate() {
        messages.extend(session_entry_to_context_messages(
            entry,
            index,
            &context_entries,
            options,
        ));
    }
    SessionContext {
        messages,
        thinking_level,
        model,
        active_tool_names,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        ActiveToolsEntry, CompactionEntry, CustomEntry, MessageEntry, ThinkingLevelEntry,
    };
    use serde_json::Map;

    fn entry(seq: i64, payload: EntryPayload) -> Entry {
        Entry {
            id: format!("e{seq}"),
            seq,
            parent_id: if seq == 1 {
                None
            } else {
                Some(format!("e{}", seq - 1))
            },
            timestamp: 1000 + seq,
            payload,
            extra: Map::new(),
        }
    }

    fn message(text: &str) -> EntryPayload {
        EntryPayload::Message(MessageEntry {
            message: AgentMessage::user_text(text),
            terminate: None,
        })
    }

    #[test]
    fn context_starts_at_the_newest_compaction() {
        let entries = vec![
            entry(1, message("dropped")),
            entry(
                2,
                EntryPayload::Compaction(CompactionEntry {
                    summary: "summary".into(),
                    retained_tail: vec![AgentMessage::user_text("retained")],
                    tokens_before: 10,
                    details: None,
                    usage: None,
                }),
            ),
            entry(3, message("kept")),
        ];
        let context = build_session_context(&entries, &SessionContextBuildOptions::default());
        let texts: Vec<String> = context
            .messages
            .iter()
            .map(|message| match message {
                AgentMessage::CompactionSummary(summary) => summary.summary.clone(),
                AgentMessage::User(user) => user.content.to_text(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(texts, vec!["summary", "retained", "kept"]);
    }

    #[test]
    fn derives_thinking_level_and_active_tools_from_the_whole_path() {
        let entries = vec![
            entry(
                1,
                EntryPayload::ThinkingLevelChange(ThinkingLevelEntry {
                    thinking_level: "high".into(),
                }),
            ),
            entry(
                2,
                EntryPayload::ActiveToolsChange(ActiveToolsEntry {
                    active_tool_names: vec!["bash".into()],
                }),
            ),
        ];
        let context = build_session_context(&entries, &SessionContextBuildOptions::default());
        assert_eq!(context.thinking_level, "high");
        assert_eq!(context.active_tool_names, Some(vec!["bash".to_string()]));
        assert!(context.messages.is_empty());
    }

    #[test]
    fn custom_entries_need_a_registered_projector() {
        let entries = vec![entry(
            1,
            EntryPayload::Custom(CustomEntry {
                custom_type: "note".into(),
                data: None,
            }),
        )];
        let mut options = SessionContextBuildOptions::default();
        assert!(build_session_context(&entries, &options)
            .messages
            .is_empty());

        options.entry_projectors.insert(
            "note".into(),
            Arc::new(|_entry, _index, _entries| Some(vec![AgentMessage::user_text("projected")])),
        );
        assert_eq!(build_session_context(&entries, &options).messages.len(), 1);
    }
}
