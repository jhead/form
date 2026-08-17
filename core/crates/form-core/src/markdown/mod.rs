//! Markdown → typed block tree, and code → scoped syntax tokens.
//!
//! **Owner: W5** (`docs/specs/05-markdown-core.md`).
//!
//! Parsing lives in Rust so three future platforms share one implementation. **Colors never
//! appear here** — only `syntect` scope names, which `FormDesign` maps onto the active
//! theme.
//!
//! Two entry points: [`parse`] for a finished document and [`parse_streaming`] for one that
//! may end mid-construct. The streaming path repairs the trailing partial construct
//! ([`stream`]) so an unterminated fence, a half-written table or a dangling `**` renders as
//! what it is about to become instead of flickering through its raw source (F7.3).
//!
//! Block ids are `(index, content hash)`. The hash covers the block's serialized content, so
//! an id changes exactly when the block's rendering would — which is what lets SwiftUI's
//! `ForEach` keep identity for everything but the tail of a growing document.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod highlight;
mod parse;
mod stream;

#[cfg(test)]
mod tests;

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
        #[serde(default, skip_serializing_if = "Option::is_none")]
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

/// Offsets are **UTF-16 code units**, not bytes, so Swift can apply them straight to an
/// `AttributedString` without re-encoding the code first. Tested with emoji and CJK.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
        /// True while the fence is still being written: the renderer holds back the copy
        /// button and the rest of the block's chrome until the text stops moving.
        #[serde(default)]
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
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

/// Parse a finished document.
pub fn parse(text: &str) -> MarkdownDoc {
    parse_streaming(text, true)
}

/// Parse a document that may end mid-construct.
///
/// With `complete: false` the trailing construct is repaired before parsing, and a trailing
/// code block is marked `partial` so the renderer holds back its chrome.
pub fn parse_streaming(text: &str, complete: bool) -> MarkdownDoc {
    parse::parse_doc(text, complete)
}

/// Identity is `(index, content hash)` — stable while a block's content is unchanged, so a
/// growing document only invalidates its tail. The hash is over the serialized block, which
/// is exactly the input the renderer sees. `CodeBlock`'s `partial` is part of that content,
/// so the trailing fence changes identity — and re-renders with its chrome — the moment it
/// finishes.
fn block_id(index: usize, kind: &BlockKind) -> String {
    let mut hasher = Sha256::new();
    // Infallible: `Sha256`'s `io::Write` never errors and `BlockKind` always serializes.
    let _ = serde_json::to_writer(&mut hasher, kind);
    let digest = hasher.finalize();

    let mut id = format!("b{index}-");
    for byte in &digest[..6] {
        let _ = write!(id, "{byte:02x}");
    }
    id
}

fn block(index: usize, kind: BlockKind) -> MarkdownBlock {
    MarkdownBlock {
        id: block_id(index, &kind),
        kind,
    }
}
