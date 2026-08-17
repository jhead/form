//! Port of `packages/agent/src/harness/`.
//!
//! `agent-harness.ts`, `reducer.ts` and `compaction/` are absent: they sit on
//! `pi-session` (W10), which is still in flight.

pub mod agent_harness;
pub mod events;
pub mod frontmatter;
pub mod messages;
pub mod prompt_templates;
pub mod skills;
pub mod summarizer;
pub mod system_prompt;
pub mod telemetry;
pub mod types;
