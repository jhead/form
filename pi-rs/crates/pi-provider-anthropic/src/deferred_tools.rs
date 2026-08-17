//! Port of `packages/ai/src/utils/deferred-tools.ts`.

use std::collections::HashSet;

use pi_core::message::Message;
use pi_core::tool::{Context, Tool};

/// Tools split into the prefix definitions and the ones loaded mid-transcript
/// via `tool_reference` blocks. Insertion order matches upstream's `Map`.
pub struct ToolPlacement {
    pub immediate: Vec<Tool>,
    pub deferred: Vec<(String, Tool)>,
}

/// Split current tools into prefix and transcript-loaded definitions.
pub fn split_deferred_tools(
    context: &Context,
    messages: &[Message],
    enabled: bool,
    normalize_name: &dyn Fn(&str) -> String,
) -> ToolPlacement {
    // Insertion-ordered dedupe by normalized name; a later definition replaces
    // an earlier one in place, exactly like `Map.set`.
    let mut unique: Vec<(String, Tool)> = Vec::new();
    for tool in context.tools() {
        let name = normalize_name(&tool.name);
        match unique.iter_mut().find(|(existing, _)| existing == &name) {
            Some(entry) => entry.1 = tool.clone(),
            None => unique.push((name, tool.clone())),
        }
    }

    if !enabled {
        return ToolPlacement {
            immediate: unique.into_iter().map(|(_, tool)| tool).collect(),
            deferred: Vec::new(),
        };
    }

    let mut deferred_names: HashSet<String> = HashSet::new();
    let mut used_names: HashSet<String> = HashSet::new();
    for message in messages {
        match message {
            Message::Assistant(assistant) => {
                for tool_call in assistant.tool_calls() {
                    used_names.insert(normalize_name(&tool_call.name));
                }
            }
            Message::ToolResult(result) => {
                for name in result.added_tool_names.iter().flatten() {
                    let normalized = normalize_name(name);
                    if !used_names.contains(&normalized) {
                        deferred_names.insert(normalized);
                    }
                }
            }
            Message::User(_) => {}
        }
    }

    let mut immediate = Vec::new();
    let mut deferred = Vec::new();
    for (name, tool) in unique {
        if deferred_names.contains(&name) {
            deferred.push((name, tool));
        } else {
            immediate.push(tool);
        }
    }

    ToolPlacement {
        immediate,
        deferred,
    }
}
