//! The subset of `packages/agent/src/harness/types.ts` the agent runtime needs.
//!
//! `FileSystem` / `Shell` / `ExecutionEnv` are **not** re-declared here: they
//! belong to `pi-tools` (W9). Everything in this module is capability-free.

use serde::{Deserialize, Serialize};

/// Skill loaded from a `SKILL.md` file or supplied by an application.
///
/// `name`, `description` and `file_path` are inserted into the system prompt in
/// an XML block (see [`crate::harness::system_prompt::format_skills_for_system_prompt`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    /// Stable skill name used for lookup and model-visible listings.
    pub name: String,
    /// Short model-visible description of when to use the skill.
    pub description: String,
    /// Full skill instructions.
    pub content: String,
    /// Absolute path to the skill file.
    pub file_path: String,
    /// Hide from model-visible lists while still allowing explicit invocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_model_invocation: Option<bool>,
}

/// Prompt template formatted into a prompt for explicit invocation.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptTemplate {
    /// Stable template name used for lookup or command routing.
    pub name: String,
    /// Optional description for command lists or autocomplete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Template content. Argument placeholders are substituted on invocation.
    pub content: String,
}

/// Resources made available to explicit invocation and system-prompt callbacks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHarnessResources {
    #[serde(default)]
    pub prompt_templates: Vec<PromptTemplate>,
    #[serde(default)]
    pub skills: Vec<Skill>,
}
