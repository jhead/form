//! Summarizing an abandoned branch before navigating away.
//!
//! Port of `harness/compaction/branch-summarization.ts`. Like
//! [`crate::compaction::compaction`], the LLM call goes through
//! [`Summarizer`] instead of the upstream `Models` object.

use pi_core::AbortSignal;

use crate::compaction::compaction::{
    check_response, estimate_tokens, thinking_for, CompactionError, SummarizationModel,
    SummarizationRequest, Summarizer, SUMMARIZATION_SYSTEM_PROMPT,
};
use crate::compaction::utils::{
    compute_file_lists, create_file_ops, extract_file_ops_from_message, format_file_operations,
    serialize_conversation, FileOperations,
};
use crate::error::{SessionError, SessionResult};
use crate::messages::{
    convert_to_llm, create_branch_summary_message, create_compaction_summary_message, AgentMessage,
};
use crate::session::Session;
use crate::types::{BranchQuery, Entry, EntryPayload};

/// File-operation details stored on generated branch-summary entries.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryDetails {
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BranchSummaryResult {
    pub summary: String,
    pub usage: Option<pi_core::Usage>,
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CollectEntriesResult {
    /// Entries to summarize, chronological.
    pub entries: Vec<Entry>,
    /// Deepest common ancestor of the previous leaf and the target.
    pub common_ancestor_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BranchPreparation {
    pub messages: Vec<AgentMessage>,
    pub file_ops: FileOperations,
    pub total_tokens: i64,
}

/// Entries that should be summarized before navigating to `target_id`.
pub async fn collect_entries_for_branch_summary(
    session: &Session,
    old_leaf_id: Option<&str>,
    target_id: &str,
) -> SessionResult<CollectEntriesResult> {
    let Some(old_leaf_id) = old_leaf_id else {
        return Ok(CollectEntriesResult {
            entries: Vec::new(),
            common_ancestor_id: None,
        });
    };
    let old_path: std::collections::HashSet<String> = session
        .find_entries_on_branch(&BranchQuery::new().with_start(old_leaf_id))
        .await?
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    let target_path = session
        .find_entries_on_branch(&BranchQuery::new().with_start(target_id))
        .await?;
    let common_ancestor_id = target_path
        .iter()
        .find(|entry| old_path.contains(&entry.id))
        .map(|entry| entry.id.clone());

    let mut entries = Vec::new();
    let mut current = Some(old_leaf_id.to_string());
    while let Some(id) = current {
        if Some(&id) == common_ancestor_id.as_ref() {
            break;
        }
        let entry = session
            .get_entry(&id)
            .await?
            .ok_or_else(|| SessionError::invalid_entry(format!("Entry {id} not found")))?;
        current = entry.parent_id.clone();
        entries.push(entry);
    }
    entries.reverse();

    Ok(CollectEntriesResult {
        entries,
        common_ancestor_id,
    })
}

fn message_from_entry(entry: &Entry) -> Option<AgentMessage> {
    match &entry.payload {
        // Tool results add noise without context once the branch is abandoned.
        EntryPayload::Message(message) => match &message.message {
            AgentMessage::ToolResult(_) => None,
            other => Some(other.clone()),
        },
        EntryPayload::BranchSummary(summary) => {
            Some(AgentMessage::BranchSummary(create_branch_summary_message(
                summary.summary.clone(),
                summary.from_id.clone(),
                entry.timestamp,
            )))
        }
        EntryPayload::Compaction(compaction) => Some(AgentMessage::CompactionSummary(
            create_compaction_summary_message(
                compaction.summary.clone(),
                compaction.tokens_before,
                entry.timestamp,
            ),
        )),
        _ => None,
    }
}

/// Select branch entries within a token budget, newest first.
pub fn prepare_branch_entries(entries: &[Entry], token_budget: i64) -> BranchPreparation {
    let mut messages: Vec<AgentMessage> = Vec::new();
    let mut file_ops = create_file_ops();
    let mut total_tokens = 0;

    // Earlier branch summaries already recorded their file operations.
    for entry in entries {
        if let EntryPayload::BranchSummary(summary) = &entry.payload {
            if let Some(details) = &summary.details {
                if let Ok(details) = serde_json::from_value::<BranchSummaryDetails>(details.clone())
                {
                    file_ops.read.extend(details.read_files);
                    file_ops.edited.extend(details.modified_files);
                }
            }
        }
    }

    for entry in entries.iter().rev() {
        let Some(message) = message_from_entry(entry) else {
            continue;
        };
        extract_file_ops_from_message(&message, &mut file_ops);

        let tokens = estimate_tokens(&message);
        if token_budget > 0 && total_tokens + tokens > token_budget {
            // A summary entry is worth keeping even when it overflows slightly,
            // because it stands in for everything before it.
            if matches!(
                entry.payload,
                EntryPayload::Compaction(_) | EntryPayload::BranchSummary(_)
            ) && (total_tokens as f64) < token_budget as f64 * 0.9
            {
                messages.insert(0, message);
                total_tokens += tokens;
            }
            break;
        }

        messages.insert(0, message);
        total_tokens += tokens;
    }

    BranchPreparation {
        messages,
        file_ops,
        total_tokens,
    }
}

const BRANCH_SUMMARY_PREAMBLE: &str =
    "The user explored a different conversation branch before returning here.\nSummary of that exploration:\n\n";

const BRANCH_SUMMARY_PROMPT: &str = r#"Create a structured summary of this conversation branch for context when returning later.

Use this EXACT format:

## Goal
[What was the user trying to accomplish in this branch?]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Work that was started but not finished]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [What should happen next to continue this work]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

#[derive(Debug, Clone, Default)]
pub struct GenerateBranchSummaryOptions {
    pub custom_instructions: Option<String>,
    /// Replace the default prompt instead of appending to it.
    pub replace_instructions: bool,
    /// Tokens reserved for the prompt and model output. Defaults to 16384.
    pub reserve_tokens: Option<i64>,
    pub signal: Option<AbortSignal>,
    pub thinking_level: Option<String>,
}

/// Generate a summary for abandoned branch entries.
pub async fn generate_branch_summary(
    entries: &[Entry],
    summarizer: &dyn Summarizer,
    model: &SummarizationModel,
    options: &GenerateBranchSummaryOptions,
) -> Result<BranchSummaryResult, CompactionError> {
    let reserve_tokens = options.reserve_tokens.unwrap_or(16384);
    let context_window = if model.context_window > 0 {
        model.context_window
    } else {
        128_000
    };
    let token_budget = context_window - reserve_tokens;

    let preparation = prepare_branch_entries(entries, token_budget);
    if preparation.messages.is_empty() {
        return Ok(BranchSummaryResult {
            summary: "No content to summarize".into(),
            usage: None,
            read_files: Vec::new(),
            modified_files: Vec::new(),
        });
    }

    let conversation_text = serialize_conversation(&convert_to_llm(&preparation.messages));
    let instructions = match (&options.custom_instructions, options.replace_instructions) {
        (Some(custom), true) => custom.clone(),
        (Some(custom), false) => format!("{BRANCH_SUMMARY_PROMPT}\n\nAdditional focus: {custom}"),
        (None, _) => BRANCH_SUMMARY_PROMPT.to_string(),
    };

    let response = summarizer
        .summarize(&SummarizationRequest {
            system_prompt: SUMMARIZATION_SYSTEM_PROMPT.into(),
            prompt: format!(
                "<conversation>\n{conversation_text}\n</conversation>\n\n{instructions}"
            ),
            max_tokens: 2048,
            thinking_level: thinking_for(model, options.thinking_level.as_deref()),
            signal: options.signal.clone(),
        })
        .await?;
    check_response(&response, "Branch summary aborted", "Branch summary failed")?;

    let text = response
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    let (read_files, modified_files) = compute_file_lists(&preparation.file_ops);
    let mut summary = format!("{BRANCH_SUMMARY_PREAMBLE}{text}");
    summary.push_str(&format_file_operations(&read_files, &modified_files));

    Ok(BranchSummaryResult {
        summary,
        usage: Some(response.usage),
        read_files,
        modified_files,
    })
}
