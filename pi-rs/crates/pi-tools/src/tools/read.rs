//! The `read` tool.
//!
//! Port of `.upstream/packages/agent/src/harness/tools/read.ts`.

use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{AbortSignal, InputContent};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{ToolError, ToolResultOf};
use crate::path_utils::resolve_read_tool_path;
use crate::tool::{parse_args, AgentTool, ToolContext, ToolResult};
use crate::tools::image::{detect_supported_image_mime_type, encode_base64};
use crate::truncate::{
    format_size, truncate_head, TruncatedBy, TruncationOptions, TruncationResult,
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolInput {
    pub path: String,
    /// 1-indexed line to start from.
    #[serde(default)]
    pub offset: Option<i64>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
}

/// Outcome of an application-supplied image conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageProcessorResult {
    Processed {
        data: String,
        mime_type: String,
        /// Extra lines appended to the text block, e.g. a conversion notice.
        hints: Vec<String>,
    },
    Failed {
        message: String,
    },
}

/// Optional image conversion/resizing hook. The SDK ships no codec, so BMP and
/// oversized images are only handled when the host installs one.
#[async_trait]
pub trait ImageProcessor: Send + Sync + 'static {
    async fn process(
        &self,
        bytes: &[u8],
        mime_type: &str,
        auto_resize_images: bool,
    ) -> ImageProcessorResult;
}

#[derive(Clone, Default)]
pub struct ReadToolOptions {
    /// Whether an injected processor should resize images. Upstream default: true.
    pub auto_resize_images: Option<bool>,
    pub image_processor: Option<Arc<dyn ImageProcessor>>,
}

impl std::fmt::Debug for ReadToolOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadToolOptions")
            .field("auto_resize_images", &self.auto_resize_images)
            .field("has_image_processor", &self.image_processor.is_some())
            .finish()
    }
}

#[derive(Clone, Default)]
pub struct ReadTool {
    options: ReadToolOptions,
}

impl ReadTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_options(options: ReadToolOptions) -> Self {
        Self { options }
    }
}

#[async_trait]
impl AgentTool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> String {
        format!(
            "Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). \
Images are sent as attachments. For text files, output is truncated to {DEFAULT_MAX_LINES} lines or {}KB \
(whichever is hit first). Use offset/limit for large files. When you need the full file, continue with \
offset until complete.",
            DEFAULT_MAX_BYTES / 1024
        )
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to read (relative or absolute)" },
                "offset": { "type": "number", "description": "Line number to start reading from (1-indexed)" },
                "limit": { "type": "number", "description": "Maximum number of lines to read" }
            },
            "required": ["path"]
        })
    }

    async fn execute(
        &self,
        args: Value,
        context: &ToolContext,
        abort: Option<AbortSignal>,
    ) -> ToolResultOf<ToolResult> {
        let input: ReadToolInput = parse_args("read", args)?;
        let absolute_path =
            resolve_read_tool_path(context.env.as_ref(), &input.path, abort.clone()).await?;
        let bytes = context
            .env
            .read_binary_file(&absolute_path, abort.clone())
            .await?;

        if let Some(mime_type) = detect_supported_image_mime_type(&bytes) {
            return Ok(self.read_image(&bytes, mime_type).await);
        }

        let text_content = String::from_utf8_lossy(&bytes);
        let all_lines: Vec<&str> = text_content.split('\n').collect();
        let total_file_lines = all_lines.len();
        let start_line = input
            .offset
            .map(|offset| (offset - 1).max(0) as usize)
            .unwrap_or(0);
        let start_line_display = start_line + 1;
        if start_line >= all_lines.len() {
            return Err(ToolError::failed(format!(
                "Offset {} is beyond end of file ({} lines total)",
                input.offset.unwrap_or_default(),
                all_lines.len()
            )));
        }

        let mut user_limited_lines: Option<usize> = None;
        let selected_content = match input.limit {
            Some(limit) => {
                let end_line = (start_line + limit.max(0) as usize).min(all_lines.len());
                user_limited_lines = Some(end_line - start_line);
                all_lines[start_line..end_line].join("\n")
            }
            None => all_lines[start_line..].join("\n"),
        };

        let truncation = truncate_head(&selected_content, TruncationOptions::default());
        let mut details: Option<ReadToolDetails> = None;
        let output_text = if truncation.first_line_exceeds_limit {
            let first_line_size = format_size(all_lines[start_line].len());
            details = Some(ReadToolDetails {
                truncation: Some(truncation.clone()),
            });
            format!(
                "[Line {start_line_display} is {first_line_size}, exceeds {} limit. Use bash: sed -n '{start_line_display}p' {} | head -c {DEFAULT_MAX_BYTES}]",
                format_size(DEFAULT_MAX_BYTES),
                input.path
            )
        } else if truncation.truncated {
            let end_line_display = start_line_display + truncation.output_lines - 1;
            let next_offset = end_line_display + 1;
            let suffix = if truncation.truncated_by == Some(TruncatedBy::Lines) {
                format!(
                    "\n\n[Showing lines {start_line_display}-{end_line_display} of {total_file_lines}. Use offset={next_offset} to continue.]"
                )
            } else {
                format!(
                    "\n\n[Showing lines {start_line_display}-{end_line_display} of {total_file_lines} ({} limit). Use offset={next_offset} to continue.]",
                    format_size(DEFAULT_MAX_BYTES)
                )
            };
            let text = format!("{}{suffix}", truncation.content);
            details = Some(ReadToolDetails {
                truncation: Some(truncation),
            });
            text
        } else if user_limited_lines.is_some_and(|limited| start_line + limited < all_lines.len()) {
            let limited = user_limited_lines.unwrap_or_default();
            let remaining = all_lines.len() - (start_line + limited);
            let next_offset = start_line + limited + 1;
            format!(
                "{}\n\n[{remaining} more lines in file. Use offset={next_offset} to continue.]",
                truncation.content
            )
        } else {
            truncation.content
        };

        Ok(ToolResult {
            content: vec![InputContent::text(output_text)],
            details: details.map(|d| serde_json::to_value(d).unwrap_or(Value::Null)),
            ..Default::default()
        })
    }
}

impl ReadTool {
    async fn read_image(&self, bytes: &[u8], mime_type: &str) -> ToolResult {
        if let Some(processor) = &self.options.image_processor {
            let auto_resize = self.options.auto_resize_images.unwrap_or(true);
            return match processor.process(bytes, mime_type, auto_resize).await {
                ImageProcessorResult::Failed { message } => {
                    ToolResult::text(format!("Read image file [{mime_type}]\n{message}"))
                }
                ImageProcessorResult::Processed {
                    data,
                    mime_type,
                    hints,
                } => {
                    let hints = if hints.is_empty() {
                        String::new()
                    } else {
                        format!("\n{}", hints.join("\n"))
                    };
                    ToolResult {
                        content: vec![
                            InputContent::text(format!("Read image file [{mime_type}]{hints}")),
                            InputContent::image(data, mime_type),
                        ],
                        ..Default::default()
                    }
                }
            };
        }
        if mime_type == "image/bmp" {
            return ToolResult::text(
                "Read image file [image/bmp]\n[Image omitted: configure an imageProcessor to convert BMP images.]",
            );
        }
        ToolResult {
            content: vec![
                InputContent::text(format!("Read image file [{mime_type}]")),
                InputContent::image(encode_base64(bytes), mime_type),
            ],
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryExecutionEnv;
    use crate::tools::image::tiny_png;
    use crate::types::{ExecutionEnvRef, FileSystem};

    async fn context_with(path: &str, content: &str) -> (ExecutionEnvRef, ToolContext) {
        let env = Arc::new(MemoryExecutionEnv::new("/work"));
        env.write_text(path, content).await;
        let env: ExecutionEnvRef = env;
        let context = ToolContext::new(env.clone()).with_tool_call_id("read-1");
        (env, context)
    }

    #[tokio::test]
    async fn reads_text_with_offsets_limits_and_continuation_notices() {
        let content = (1..=100)
            .map(|i| format!("Line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (_env, context) = context_with("test.txt", &content).await;

        let result = ReadTool::new()
            .execute(
                json!({ "path": "test.txt", "offset": 41, "limit": 20 }),
                &context,
                None,
            )
            .await
            .unwrap();
        let output = result.text_output();

        assert!(!output.contains("Line 40"));
        assert!(output.contains("Line 41"));
        assert!(output.contains("Line 60"));
        assert!(!output.contains("Line 61"));
        assert!(output.contains("[40 more lines in file. Use offset=61 to continue.]"));
    }

    #[tokio::test]
    async fn truncates_large_text_by_line_count() {
        let content = (1..=2500)
            .map(|i| format!("Line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (_env, context) = context_with("large.txt", &content).await;

        let result = ReadTool::new()
            .execute(json!({ "path": "large.txt" }), &context, None)
            .await
            .unwrap();

        assert!(result
            .text_output()
            .contains("[Showing lines 1-2000 of 2500. Use offset=2001 to continue.]"));
        let details: ReadToolDetails =
            serde_json::from_value(result.details.clone().unwrap()).unwrap();
        let truncation = details.truncation.unwrap();
        assert!(truncation.truncated);
        assert_eq!(truncation.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!(truncation.total_lines, 2500);
        assert_eq!(truncation.output_lines, 2000);
    }

    #[tokio::test]
    async fn does_not_count_a_trailing_newline_as_an_extra_line_at_the_limit() {
        let content = format!("{}\n", vec!["x"; 2000].join("\n"));
        let (_env, context) = context_with("exact.txt", &content).await;

        let result = ReadTool::new()
            .execute(json!({ "path": "exact.txt" }), &context, None)
            .await
            .unwrap();

        assert!(result.details.is_none());
        assert!(!result.text_output().contains("Use offset="));
    }

    #[tokio::test]
    async fn rejects_offsets_beyond_the_file() {
        let (_env, context) = context_with("short.txt", "one\ntwo\nthree").await;

        let error = ReadTool::new()
            .execute(
                json!({ "path": "short.txt", "offset": 100 }),
                &context,
                None,
            )
            .await
            .unwrap_err();

        assert_eq!(
            error.message(),
            "Offset 100 is beyond end of file (3 lines total)"
        );
    }

    #[tokio::test]
    async fn detects_supported_images_by_content() {
        let env = Arc::new(MemoryExecutionEnv::new("/work"));
        let png = tiny_png();
        env.write_file("image.txt", &png, None).await.unwrap();
        let context = ToolContext::new(env as ExecutionEnvRef);

        let result = ReadTool::new()
            .execute(json!({ "path": "image.txt" }), &context, None)
            .await
            .unwrap();

        assert!(result.text_output().contains("Read image file [image/png]"));
        assert!(result
            .content
            .contains(&InputContent::image(encode_base64(&png), "image/png")));
    }

    struct RecordingProcessor {
        seen: parking_lot::Mutex<Option<(Vec<u8>, String, bool)>>,
    }

    #[async_trait]
    impl ImageProcessor for RecordingProcessor {
        async fn process(
            &self,
            bytes: &[u8],
            mime_type: &str,
            auto_resize_images: bool,
        ) -> ImageProcessorResult {
            *self.seen.lock() = Some((bytes.to_vec(), mime_type.to_string(), auto_resize_images));
            ImageProcessorResult::Processed {
                data: "converted".into(),
                mime_type: "image/png".into(),
                hints: vec!["[Image converted from image/bmp to image/png.]".into()],
            }
        }
    }

    #[tokio::test]
    async fn delegates_image_conversion_to_an_injected_processor() {
        let env = Arc::new(MemoryExecutionEnv::new("/work"));
        // A BMP the sniffer accepts.
        let mut bmp = vec![0u8; 58];
        bmp[0] = 0x42;
        bmp[1] = 0x4d;
        bmp[2..6].copy_from_slice(&58u32.to_le_bytes());
        bmp[10..14].copy_from_slice(&54u32.to_le_bytes());
        bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
        bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
        bmp[28..30].copy_from_slice(&24u16.to_le_bytes());
        env.write_file("image.bmp", &bmp, None).await.unwrap();
        let context = ToolContext::new(env as ExecutionEnvRef);

        let processor = Arc::new(RecordingProcessor {
            seen: parking_lot::Mutex::new(None),
        });
        let tool = ReadTool::with_options(ReadToolOptions {
            auto_resize_images: Some(false),
            image_processor: Some(processor.clone()),
        });

        let result = tool
            .execute(json!({ "path": "image.bmp" }), &context, None)
            .await
            .unwrap();

        let seen = processor.seen.lock().clone().expect("processor called");
        assert_eq!(seen.0, bmp);
        assert_eq!(seen.1, "image/bmp");
        assert!(!seen.2);
        assert!(result
            .text_output()
            .contains("[Image converted from image/bmp to image/png.]"));
        assert!(result
            .content
            .contains(&InputContent::image("converted", "image/png")));
    }

    #[tokio::test]
    async fn omits_bmp_without_a_processor() {
        let env = Arc::new(MemoryExecutionEnv::new("/work"));
        let mut bmp = vec![0u8; 58];
        bmp[0] = 0x42;
        bmp[1] = 0x4d;
        bmp[2..6].copy_from_slice(&58u32.to_le_bytes());
        bmp[10..14].copy_from_slice(&54u32.to_le_bytes());
        bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
        bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
        bmp[28..30].copy_from_slice(&24u16.to_le_bytes());
        env.write_file("image.bmp", &bmp, None).await.unwrap();
        let context = ToolContext::new(env as ExecutionEnvRef);

        let result = ReadTool::new()
            .execute(json!({ "path": "image.bmp" }), &context, None)
            .await
            .unwrap();

        assert!(result.text_output().contains("[Image omitted"));
        assert_eq!(result.content.len(), 1);
    }
}
