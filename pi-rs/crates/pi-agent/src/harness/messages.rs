//! Port of `packages/agent/src/harness/messages.ts`.
//!
//! **The types live in `pi-session`, not here.** Upstream declares
//! `AgentMessage` in `agent/src/types.ts` and grows the union through
//! declaration merging in `harness/messages.ts`; Rust has no declaration
//! merging, so the closed union lives in the crate that owns the durable JSONL
//! format — `pi-session` hand-writes `Serialize` to re-materialize the required
//! `isError: false` on tool results, which the TypeScript side needs. This
//! module re-exports that vocabulary and adds the one piece that is the agent
//! runtime's own: the [`MessageConverter`](crate::types::MessageConverter)
//! adapter the loop calls at the provider boundary.

pub use pi_session::messages::{
    bash_execution_to_text, convert_to_llm, create_branch_summary_message,
    create_compaction_summary_message, create_custom_message, BashExecutionMessage,
    BranchSummaryMessage, CompactionSummaryMessage, CustomMessage, BRANCH_SUMMARY_PREFIX,
    BRANCH_SUMMARY_SUFFIX, COMPACTION_SUMMARY_PREFIX, COMPACTION_SUMMARY_SUFFIX,
};

use pi_core::Message;

use crate::types::AgentMessage;

/// [`crate::types::MessageConverter`] wrapper around [`convert_to_llm`].
///
/// Use this instead of [`crate::types::DefaultMessageConverter`] when the
/// transcript carries harness message kinds: the default converter drops
/// anything that is not already an LLM role, whereas this one flattens bash
/// executions, custom messages and summaries onto user messages.
pub struct HarnessMessageConverter;

#[async_trait::async_trait]
impl crate::types::MessageConverter for HarnessMessageConverter {
    async fn convert(&self, messages: &[AgentMessage]) -> Vec<Message> {
        convert_to_llm(messages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MessageConverter;

    #[tokio::test]
    async fn the_harness_converter_flattens_summaries_onto_user_messages() {
        let messages = vec![
            AgentMessage::user_text("hi"),
            AgentMessage::BranchSummary(create_branch_summary_message("branched", "e1", 3)),
            AgentMessage::CompactionSummary(create_compaction_summary_message("compacted", 10, 4)),
        ];
        let converted = HarnessMessageConverter.convert(&messages).await;

        assert_eq!(converted.len(), 3);
        assert!(converted.iter().all(|m| m.role() == "user"));
        let branch = converted[1].as_user().unwrap().content.to_text();
        assert!(branch.starts_with(BRANCH_SUMMARY_PREFIX));
        assert!(branch.ends_with(BRANCH_SUMMARY_SUFFIX));
        let compaction = converted[2].as_user().unwrap().content.to_text();
        assert!(compaction.starts_with(COMPACTION_SUMMARY_PREFIX));
        assert!(compaction.ends_with(COMPACTION_SUMMARY_SUFFIX));
    }

    #[tokio::test]
    async fn the_default_converter_drops_harness_message_kinds() {
        let messages = vec![
            AgentMessage::user_text("hi"),
            AgentMessage::BranchSummary(create_branch_summary_message("branched", "e1", 3)),
        ];
        let converted = crate::types::DefaultMessageConverter
            .convert(&messages)
            .await;
        assert_eq!(converted.len(), 1);
    }
}
