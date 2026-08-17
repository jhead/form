//! The `grep` tool: search file contents.
//!
//! Port of `.upstream/packages/coding-agent/src/core/tools/grep.ts`, with the
//! `rg` subprocess replaced by an in-process walk (see [`crate::search`]) and
//! the `regex` crate.

use std::path::Path;

use async_trait::async_trait;
use pi_core::AbortSignal;
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{ToolError, ToolResultOf};
use crate::path_utils::resolve_tool_path;
use crate::search::{candidate_files, looks_binary, relativize_result_path};
use crate::tool::{check_tool_abort, parse_args, AgentTool, ToolContext, ToolResult};
use crate::truncate::{
    format_size, truncate_head, truncate_line, TruncationOptions, TruncationResult,
    DEFAULT_MAX_BYTES, GREP_MAX_LINE_LENGTH,
};

const DEFAULT_LIMIT: usize = 100;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepToolInput {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub glob: Option<String>,
    #[serde(default)]
    pub ignore_case: Option<bool>,
    #[serde(default)]
    pub literal: Option<bool>,
    #[serde(default)]
    pub context: Option<i64>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepToolDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_limit_reached: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines_truncated: Option<bool>,
}

#[derive(Clone, Default)]
pub struct GrepTool;

impl GrepTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl AgentTool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> String {
        format!(
            "Search file contents for a pattern. Returns matching lines with file paths and line \
numbers. Respects .gitignore. Output is truncated to {DEFAULT_LIMIT} matches or {}KB (whichever is hit \
first). Long lines are truncated to {GREP_MAX_LINE_LENGTH} chars.",
            DEFAULT_MAX_BYTES / 1024
        )
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Search pattern (regex or literal string)" },
                "path": { "type": "string", "description": "Directory or file to search (default: current directory)" },
                "glob": { "type": "string", "description": "Filter files by glob pattern, e.g. '*.ts' or '**/*.spec.ts'" },
                "ignoreCase": { "type": "boolean", "description": "Case-insensitive search (default: false)" },
                "literal": { "type": "boolean", "description": "Treat pattern as literal string instead of regex (default: false)" },
                "context": { "type": "number", "description": "Number of lines to show before and after each match (default: 0)" },
                "limit": { "type": "number", "description": "Maximum number of matches to return (default: 100)" }
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
        let input: GrepToolInput = parse_args("grep", args)?;
        check_tool_abort(&abort)?;

        let search_dir = input.path.clone().unwrap_or_else(|| ".".to_string());
        let search_path =
            resolve_tool_path(context.env.as_ref(), &search_dir, abort.clone()).await?;
        if !context.env.exists(&search_path, abort.clone()).await? {
            return Err(ToolError::failed(format!("Path not found: {search_path}")));
        }

        let pattern = if input.literal.unwrap_or(false) {
            regex::escape(&input.pattern)
        } else {
            input.pattern.clone()
        };
        let regex = RegexBuilder::new(&pattern)
            .case_insensitive(input.ignore_case.unwrap_or(false))
            .build()
            .map_err(|e| ToolError::failed(format!("Invalid pattern: {e}")))?;

        let context_value = input.context.unwrap_or(0).max(0) as usize;
        let effective_limit = input
            .limit
            .map(|limit| limit.max(1) as usize)
            .unwrap_or(DEFAULT_LIMIT);

        let root = Path::new(&search_path);
        let is_directory = root.is_dir();
        let files = candidate_files(root, input.glob.as_deref()).map_err(ToolError::failed)?;

        let mut output_lines: Vec<String> = Vec::new();
        let mut match_count = 0usize;
        let mut lines_truncated = false;
        let mut match_limit_reached = false;

        'files: for file in &files {
            check_tool_abort(&abort)?;
            let Ok(bytes) = std::fs::read(file) else {
                continue;
            };
            if looks_binary(&bytes) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes)
                .replace("\r\n", "\n")
                .replace('\r', "\n");
            let lines: Vec<&str> = text.split('\n').collect();
            let display_path = if is_directory {
                relativize_result_path(file, root)
            } else {
                file.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            };

            for (index, line) in lines.iter().enumerate() {
                if !regex.is_match(line) {
                    continue;
                }
                let line_number = index + 1;
                match_count += 1;

                if context_value == 0 {
                    let (text, was_truncated) = truncate_line(line, GREP_MAX_LINE_LENGTH);
                    lines_truncated |= was_truncated;
                    output_lines.push(format!("{display_path}:{line_number}: {text}"));
                } else {
                    let start = line_number.saturating_sub(context_value).max(1);
                    let end = (line_number + context_value).min(lines.len());
                    for current in start..=end {
                        let line_text = lines.get(current - 1).copied().unwrap_or("");
                        let (text, was_truncated) = truncate_line(line_text, GREP_MAX_LINE_LENGTH);
                        lines_truncated |= was_truncated;
                        if current == line_number {
                            output_lines.push(format!("{display_path}:{current}: {text}"));
                        } else {
                            output_lines.push(format!("{display_path}-{current}- {text}"));
                        }
                    }
                }

                if match_count >= effective_limit {
                    match_limit_reached = true;
                    break 'files;
                }
            }
        }

        if match_count == 0 {
            return Ok(ToolResult::text("No matches found"));
        }

        // No line limit: the match limit already capped the number of rows.
        let truncation = truncate_head(
            &output_lines.join("\n"),
            TruncationOptions::bytes_only(DEFAULT_MAX_BYTES),
        );
        let mut output = truncation.content.clone();
        let mut details = GrepToolDetails {
            truncation: None,
            match_limit_reached: None,
            lines_truncated: None,
        };
        let mut notices: Vec<String> = Vec::new();
        if match_limit_reached {
            notices.push(format!(
                "{effective_limit} matches limit reached. Use limit={} for more, or refine pattern",
                effective_limit * 2
            ));
            details.match_limit_reached = Some(effective_limit);
        }
        if truncation.truncated {
            notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
            details.truncation = Some(truncation);
        }
        if lines_truncated {
            notices.push(format!(
                "Some lines truncated to {GREP_MAX_LINE_LENGTH} chars. Use read tool to see full lines"
            ));
            details.lines_truncated = Some(true);
        }
        if !notices.is_empty() {
            output.push_str(&format!("\n\n[{}]", notices.join(". ")));
        }

        let has_details = details.truncation.is_some()
            || details.match_limit_reached.is_some()
            || details.lines_truncated.is_some();
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
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("build")).unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "fn main() {\n    let needle = 1;\n    println!(\"{needle}\");\n}\n",
        )
        .unwrap();
        std::fs::write(root.join("src/other.rs"), "// NEEDLE in a comment\n").unwrap();
        std::fs::write(root.join("notes.md"), "no match here\n").unwrap();
        std::fs::write(root.join("build/gen.rs"), "let needle = 2;\n").unwrap();
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
    async fn finds_matches_with_paths_and_line_numbers() {
        let dir = temp_repo();
        let context = context_for(&dir);

        let result = GrepTool::new()
            .execute(json!({ "pattern": "needle" }), &context, None)
            .await
            .unwrap();

        let output = result.text_output();
        assert!(output.contains("src/main.rs:2:     let needle = 1;"));
        assert!(output.contains("src/main.rs:3:"));
        // .gitignore is respected.
        assert!(!output.contains("build/gen.rs"));
        // Case sensitive by default.
        assert!(!output.contains("other.rs"));
    }

    #[tokio::test]
    async fn supports_case_insensitive_search() {
        let dir = temp_repo();
        let context = context_for(&dir);

        let result = GrepTool::new()
            .execute(
                json!({ "pattern": "needle", "ignoreCase": true }),
                &context,
                None,
            )
            .await
            .unwrap();

        assert!(result.text_output().contains("src/other.rs:1:"));
    }

    #[tokio::test]
    async fn supports_literal_patterns() {
        let dir = temp_repo();
        std::fs::write(dir.path().join("src/re.rs"), "a.b\n").unwrap();
        let context = context_for(&dir);

        let literal = GrepTool::new()
            .execute(
                json!({ "pattern": "a.b", "literal": true, "glob": "re.rs" }),
                &context,
                None,
            )
            .await
            .unwrap();
        assert!(literal.text_output().contains("re.rs:1: a.b"));

        std::fs::write(dir.path().join("src/re.rs"), "axb\n").unwrap();
        let literal = GrepTool::new()
            .execute(
                json!({ "pattern": "a.b", "literal": true, "glob": "re.rs" }),
                &context,
                None,
            )
            .await
            .unwrap();
        assert_eq!(literal.text_output(), "No matches found");
    }

    #[tokio::test]
    async fn filters_by_glob() {
        let dir = temp_repo();
        let context = context_for(&dir);

        let result = GrepTool::new()
            .execute(
                json!({ "pattern": "match", "glob": "*.md" }),
                &context,
                None,
            )
            .await
            .unwrap();

        assert_eq!(result.text_output(), "notes.md:1: no match here");
    }

    #[tokio::test]
    async fn emits_context_lines() {
        let dir = temp_repo();
        let context = context_for(&dir);

        let result = GrepTool::new()
            .execute(
                json!({ "pattern": "let needle", "context": 1, "glob": "main.rs" }),
                &context,
                None,
            )
            .await
            .unwrap();

        let output = result.text_output();
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines[0], "src/main.rs-1- fn main() {");
        assert_eq!(lines[1], "src/main.rs:2:     let needle = 1;");
        assert!(lines[2].starts_with("src/main.rs-3-"));
    }

    #[tokio::test]
    async fn reports_the_match_limit() {
        let dir = temp_repo();
        let context = context_for(&dir);

        let result = GrepTool::new()
            .execute(json!({ "pattern": "needle", "limit": 1 }), &context, None)
            .await
            .unwrap();

        assert!(result
            .text_output()
            .contains("[1 matches limit reached. Use limit=2 for more, or refine pattern]"));
        let details: GrepToolDetails =
            serde_json::from_value(result.details.clone().unwrap()).unwrap();
        assert_eq!(details.match_limit_reached, Some(1));
    }

    #[tokio::test]
    async fn truncates_long_lines_and_says_so() {
        let dir = temp_repo();
        std::fs::write(
            dir.path().join("src/long.rs"),
            format!("needle{}\n", "x".repeat(1000)),
        )
        .unwrap();
        let context = context_for(&dir);

        let result = GrepTool::new()
            .execute(
                json!({ "pattern": "needle", "glob": "long.rs" }),
                &context,
                None,
            )
            .await
            .unwrap();

        assert!(result.text_output().contains("... [truncated]"));
        let details: GrepToolDetails =
            serde_json::from_value(result.details.clone().unwrap()).unwrap();
        assert_eq!(details.lines_truncated, Some(true));
    }

    #[tokio::test]
    async fn reports_no_matches() {
        let dir = temp_repo();
        let context = context_for(&dir);

        let result = GrepTool::new()
            .execute(json!({ "pattern": "zzz-not-there" }), &context, None)
            .await
            .unwrap();

        assert_eq!(result.text_output(), "No matches found");
        assert!(result.details.is_none());
    }

    #[tokio::test]
    async fn searches_a_single_file_by_basename() {
        let dir = temp_repo();
        let context = context_for(&dir);

        let result = GrepTool::new()
            .execute(
                json!({ "pattern": "needle", "path": "src/main.rs" }),
                &context,
                None,
            )
            .await
            .unwrap();

        assert!(result.text_output().starts_with("main.rs:2:"));
    }

    #[tokio::test]
    async fn rejects_invalid_patterns() {
        let dir = temp_repo();
        let context = context_for(&dir);

        let error = GrepTool::new()
            .execute(json!({ "pattern": "(" }), &context, None)
            .await
            .unwrap_err();
        assert!(error.message().contains("Invalid pattern"));
    }
}
