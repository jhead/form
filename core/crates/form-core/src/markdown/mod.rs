//! Markdown → typed block tree, and code → scoped syntax tokens.
//!
//! **Owner: W5** (`docs/specs/05-markdown-core.md`).
//!
//! Parsing lives in Rust so three future platforms share one implementation. **Colors never
//! appear here** — only `syntect` scope names, which `FormDesign` maps onto the active
//! theme. What is here now is the type shape plus a paragraph/code-fence parse good enough
//! to render something real; W5 replaces `parse` with the full `pulldown-cmark` pass.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ColumnAlign {
    Left,
    Center,
    Right,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Span {
    Text {
        text: String,
    },
    Emphasis {
        spans: Vec<Span>,
    },
    Strong {
        spans: Vec<Span>,
    },
    Strike {
        spans: Vec<Span>,
    },
    Code {
        text: String,
    },
    Link {
        url: String,
        title: Option<String>,
        spans: Vec<Span>,
    },
    FootnoteRef {
        label: String,
    },
    Break {
        hard: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    pub blocks: Vec<MarkdownBlock>,
}

/// Byte offsets are **UTF-16 code units** so Swift can apply them to an `AttributedString`
/// without re-encoding. Tested with emoji and CJK.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeToken {
    pub start: u32,
    pub len: u32,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BlockKind {
    Paragraph {
        spans: Vec<Span>,
    },
    Heading {
        level: u8,
        spans: Vec<Span>,
        anchor: String,
    },
    CodeBlock {
        #[serde(skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        code: String,
        tokens: Vec<CodeToken>,
        partial: bool,
    },
    List {
        ordered: bool,
        start: u64,
        tight: bool,
        items: Vec<ListItem>,
    },
    Quote {
        blocks: Vec<MarkdownBlock>,
    },
    Table {
        align: Vec<ColumnAlign>,
        header: Vec<Vec<Span>>,
        rows: Vec<Vec<Vec<Span>>>,
    },
    Rule,
    Image {
        url: String,
        alt: String,
        title: Option<String>,
    },
    /// Captured, never interpreted — rendered as escaped text.
    Html {
        raw: String,
    },
    FootnoteDef {
        label: String,
        blocks: Vec<MarkdownBlock>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownBlock {
    /// Stable across incremental re-parses so SwiftUI keeps view identity while streaming.
    pub id: String,
    #[serde(flatten)]
    pub kind: BlockKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownDoc {
    pub blocks: Vec<MarkdownBlock>,
}

pub fn parse(text: &str) -> MarkdownDoc {
    parse_streaming(text, true)
}

/// TODO(W5): replace with the full `pulldown-cmark` + `syntect` implementation from spec 05.
/// This placeholder handles only paragraphs and fenced code so the renderer has real input.
pub fn parse_streaming(text: &str, complete: bool) -> MarkdownDoc {
    let mut blocks = Vec::new();
    let mut in_fence = false;
    let mut fence_lang: Option<String> = None;
    let mut buffer: Vec<&str> = Vec::new();

    let flush_paragraph = |buffer: &mut Vec<&str>, blocks: &mut Vec<MarkdownBlock>| {
        let joined = buffer.join("\n");
        buffer.clear();
        if joined.trim().is_empty() {
            return;
        }
        blocks.push(block(
            blocks.len(),
            &joined,
            BlockKind::Paragraph {
                spans: vec![Span::Text {
                    text: joined.clone(),
                }],
            },
        ));
    };

    for line in text.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("```") {
            if in_fence {
                let code = buffer.join("\n");
                buffer.clear();
                let id = block_id(blocks.len(), &code);
                blocks.push(MarkdownBlock {
                    id,
                    kind: BlockKind::CodeBlock {
                        language: fence_lang.take(),
                        code,
                        tokens: Vec::new(),
                        partial: false,
                    },
                });
                in_fence = false;
            } else {
                flush_paragraph(&mut buffer, &mut blocks);
                fence_lang = (!rest.trim().is_empty()).then(|| rest.trim().to_string());
                in_fence = true;
            }
            continue;
        }
        if in_fence {
            buffer.push(line);
        } else if line.trim().is_empty() {
            flush_paragraph(&mut buffer, &mut blocks);
        } else {
            buffer.push(line);
        }
    }

    // An unterminated fence still renders as a code block (F7.3).
    if in_fence {
        let code = buffer.join("\n");
        let id = block_id(blocks.len(), &code);
        blocks.push(MarkdownBlock {
            id,
            kind: BlockKind::CodeBlock {
                language: fence_lang,
                code,
                tokens: Vec::new(),
                partial: !complete,
            },
        });
    } else {
        flush_paragraph(&mut buffer, &mut blocks);
    }

    MarkdownDoc { blocks }
}

fn block(index: usize, content: &str, kind: BlockKind) -> MarkdownBlock {
    MarkdownBlock {
        id: block_id(index, content),
        kind,
    }
}

/// Identity is `(index, content hash)` — stable while a block's text is unchanged, so a
/// growing document only invalidates its tail.
fn block_id(index: usize, content: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    format!("b{index}-{:x}", hasher.finish())
}
