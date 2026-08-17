//! Context compaction. Port of `harness/compaction/compaction.ts`.
//!
//! The decision logic (what to summarize, where to cut, how many tokens the
//! context is worth) is pure and lives here in full. The one impure step —
//! asking a model for the summary — is behind the [`Summarizer`] trait rather
//! than the upstream `Models`/`completeSimpleWithRetries` pair, because the
//! model catalog and retry machinery live in other crates. `pi-agent` (W11)
//! supplies the real implementation; tests supply a scripted one.

use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{AbortSignal, AssistantContent, AssistantMessage, StopReason, Usage};

use crate::compaction::utils::{
    compute_file_lists, create_file_ops, extract_file_ops_from_message, format_file_operations,
    serialize_conversation, FileOperations,
};
use crate::context::{build_session_context, SessionContextBuildOptions};
use crate::messages::{
    convert_to_llm, create_branch_summary_message, create_compaction_summary_message, AgentMessage,
};
use crate::types::{Entry, EntryPayload, EntryType};

/// File-operation details stored on generated compaction entries.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionDetails {
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CompactionErrorKind {
    #[error("aborted")]
    Aborted,
    #[error("summarization failed")]
    SummarizationFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct CompactionError {
    pub kind: CompactionErrorKind,
    pub message: String,
}

impl CompactionError {
    pub fn aborted(message: impl Into<String>) -> Self {
        Self {
            kind: CompactionErrorKind::Aborted,
            message: message.into(),
        }
    }

    pub fn summarization_failed(message: impl Into<String>) -> Self {
        Self {
            kind: CompactionErrorKind::SummarizationFailed,
            message: message.into(),
        }
    }

    pub fn code(&self) -> &'static str {
        match self.kind {
            CompactionErrorKind::Aborted => "aborted",
            CompactionErrorKind::SummarizationFailed => "summarization_failed",
        }
    }
}

/// One summarization request. Mirrors the arguments upstream passes to
/// `completeSimpleWithRetries`.
#[derive(Debug, Clone)]
pub struct SummarizationRequest {
    pub system_prompt: String,
    pub prompt: String,
    pub max_tokens: i64,
    /// `thinkingLevel` when the model supports reasoning and it is not "off".
    pub thinking_level: Option<String>,
    pub signal: Option<AbortSignal>,
}

/// The single impure dependency of compaction.
///
/// Implementations must return an [`AssistantMessage`] rather than an error for
/// provider failures, exactly like `ApiClient::stream`: `stop_reason` carries
/// `Aborted` or `Error` and `error_message` the detail. Reserve `Err` for
/// programmer errors.
#[async_trait]
pub trait Summarizer: Send + Sync {
    async fn summarize(
        &self,
        request: &SummarizationRequest,
    ) -> Result<AssistantMessage, CompactionError>;
}

pub type SummarizerRef = Arc<dyn Summarizer>;

/// Model facts compaction needs, without depending on the catalog crate.
#[derive(Debug, Clone, PartialEq)]
pub struct SummarizationModel {
    pub context_window: i64,
    /// `0` means "no declared limit", matching upstream's `model.maxTokens > 0` check.
    pub max_tokens: i64,
    pub reasoning: bool,
}

impl Default for SummarizationModel {
    fn default() -> Self {
        Self {
            context_window: 128_000,
            max_tokens: 0,
            reasoning: false,
        }
    }
}

/// Compaction thresholds and retention settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionSettings {
    pub enabled: bool,
    /// Tokens reserved for the summary prompt and its output.
    pub reserve_tokens: i64,
    /// Approximate recent-context tokens to keep after compaction.
    pub keep_recent_tokens: i64,
}

pub const DEFAULT_COMPACTION_SETTINGS: CompactionSettings = CompactionSettings {
    enabled: true,
    reserve_tokens: 16384,
    keep_recent_tokens: 20000,
};

impl Default for CompactionSettings {
    fn default() -> Self {
        DEFAULT_COMPACTION_SETTINGS
    }
}

/// Total context tokens from provider usage, falling back to the components.
pub fn calculate_context_tokens(usage: &Usage) -> i64 {
    usage.context_tokens()
}

fn assistant_usage(message: &AgentMessage) -> Option<&Usage> {
    let AgentMessage::Assistant(assistant) = message else {
        return None;
    };
    // Aborted and errored turns report usage that does not reflect the context.
    if matches!(
        assistant.stop_reason,
        StopReason::Aborted | StopReason::Error
    ) {
        return None;
    }
    (calculate_context_tokens(&assistant.usage) > 0).then_some(&assistant.usage)
}

/// Usage from the last valid assistant message in a list of entries.
pub fn get_last_assistant_usage(entries: &[Entry]) -> Option<Usage> {
    entries.iter().rev().find_map(|entry| {
        entry
            .as_message()
            .and_then(|message| assistant_usage(&message.message))
            .cloned()
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsageEstimate {
    pub tokens: i64,
    /// Tokens reported by the most recent assistant usage block.
    pub usage_tokens: i64,
    /// Estimated tokens after that block.
    pub trailing_tokens: i64,
    pub last_usage_index: Option<usize>,
}

pub fn estimate_context_tokens(messages: &[AgentMessage]) -> ContextUsageEstimate {
    let usage_info = messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| assistant_usage(message).map(|usage| (index, usage)));

    let Some((index, usage)) = usage_info else {
        let estimated: i64 = messages.iter().map(estimate_tokens).sum();
        return ContextUsageEstimate {
            tokens: estimated,
            usage_tokens: 0,
            trailing_tokens: estimated,
            last_usage_index: None,
        };
    };

    let usage_tokens = calculate_context_tokens(usage);
    let trailing_tokens: i64 = messages[index + 1..].iter().map(estimate_tokens).sum();
    ContextUsageEstimate {
        tokens: usage_tokens + trailing_tokens,
        usage_tokens,
        trailing_tokens,
        last_usage_index: Some(index),
    }
}

pub fn should_compact(
    context_tokens: i64,
    context_window: i64,
    settings: &CompactionSettings,
) -> bool {
    settings.enabled && context_tokens > context_window - settings.reserve_tokens
}

const ESTIMATED_IMAGE_CHARS: usize = 4800;

fn input_content_chars(content: &[pi_core::InputContent]) -> usize {
    content
        .iter()
        .map(|block| match block {
            pi_core::InputContent::Text(text) => text.text.chars().count(),
            pi_core::InputContent::Image(_) => ESTIMATED_IMAGE_CHARS,
        })
        .sum()
}

fn user_content_chars(content: &pi_core::UserContent) -> usize {
    match content {
        pi_core::UserContent::Text(text) => text.chars().count(),
        pi_core::UserContent::Blocks(blocks) => input_content_chars(blocks),
    }
}

/// Conservative four-characters-per-token heuristic, matching upstream exactly.
pub fn estimate_tokens(message: &AgentMessage) -> i64 {
    let chars: usize = match message {
        AgentMessage::User(user) => user_content_chars(&user.content),
        AgentMessage::Assistant(assistant) => assistant
            .content
            .iter()
            .map(|block| match block {
                AssistantContent::Text(text) => text.text.chars().count(),
                AssistantContent::Thinking(thinking) => thinking.thinking.chars().count(),
                AssistantContent::ToolCall(call) => {
                    call.name.chars().count()
                        + serde_json::to_string(&call.arguments)
                            .unwrap_or_else(|_| "[unserializable]".into())
                            .chars()
                            .count()
                }
            })
            .sum(),
        AgentMessage::ToolResult(result) => input_content_chars(&result.content),
        AgentMessage::Custom(custom) => user_content_chars(&custom.content),
        AgentMessage::BashExecution(bash) => {
            bash.command.chars().count() + bash.output.chars().count()
        }
        AgentMessage::BranchSummary(summary) => summary.summary.chars().count(),
        AgentMessage::CompactionSummary(summary) => summary.summary.chars().count(),
    };
    chars.div_ceil(4) as i64
}

fn message_from_entry(entry: &Entry) -> Option<AgentMessage> {
    match &entry.payload {
        EntryPayload::Message(message) => Some(message.message.clone()),
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

fn message_from_entry_for_compaction(entry: &Entry) -> Option<AgentMessage> {
    if entry.entry_type() == EntryType::Compaction {
        return None;
    }
    message_from_entry(entry)
}

/// Entry indexes a compaction cut may land on. Tool results are excluded so a
/// cut never orphans a tool call from its result.
fn find_valid_cut_points(entries: &[Entry], start_index: usize, end_index: usize) -> Vec<usize> {
    let mut cut_points = Vec::new();
    for (index, entry) in entries.iter().enumerate().take(end_index).skip(start_index) {
        if let Some(message) = entry.as_message() {
            if !matches!(message.message, AgentMessage::ToolResult(_)) {
                cut_points.push(index);
            }
        }
        if entry.entry_type() == EntryType::BranchSummary {
            cut_points.push(index);
        }
    }
    cut_points
}

/// The user-visible message that starts the turn containing `entry_index`.
pub fn find_turn_start_index(
    entries: &[Entry],
    entry_index: usize,
    start_index: usize,
) -> Option<usize> {
    let mut index = entry_index;
    loop {
        let entry = &entries[index];
        if entry.entry_type() == EntryType::BranchSummary {
            return Some(index);
        }
        if let Some(message) = entry.as_message() {
            if matches!(
                message.message,
                AgentMessage::User(_) | AgentMessage::BashExecution(_)
            ) {
                return Some(index);
            }
        }
        if index == start_index {
            return None;
        }
        index -= 1;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutPointResult {
    /// Index of the first entry retained after compaction.
    pub first_kept_entry_index: usize,
    /// Turn-start entry when the cut splits a turn.
    pub turn_start_index: Option<usize>,
    pub is_split_turn: bool,
}

/// The cut point that keeps approximately `keep_recent_tokens` of recent context.
pub fn find_cut_point(
    entries: &[Entry],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: i64,
) -> CutPointResult {
    let cut_points = find_valid_cut_points(entries, start_index, end_index);
    if cut_points.is_empty() {
        return CutPointResult {
            first_kept_entry_index: start_index,
            turn_start_index: None,
            is_split_turn: false,
        };
    }

    let mut accumulated_tokens = 0;
    let mut cut_index = cut_points[0];
    for index in (start_index..end_index).rev() {
        let Some(message) = entries[index].as_message() else {
            continue;
        };
        accumulated_tokens += estimate_tokens(&message.message);
        if accumulated_tokens >= keep_recent_tokens {
            if let Some(candidate) = cut_points.iter().find(|candidate| **candidate >= index) {
                cut_index = *candidate;
            }
            break;
        }
    }

    // Walk back over non-message, non-compaction entries so configuration
    // changes stay with the turn they belong to.
    while cut_index > start_index {
        let previous = &entries[cut_index - 1];
        if matches!(
            previous.entry_type(),
            EntryType::Compaction | EntryType::Message
        ) {
            break;
        }
        cut_index -= 1;
    }

    let is_user_message = entries[cut_index]
        .as_message()
        .is_some_and(|message| matches!(message.message, AgentMessage::User(_)));
    let turn_start_index = if is_user_message {
        None
    } else {
        find_turn_start_index(entries, cut_index, start_index)
    };

    CutPointResult {
        first_kept_entry_index: cut_index,
        turn_start_index,
        is_split_turn: !is_user_message && turn_start_index.is_some(),
    }
}

pub const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.\n\nDo NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

const SUMMARIZATION_PROMPT: &str = r#"The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or "(none)" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

const UPDATE_SUMMARIZATION_PROMPT: &str = r#"The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.

Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from "In Progress" to "Done" when completed
- UPDATE "Next Steps" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = r#"This is the PREFIX of a turn that was too large to keep. The SUFFIX (recent work) is retained.

Summarize the prefix to provide context for the retained suffix:

## Original Request
[What did the user ask for in this turn?]

## Early Progress
- [Key decisions and work done in the prefix]

## Context for Suffix
- [Information needed to understand the retained recent work]

Be concise. Focus on what's needed to understand the kept suffix."#;

/// Prepared inputs for a compaction run.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionPreparation {
    pub messages_to_summarize: Vec<AgentMessage>,
    /// Prefix messages summarized separately when compaction splits a turn.
    pub turn_prefix_messages: Vec<AgentMessage>,
    /// Recent messages retained on the compaction entry.
    pub retained_tail: Vec<AgentMessage>,
    pub is_split_turn: bool,
    pub tokens_before: i64,
    /// Previous compaction summary, for iterative updates.
    pub previous_summary: Option<String>,
    pub file_ops: FileOperations,
    pub settings: CompactionSettings,
}

/// Generated compaction data ready to be persisted as a compaction entry.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactResult {
    pub summary: String,
    pub tokens_before: i64,
    pub usage: Option<Usage>,
    pub retained_tail: Vec<AgentMessage>,
    pub details: CompactionDetails,
}

fn extract_file_operations(
    messages: &[AgentMessage],
    entries: &[Entry],
    prev_compaction_index: Option<usize>,
) -> FileOperations {
    let mut file_ops = create_file_ops();
    if let Some(index) = prev_compaction_index {
        if let Some(details) = entries[index]
            .as_compaction()
            .and_then(|entry| entry.details.as_ref())
        {
            if let Ok(details) = serde_json::from_value::<CompactionDetails>(details.clone()) {
                file_ops.read.extend(details.read_files);
                file_ops.edited.extend(details.modified_files);
            }
        }
    }
    for message in messages {
        extract_file_ops_from_message(message, &mut file_ops);
    }
    file_ops
}

/// Prepare session entries for compaction, or `None` when it does not apply.
pub fn prepare_compaction(
    path_entries: &[Entry],
    settings: &CompactionSettings,
) -> Option<CompactionPreparation> {
    if path_entries.is_empty() || path_entries.last()?.entry_type() == EntryType::Compaction {
        return None;
    }

    let prev_compaction_index = path_entries
        .iter()
        .rposition(|entry| entry.entry_type() == EntryType::Compaction);

    let mut previous_summary = None;
    let compactable_entries: Vec<Entry> = match prev_compaction_index {
        None => path_entries.to_vec(),
        Some(index) => {
            let prev = path_entries[index]
                .as_compaction()
                .expect("compaction entry");
            previous_summary = Some(prev.summary.clone());
            // The previous compaction's retained tail becomes virtual entries so
            // the cut-point search can consider it alongside newer entries.
            let mut entries: Vec<Entry> = prev
                .retained_tail
                .iter()
                .enumerate()
                .map(|(tail_index, message)| Entry {
                    id: format!("{}:retained:{tail_index}", path_entries[index].id),
                    seq: path_entries[index].seq,
                    parent_id: Some(if tail_index == 0 {
                        path_entries[index].id.clone()
                    } else {
                        format!("{}:retained:{}", path_entries[index].id, tail_index - 1)
                    }),
                    timestamp: message.timestamp(),
                    payload: EntryPayload::Message(crate::types::MessageEntry {
                        message: message.clone(),
                        terminate: None,
                    }),
                    extra: serde_json::Map::new(),
                })
                .collect();
            entries.extend_from_slice(&path_entries[index + 1..]);
            entries
        }
    };
    let boundary_end = compactable_entries.len();

    let tokens_before = estimate_context_tokens(
        &build_session_context(path_entries, &SessionContextBuildOptions::default()).messages,
    )
    .tokens;

    let cut_point = find_cut_point(
        &compactable_entries,
        0,
        boundary_end,
        settings.keep_recent_tokens,
    );
    let history_end = if cut_point.is_split_turn {
        cut_point
            .turn_start_index
            .unwrap_or(cut_point.first_kept_entry_index)
    } else {
        cut_point.first_kept_entry_index
    };

    let messages_to_summarize: Vec<AgentMessage> = compactable_entries[..history_end]
        .iter()
        .filter_map(message_from_entry_for_compaction)
        .collect();
    let turn_prefix_messages: Vec<AgentMessage> = if cut_point.is_split_turn {
        compactable_entries
            [cut_point.turn_start_index.unwrap_or(0)..cut_point.first_kept_entry_index]
            .iter()
            .filter_map(message_from_entry_for_compaction)
            .collect()
    } else {
        Vec::new()
    };
    let retained_tail: Vec<AgentMessage> = compactable_entries
        [cut_point.first_kept_entry_index..boundary_end]
        .iter()
        .filter_map(message_from_entry_for_compaction)
        .collect();

    let mut file_ops =
        extract_file_operations(&messages_to_summarize, path_entries, prev_compaction_index);
    if cut_point.is_split_turn {
        for message in &turn_prefix_messages {
            extract_file_ops_from_message(message, &mut file_ops);
        }
    }

    Some(CompactionPreparation {
        messages_to_summarize,
        turn_prefix_messages,
        retained_tail,
        is_split_turn: cut_point.is_split_turn,
        tokens_before,
        previous_summary,
        file_ops,
        settings: *settings,
    })
}

fn bounded_max_tokens(fraction: f64, reserve_tokens: i64, model: &SummarizationModel) -> i64 {
    let scaled = (fraction * reserve_tokens as f64).floor() as i64;
    if model.max_tokens > 0 {
        scaled.min(model.max_tokens)
    } else {
        scaled
    }
}

fn response_text(message: &AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn check_response(
    response: &AssistantMessage,
    aborted_message: &str,
    failed_prefix: &str,
) -> Result<(), CompactionError> {
    match response.stop_reason {
        StopReason::Aborted => Err(CompactionError::aborted(
            response
                .error_message
                .clone()
                .unwrap_or_else(|| aborted_message.into()),
        )),
        StopReason::Error => Err(CompactionError::summarization_failed(format!(
            "{failed_prefix}: {}",
            response
                .error_message
                .clone()
                .unwrap_or_else(|| "Unknown error".into())
        ))),
        _ => Ok(()),
    }
}

pub(crate) fn thinking_for(
    model: &SummarizationModel,
    thinking_level: Option<&str>,
) -> Option<String> {
    match thinking_level {
        Some(level) if model.reasoning && level != "off" => Some(level.to_string()),
        _ => None,
    }
}

/// Everything `generate_summary_with_usage` needs besides the messages.
#[derive(Debug, Clone, Default)]
pub struct GenerateSummaryOptions {
    /// Tokens reserved for the prompt and the model's output.
    pub reserve_tokens: i64,
    pub signal: Option<AbortSignal>,
    pub custom_instructions: Option<String>,
    /// Present for an iterative update of an earlier compaction summary.
    pub previous_summary: Option<String>,
    pub thinking_level: Option<String>,
}

/// Generate or update a conversation summary, returning its provider usage.
pub async fn generate_summary_with_usage(
    current_messages: &[AgentMessage],
    summarizer: &dyn Summarizer,
    model: &SummarizationModel,
    options: &GenerateSummaryOptions,
) -> Result<(String, Usage), CompactionError> {
    let GenerateSummaryOptions {
        reserve_tokens,
        signal,
        custom_instructions,
        previous_summary,
        thinking_level,
    } = options;
    let (reserve_tokens, custom_instructions, previous_summary, thinking_level) = (
        *reserve_tokens,
        custom_instructions.as_deref(),
        previous_summary.as_deref(),
        thinking_level.as_deref(),
    );
    let max_tokens = bounded_max_tokens(0.8, reserve_tokens, model);
    let mut base_prompt = if previous_summary.is_some() {
        UPDATE_SUMMARIZATION_PROMPT.to_string()
    } else {
        SUMMARIZATION_PROMPT.to_string()
    };
    if let Some(instructions) = custom_instructions {
        base_prompt = format!("{base_prompt}\n\nAdditional focus: {instructions}");
    }
    let conversation_text = serialize_conversation(&convert_to_llm(current_messages));
    let mut prompt = format!("<conversation>\n{conversation_text}\n</conversation>\n\n");
    if let Some(previous) = previous_summary {
        prompt.push_str(&format!(
            "<previous-summary>\n{previous}\n</previous-summary>\n\n"
        ));
    }
    prompt.push_str(&base_prompt);

    let response = summarizer
        .summarize(&SummarizationRequest {
            system_prompt: SUMMARIZATION_SYSTEM_PROMPT.into(),
            prompt,
            max_tokens,
            thinking_level: thinking_for(model, thinking_level),
            signal: signal.clone(),
        })
        .await?;
    check_response(&response, "Summarization aborted", "Summarization failed")?;
    Ok((response_text(&response), response.usage))
}

async fn generate_turn_prefix_summary(
    messages: &[AgentMessage],
    summarizer: &dyn Summarizer,
    model: &SummarizationModel,
    reserve_tokens: i64,
    signal: Option<AbortSignal>,
    thinking_level: Option<&str>,
) -> Result<(String, Usage), CompactionError> {
    let conversation_text = serialize_conversation(&convert_to_llm(messages));
    let response = summarizer
        .summarize(&SummarizationRequest {
            system_prompt: SUMMARIZATION_SYSTEM_PROMPT.into(),
            prompt: format!("<conversation>\n{conversation_text}\n</conversation>\n\n{TURN_PREFIX_SUMMARIZATION_PROMPT}"),
            max_tokens: bounded_max_tokens(0.5, reserve_tokens, model),
            thinking_level: thinking_for(model, thinking_level),
            signal,
        })
        .await?;
    check_response(
        &response,
        "Turn prefix summarization aborted",
        "Turn prefix summarization failed",
    )?;
    Ok((response_text(&response), response.usage))
}

/// Generate compaction summary data from prepared session history.
pub async fn compact(
    preparation: &CompactionPreparation,
    summarizer: &dyn Summarizer,
    model: &SummarizationModel,
    custom_instructions: Option<&str>,
    signal: Option<AbortSignal>,
    thinking_level: Option<&str>,
) -> Result<CompactResult, CompactionError> {
    let (summary, summary_usage) =
        if preparation.is_split_turn && !preparation.turn_prefix_messages.is_empty() {
            let mut history_text = "No prior history.".to_string();
            let mut history_usage = None;
            if !preparation.messages_to_summarize.is_empty() {
                let (text, usage) = generate_summary_with_usage(
                    &preparation.messages_to_summarize,
                    summarizer,
                    model,
                    &GenerateSummaryOptions {
                        reserve_tokens: preparation.settings.reserve_tokens,
                        signal: signal.clone(),
                        custom_instructions: custom_instructions.map(str::to_string),
                        previous_summary: preparation.previous_summary.clone(),
                        thinking_level: thinking_level.map(str::to_string),
                    },
                )
                .await?;
                history_text = text;
                history_usage = Some(usage);
            }
            let (prefix_text, prefix_usage) = generate_turn_prefix_summary(
                &preparation.turn_prefix_messages,
                summarizer,
                model,
                preparation.settings.reserve_tokens,
                signal,
                thinking_level,
            )
            .await?;
            let usage = match history_usage {
                Some(history) => Usage::add(&history, &prefix_usage),
                None => prefix_usage,
            };
            (
                format!("{history_text}\n\n---\n\n**Turn Context (split turn):**\n\n{prefix_text}"),
                usage,
            )
        } else {
            generate_summary_with_usage(
                &preparation.messages_to_summarize,
                summarizer,
                model,
                &GenerateSummaryOptions {
                    reserve_tokens: preparation.settings.reserve_tokens,
                    signal,
                    custom_instructions: custom_instructions.map(str::to_string),
                    previous_summary: preparation.previous_summary.clone(),
                    thinking_level: thinking_level.map(str::to_string),
                },
            )
            .await?
        };

    let (read_files, modified_files) = compute_file_lists(&preparation.file_ops);
    let summary = format!(
        "{summary}{}",
        format_file_operations(&read_files, &modified_files)
    );

    Ok(CompactResult {
        summary,
        tokens_before: preparation.tokens_before,
        usage: Some(summary_usage),
        retained_tail: preparation.retained_tail.clone(),
        details: CompactionDetails {
            read_files,
            modified_files,
        },
    })
}
