//! The built-in tool set.
//!
//! Port of `.upstream/packages/agent/src/harness/tools/`, plus the `find` and
//! `grep` tools from `.upstream/packages/coding-agent/src/core/tools/`.

pub mod bash;
pub mod edit;
pub mod find;
pub mod grep;
pub mod image;
pub mod read;
pub mod write;

use std::sync::Arc;

use crate::tool::AgentToolRef;

/// Every built-in tool, in the order upstream registers them.
pub fn default_tools() -> Vec<AgentToolRef> {
    vec![
        Arc::new(bash::BashTool::new()),
        Arc::new(read::ReadTool::new()),
        Arc::new(write::WriteTool::new()),
        Arc::new(edit::EditTool::new()),
        Arc::new(find::FindTool::new()),
        Arc::new(grep::GrepTool::new()),
    ]
}
