//! Context compaction and branch summarization.
//!
//! Port of `harness/compaction/`. The decision logic is a faithful,
//! fully-tested port; the model call is abstracted behind
//! [`compaction::Summarizer`] because the catalog (`pi-catalog`) and retry
//! (`pi-http`) machinery upstream's `completeSimpleWithRetries` uses belong to
//! other crates. `pi-agent` (W11) wires the real one.

pub mod branch_summarization;
#[allow(clippy::module_inception)]
pub mod compaction;
pub mod utils;

pub use branch_summarization::{
    collect_entries_for_branch_summary, generate_branch_summary, prepare_branch_entries,
    BranchPreparation, BranchSummaryDetails, BranchSummaryResult, CollectEntriesResult,
    GenerateBranchSummaryOptions,
};
pub use compaction::{
    calculate_context_tokens, compact, estimate_context_tokens, estimate_tokens, find_cut_point,
    find_turn_start_index, generate_summary_with_usage, get_last_assistant_usage,
    prepare_compaction, should_compact, CompactResult, CompactionDetails, CompactionError,
    CompactionErrorKind, CompactionPreparation, CompactionSettings, ContextUsageEstimate,
    CutPointResult, GenerateSummaryOptions, SummarizationModel, SummarizationRequest, Summarizer,
    SummarizerRef, DEFAULT_COMPACTION_SETTINGS, SUMMARIZATION_SYSTEM_PROMPT,
};
pub use utils::{
    compute_file_lists, create_file_ops, extract_file_ops_from_message, format_file_operations,
    serialize_conversation, FileOperations,
};
