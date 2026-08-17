//! The `find` tool: locate files by glob pattern.
//!
//! Port of `.upstream/packages/coding-agent/src/core/tools/find.ts`, with the
//! `fd` subprocess replaced by an in-process walk (see [`crate::search`]).

use std::path::Path;

use async_trait::async_trait;
use pi_core::AbortSignal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{ToolError, ToolResultOf};
use crate::path_utils::resolve_tool_path;
use crate::search::{glob_paths, relativize_result_path};
use crate::tool::{check_tool_abort, parse_args, AgentTool, ToolContext, ToolResult};
use crate::truncate::{
    format_size, truncate_head, TruncationOptions, TruncationResult, DEFAULT_MAX_BYTES,
};

const DEFAULT_LIMIT: usize = 1000;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindToolInput {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindToolDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_limit_reached: Option<usize>,
}

#[derive(Clone, Default)]
pub struct FindTool;

impl FindTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl AgentTool for FindTool {
    fn name(&self) -> &str {
        "find"
    }

    fn description(&self) -> String {
        format!(
            "Search for files by glob pattern. Returns matching file paths relative to the search \
directory. Respects .gitignore. Output is truncated to {DEFAULT_LIMIT} results or {}KB (whichever is \
hit first).",
            DEFAULT_MAX_BYTES / 1024
        )
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern to match files, e.g. '*.ts', '**/*.json', or 'src/**/*.spec.ts'" },
                "path": { "type": "string", "description": "Directory to search in (default: current directory)" },
                "limit": { "type": "number", "description": "Maximum number of results (default: 1000)" }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(
        &self,
        args: Value,
        context: &ToolContext,
        abort: Option<AbortSignal>,
    ) -> ToolResultOf<ToolResult> {
        let input: FindToolInput = parse_args("find", args)?;
        check_tool_abort(&abort)?;

        let search_dir = input.path.clone().unwrap_or_else(|| ".".to_string());
        let search_path =
            resolve_tool_path(context.env.as_ref(), &search_dir, abort.clone()).await?;
        if !context.env.exists(&search_path, abort.clone()).await? {
            return Err(ToolError::failed(format!("Path not found: {search_path}")));
        }
        let effective_limit = input
            .limit
            .map(|limit| limit.max(1) as usize)
            .unwrap_or(DEFAULT_LIMIT);

        let root = Path::new(&search_path);
        let matches =
            glob_paths(root, &input.pattern, effective_limit).map_err(ToolError::failed)?;
        check_tool_abort(&abort)?;

        if matches.is_empty() {
            return Ok(ToolResult::text("No files found matching pattern"));
        }

        let relativized: Vec<String> = matches
            .iter()
            .map(|path| relativize_result_path(path, root))
            .collect();
        let result_limit_reached = relativized.len() >= effective_limit;
        let truncation = truncate_head(
            &relativized.join("\n"),
            TruncationOptions::bytes_only(DEFAULT_MAX_BYTES),
        );

        let mut output = truncation.content.clone();
        let mut details = FindToolDetails {
            truncation: None,
            result_limit_reached: None,
        };
        let mut notices: Vec<String> = Vec::new();
        if result_limit_reached {
            notices.push(format!(
                "{effective_limit} results limit reached. Use limit={} for more, or refine pattern",
                effective_limit * 2
            ));
            details.result_limit_reached = Some(effective_limit);
        }
        if truncation.truncated {
            notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
            details.truncation = Some(truncation);
        }
        if !notices.is_empty() {
            output.push_str(&format!("\n\n[{}]", notices.join(". ")));
        }

        let has_details = details.truncation.is_some() || details.result_limit_reached.is_some();
        Ok(ToolResult::text(output).with_details(
            has_details.then(|| serde_json::to_value(details).unwrap_or(Value::Null)),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::local::LocalExecutionEnv;
    use crate::types::ExecutionEnvRef;

    fn temp_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::create_dir_all(root.join("build")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("src/nested/lib.rs"), "pub fn f() {}\n").unwrap();
        std::fs::write(root.join("notes.md"), "# notes\n").unwrap();
        std::fs::write(root.join("build/out.rs"), "generated\n").unwrap();
        std::fs::write(root.join(".gitignore"), "build/\n").unwrap();
        dir
    }

    fn context_for(dir: &tempfile::TempDir) -> ToolContext {
        let env: ExecutionEnvRef = Arc::new(LocalExecutionEnv::new(
            dir.path().to_string_lossy().into_owned(),
        ));
        ToolContext::new(env)
    }

    #[tokio::test]
    async fn finds_files_by_basename_glob() {
        let dir = temp_repo();
        let context = context_for(&dir);

        let result = FindTool::new()
            .execute(json!({ "pattern": "*.rs" }), &context, None)
            .await
            .unwrap();

        let output = result.text_output();
        let lines: Vec<&str> = output.lines().collect();
        assert!(lines.contains(&"src/main.rs"));
        assert!(lines.contains(&"src/nested/lib.rs"));
        assert!(!lines.iter().any(|l| l.starts_with("build/")));
    }

    #[tokio::test]
    async fn finds_files_by_path_glob() {
        let dir = temp_repo();
        let context = context_for(&dir);

        let result = FindTool::new()
            .execute(json!({ "pattern": "src/nested/*.rs" }), &context, None)
            .await
            .unwrap();

        assert_eq!(result.text_output(), "src/nested/lib.rs");
    }

    #[tokio::test]
    async fn reports_no_matches() {
        let dir = temp_repo();
        let context = context_for(&dir);

        let result = FindTool::new()
            .execute(json!({ "pattern": "*.zig" }), &context, None)
            .await
            .unwrap();

        assert_eq!(result.text_output(), "No files found matching pattern");
        assert!(result.details.is_none());
    }

    #[tokio::test]
    async fn reports_the_result_limit() {
        let dir = temp_repo();
        let context = context_for(&dir);

        let result = FindTool::new()
            .execute(json!({ "pattern": "*.rs", "limit": 1 }), &context, None)
            .await
            .unwrap();

        assert!(result
            .text_output()
            .contains("[1 results limit reached. Use limit=2 for more, or refine pattern]"));
        let details: FindToolDetails =
            serde_json::from_value(result.details.clone().unwrap()).unwrap();
        assert_eq!(details.result_limit_reached, Some(1));
    }

    #[tokio::test]
    async fn rejects_a_missing_search_path() {
        let dir = temp_repo();
        let context = context_for(&dir);

        let error = FindTool::new()
            .execute(json!({ "pattern": "*", "path": "nope" }), &context, None)
            .await
            .unwrap_err();
        assert!(error.message().starts_with("Path not found: "));
    }

    #[tokio::test]
    async fn rejects_invalid_globs() {
        let dir = temp_repo();
        let context = context_for(&dir);

        let error = FindTool::new()
            .execute(json!({ "pattern": "[" }), &context, None)
            .await
            .unwrap_err();
        assert!(error.message().contains("Invalid glob pattern"));
    }
}
