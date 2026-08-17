//! The `edit` tool.
//!
//! Port of `.upstream/packages/agent/src/harness/tools/edit.ts`. The matching
//! and diffing live in [`crate::edit_diff`].

use async_trait::async_trait;
use pi_core::AbortSignal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::edit_diff::{
    apply_edits_to_normalized_content, detect_line_ending, generate_diff_string,
    generate_unified_patch, normalize_to_lf, restore_line_endings, strip_bom, Edit,
    DEFAULT_DIFF_CONTEXT_LINES,
};
use crate::error::{FileError, ToolError, ToolResultOf};
use crate::file_mutation_queue::with_file_mutation_queue;
use crate::path_utils::resolve_tool_path;
use crate::tool::{check_tool_abort, parse_args, AgentTool, ToolContext, ToolResult};
use crate::types::FileKind;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditToolInput {
    pub path: String,
    #[serde(default)]
    pub edits: Vec<Edit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditToolDetails {
    pub diff: String,
    pub patch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_changed_line: Option<usize>,
}

#[derive(Clone, Default)]
pub struct EditTool;

impl EditTool {
    pub fn new() -> Self {
        Self
    }
}

fn edit_access_error(path: &str, error: &FileError) -> ToolError {
    ToolError::failed(format!(
        "Could not edit file: {path}. Error code: {}.",
        error.code()
    ))
}

#[async_trait]
impl AgentTool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> String {
        "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, \
non-overlapping region of the original file. If two changes affect the same block or nearby lines, \
merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions \
just to connect distant changes."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to edit (relative or absolute)" },
                "edits": {
                    "type": "array",
                    "description": "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "oldText": { "type": "string", "description": "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call." },
                            "newText": { "type": "string", "description": "Replacement text for this targeted edit." }
                        },
                        "required": ["oldText", "newText"]
                    }
                }
            },
            "required": ["path", "edits"]
        })
    }

    /// Compatibility shim for two shapes models keep producing: `edits` sent as
    /// a JSON string, and a single top-level `oldText`/`newText` pair.
    fn prepare_arguments(&self, args: Value) -> Value {
        let Value::Object(mut map) = args else {
            return args;
        };
        if let Some(Value::String(raw)) = map.get("edits") {
            if let Ok(parsed @ Value::Array(_)) = serde_json::from_str::<Value>(raw) {
                map.insert("edits".into(), parsed);
            }
        }

        let legacy_old = map
            .get("oldText")
            .and_then(Value::as_str)
            .map(str::to_string);
        let legacy_new = map
            .get("newText")
            .and_then(Value::as_str)
            .map(str::to_string);
        if let (Some(old_text), Some(new_text)) = (legacy_old, legacy_new) {
            let mut edits = match map.remove("edits") {
                Some(Value::Array(items)) => items,
                _ => Vec::new(),
            };
            edits.push(json!({ "oldText": old_text, "newText": new_text }));
            map.remove("oldText");
            map.remove("newText");
            map.insert("edits".into(), Value::Array(edits));
        }
        Value::Object(map)
    }

    async fn execute(
        &self,
        args: Value,
        context: &ToolContext,
        abort: Option<AbortSignal>,
    ) -> ToolResultOf<ToolResult> {
        let input: EditToolInput = parse_args("edit", args)?;
        if input.edits.is_empty() {
            return Err(ToolError::invalid_arguments(
                "Edit tool input is invalid. edits must contain at least one replacement.",
            ));
        }
        let path = input.path.clone();
        let edits = input.edits.clone();
        let absolute_path = resolve_tool_path(context.env.as_ref(), &path, abort.clone()).await?;

        with_file_mutation_queue(&context.env, &absolute_path, || async {
            check_tool_abort(&abort)?;
            let info = context
                .env
                .file_info(&absolute_path, abort.clone())
                .await
                .map_err(|e| edit_access_error(&path, &e))?;
            if info.kind != FileKind::File && info.kind != FileKind::Symlink {
                return Err(ToolError::failed(format!(
                    "Could not edit file: {path}. Path is not a file."
                )));
            }

            let read = context
                .env
                .read_text_file(&absolute_path, abort.clone())
                .await
                .map_err(|e| edit_access_error(&path, &e))?;
            check_tool_abort(&abort)?;

            let (bom, content) = strip_bom(&read);
            let original_ending = detect_line_ending(content);
            let normalized_content = normalize_to_lf(content);
            let applied = apply_edits_to_normalized_content(&normalized_content, &edits, &path)?;
            check_tool_abort(&abort)?;

            let final_content = format!(
                "{bom}{}",
                restore_line_endings(&applied.new_content, original_ending)
            );
            context
                .env
                .write_file(&absolute_path, final_content.as_bytes(), abort.clone())
                .await
                .map_err(|e| edit_access_error(&path, &e))?;
            check_tool_abort(&abort)?;

            let diff_result = generate_diff_string(
                &applied.base_content,
                &applied.new_content,
                DEFAULT_DIFF_CONTEXT_LINES,
            );
            let details = EditToolDetails {
                diff: diff_result.diff,
                patch: generate_unified_patch(
                    &path,
                    &applied.base_content,
                    &applied.new_content,
                    DEFAULT_DIFF_CONTEXT_LINES,
                ),
                first_changed_line: diff_result.first_changed_line,
            };
            Ok(ToolResult::text(format!(
                "Successfully replaced {} block(s) in {path}.",
                edits.len()
            ))
            .with_details(Some(serde_json::to_value(details).unwrap_or(Value::Null))))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::memory::MemoryExecutionEnv;
    use crate::types::{ExecutionEnvRef, FileSystem};

    async fn context_with(path: &str, content: &str) -> (Arc<MemoryExecutionEnv>, ToolContext) {
        let env = Arc::new(MemoryExecutionEnv::new("/work"));
        env.write_text(path, content).await;
        let context = ToolContext::new(env.clone() as ExecutionEnvRef);
        (env, context)
    }

    fn details_of(result: &ToolResult) -> EditToolDetails {
        serde_json::from_value(result.details.clone().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn applies_disjoint_edits_and_returns_both_diff_formats() {
        let (env, context) = context_with("edit.txt", "alpha\nbeta\ngamma\ndelta\n").await;

        let result = EditTool::new()
            .execute(
                json!({
                    "path": "edit.txt",
                    "edits": [
                        { "oldText": "alpha\n", "newText": "ALPHA\n" },
                        { "oldText": "gamma\n", "newText": "GAMMA\n" }
                    ]
                }),
                &context,
                None,
            )
            .await
            .unwrap();

        assert_eq!(
            result.text_output(),
            "Successfully replaced 2 block(s) in edit.txt."
        );
        let details = details_of(&result);
        assert!(details.diff.contains("ALPHA"));
        assert!(details.diff.contains("GAMMA"));
        assert!(details.patch.contains("+ALPHA"));
        assert!(details.patch.contains("-gamma"));
        assert_eq!(
            env.read_text("edit.txt").await,
            "ALPHA\nbeta\nGAMMA\ndelta\n"
        );
    }

    #[tokio::test]
    async fn matches_all_edits_against_the_original_and_rejects_overlaps() {
        let (env, context) = context_with("edit.txt", "one\ntwo\nthree\n").await;

        let error = EditTool::new()
            .execute(
                json!({
                    "path": "edit.txt",
                    "edits": [
                        { "oldText": "one\ntwo\n", "newText": "ONE\nTWO\n" },
                        { "oldText": "two\nthree\n", "newText": "TWO\nTHREE\n" }
                    ]
                }),
                &context,
                None,
            )
            .await
            .unwrap_err();

        assert!(error.message().contains("overlap"));
        assert_eq!(env.read_text("edit.txt").await, "one\ntwo\nthree\n");
    }

    #[tokio::test]
    async fn rejects_missing_and_duplicate_target_text() {
        let (_env, context) = context_with("edit.txt", "foo foo foo").await;
        let tool = EditTool::new();

        let missing = tool
            .execute(
                json!({ "path": "edit.txt", "edits": [{ "oldText": "bar", "newText": "baz" }] }),
                &context,
                None,
            )
            .await
            .unwrap_err();
        assert!(missing.message().contains("Could not find the exact text"));

        let duplicate = tool
            .execute(
                json!({ "path": "edit.txt", "edits": [{ "oldText": "foo", "newText": "bar" }] }),
                &context,
                None,
            )
            .await
            .unwrap_err();
        assert!(duplicate.message().contains("Found 3 occurrences"));
    }

    #[tokio::test]
    async fn rejects_an_empty_edit_list() {
        let (_env, context) = context_with("edit.txt", "x").await;
        let error = EditTool::new()
            .execute(json!({ "path": "edit.txt", "edits": [] }), &context, None)
            .await
            .unwrap_err();
        assert!(error.message().contains("at least one replacement"));
    }

    #[tokio::test]
    async fn edits_regular_files_through_symlinks() {
        let (env, context) = context_with("target.txt", "before\n").await;
        env.symlink("target.txt", "link.txt");

        EditTool::new()
            .execute(
                json!({ "path": "link.txt", "edits": [{ "oldText": "before", "newText": "after" }] }),
                &context,
                None,
            )
            .await
            .unwrap();

        assert_eq!(env.read_text("target.txt").await, "after\n");
    }

    /// Upstream: "serializes concurrent edits through canonical and symlink paths".
    #[tokio::test]
    async fn serializes_concurrent_edits_through_canonical_and_symlink_paths() {
        let (env, context) = context_with("target.txt", "alpha\nbeta\ngamma\n").await;
        env.symlink("target.txt", "link.txt");
        // Slow the write so the two calls would interleave without the queue.
        env.set_write_hook(Arc::new(|_path, _content| {
            Box::pin(async {
                tokio::time::sleep(Duration::from_millis(20)).await;
            })
        }));

        let first = {
            let context = context.clone();
            tokio::spawn(async move {
                EditTool::new()
                    .execute(
                        json!({ "path": "target.txt", "edits": [{ "oldText": "alpha", "newText": "ALPHA" }] }),
                        &context,
                        None,
                    )
                    .await
            })
        };
        let second = {
            let context = context.clone();
            tokio::spawn(async move {
                EditTool::new()
                    .execute(
                        json!({ "path": "link.txt", "edits": [{ "oldText": "beta", "newText": "BETA" }] }),
                        &context,
                        None,
                    )
                    .await
            })
        };
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();

        assert_eq!(env.read_text("target.txt").await, "ALPHA\nBETA\ngamma\n");
    }

    #[tokio::test]
    async fn preserves_bom_and_crlf_line_endings() {
        let (env, context) = context_with("edit.txt", "\u{FEFF}one\r\ntwo\r\n").await;

        EditTool::new()
            .execute(
                json!({ "path": "edit.txt", "edits": [{ "oldText": "two", "newText": "TWO" }] }),
                &context,
                None,
            )
            .await
            .unwrap();

        assert_eq!(env.read_text("edit.txt").await, "\u{FEFF}one\r\nTWO\r\n");
    }

    #[tokio::test]
    async fn refuses_to_edit_a_directory() {
        let env = Arc::new(MemoryExecutionEnv::new("/work"));
        env.create_dir("adir", true, None).await.unwrap();
        let context = ToolContext::new(env as ExecutionEnvRef);

        let error = EditTool::new()
            .execute(
                json!({ "path": "adir", "edits": [{ "oldText": "a", "newText": "b" }] }),
                &context,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.message(),
            "Could not edit file: adir. Path is not a file."
        );
    }

    #[tokio::test]
    async fn reports_missing_files_with_the_error_code() {
        let env = Arc::new(MemoryExecutionEnv::new("/work"));
        let context = ToolContext::new(env as ExecutionEnvRef);

        let error = EditTool::new()
            .execute(
                json!({ "path": "nope.txt", "edits": [{ "oldText": "a", "newText": "b" }] }),
                &context,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.message(),
            "Could not edit file: nope.txt. Error code: not_found."
        );
    }

    #[test]
    fn prepare_arguments_accepts_a_stringified_edit_array() {
        let prepared = EditTool::new().prepare_arguments(json!({
            "path": "a.txt",
            "edits": "[{\"oldText\":\"a\",\"newText\":\"b\"}]"
        }));
        assert_eq!(prepared["edits"][0]["oldText"], "a");
    }

    #[test]
    fn prepare_arguments_lifts_the_legacy_single_edit_shape() {
        let prepared = EditTool::new().prepare_arguments(json!({
            "path": "a.txt",
            "oldText": "a",
            "newText": "b"
        }));
        assert_eq!(prepared["edits"].as_array().unwrap().len(), 1);
        assert_eq!(prepared["edits"][0]["newText"], "b");
        assert!(prepared.get("oldText").is_none());
    }

    #[test]
    fn prepare_arguments_appends_the_legacy_pair_to_an_existing_array() {
        let prepared = EditTool::new().prepare_arguments(json!({
            "path": "a.txt",
            "edits": [{ "oldText": "x", "newText": "y" }],
            "oldText": "a",
            "newText": "b"
        }));
        let edits = prepared["edits"].as_array().unwrap();
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[1]["oldText"], "a");
    }
}
