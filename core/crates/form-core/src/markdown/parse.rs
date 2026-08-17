//! `pulldown-cmark` events → the typed block tree.
//!
//! GFM extensions on (tables, strikethrough, task lists, footnotes) and **smart punctuation
//! off**: quotes and dashes inside technical prose and inline code must survive verbatim,
//! and a model that writes `--force` means `--force`.
//!
//! Two things happen here that a plain CommonMark renderer would not do:
//!
//! * **Sanitizing.** Raw HTML is captured as text, never interpreted, and a link whose
//!   scheme is not one the app can actually open — anything but `http`, `https`, `mailto`,
//!   `file` — loses its link and keeps its text. That is the safety half of F7.5; the
//!   rendering half is W11's.
//! * **Bare-URL autolinking.** GFM linkifies bare URLs; `pulldown-cmark` 0.12 has no flag
//!   for it (it implements only CommonMark's `<…>` autolinks), so a conservative pass over
//!   text spans does it here rather than leaving W11 to parse anything.

use std::borrow::Cow;
use std::collections::HashMap;
use std::mem;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd};

use super::{
    block, block_id, highlight, stream, BlockKind, ColumnAlign, ListItem, MarkdownBlock,
    MarkdownDoc, Span,
};

/// The only schemes the app can act on (spec 11 §2). Everything else is text.
const ALLOWED_SCHEMES: [&str; 4] = ["http", "https", "mailto", "file"];

pub(crate) fn parse_doc(text: &str, complete: bool) -> MarkdownDoc {
    let source = if complete {
        Cow::Borrowed(text)
    } else {
        stream::repair_tail(text)
    };

    let mut builder = Builder::default();
    for event in Parser::new_ext(&source, options()) {
        builder.event(event);
    }
    let mut blocks = builder.finish();
    if !complete {
        mark_partial(&mut blocks);
    }
    MarkdownDoc { blocks }
}

fn options() -> Options {
    // Smart punctuation is deliberately absent.
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
}

/// The trailing block of an incomplete document is still being written: the renderer holds
/// back its chrome until it settles. Its id is recomputed so the block re-renders when it
/// stops being partial.
fn mark_partial(blocks: &mut [MarkdownBlock]) {
    let index = blocks.len().wrapping_sub(1);
    let Some(last) = blocks.last_mut() else {
        return;
    };
    last.partial = true;
    last.id = block_id(index, &last.kind, true);
}

// ---------------------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------------------

/// A container that collects blocks. `Table` collects nothing but keeps its state on the
/// same stack so nesting a table inside a quote or list item needs no special case.
enum FrameKind {
    Root,
    Quote,
    List {
        ordered: bool,
        start: u64,
        tight: bool,
        items: Vec<ListItem>,
    },
    Item {
        checked: Option<bool>,
    },
    Footnote {
        label: String,
    },
    Table(TableState),
}

struct Frame {
    kind: FrameKind,
    blocks: Vec<MarkdownBlock>,
}

struct TableState {
    align: Vec<ColumnAlign>,
    header: Vec<Vec<Span>>,
    rows: Vec<Vec<Vec<Span>>>,
    row: Vec<Vec<Span>>,
    in_head: bool,
}

enum SpanKind {
    /// `implicit` marks a paragraph the parser never announced — the content of a tight
    /// list item arrives as bare inlines.
    Paragraph {
        implicit: bool,
    },
    Heading(u8),
    Emphasis,
    Strong,
    Strike,
    Link {
        url: String,
        title: Option<String>,
    },
    /// A link whose scheme is not allowed: its children survive, the link does not.
    Downgraded,
    Image {
        url: Option<String>,
        title: Option<String>,
    },
    Cell,
}

struct SpanFrame {
    kind: SpanKind,
    spans: Vec<Span>,
}

struct CodeAccum {
    language: Option<String>,
    code: String,
}

struct Builder {
    frames: Vec<Frame>,
    spans: Vec<SpanFrame>,
    code: Option<CodeAccum>,
    html: Option<String>,
    anchors: HashMap<String, u32>,
}

impl Default for Builder {
    fn default() -> Self {
        Self {
            frames: vec![Frame {
                kind: FrameKind::Root,
                blocks: Vec::new(),
            }],
            spans: Vec::new(),
            code: None,
            html: None,
            anchors: HashMap::new(),
        }
    }
}

impl Builder {
    fn finish(mut self) -> Vec<MarkdownBlock> {
        self.close_implicit_paragraph();
        // Unbalanced events cannot happen — pulldown-cmark guarantees balance — but an
        // unwound stack is still better than a panic in a query.
        while self.frames.len() > 1 {
            let frame = self.frames.remove(self.frames.len() - 1);
            for kind in frame.blocks.into_iter().map(|b| b.kind) {
                self.push_block(kind);
            }
        }
        self.frames.pop().map(|f| f.blocks).unwrap_or_default()
    }

    fn event(&mut self, event: Event) {
        if self.code.is_some() {
            match event {
                Event::Text(text) => {
                    if let Some(code) = self.code.as_mut() {
                        code.code.push_str(&text);
                    }
                }
                Event::End(TagEnd::CodeBlock) => self.end_code(),
                _ => {}
            }
            return;
        }
        if self.html.is_some() {
            match event {
                Event::Html(raw) | Event::Text(raw) => {
                    if let Some(html) = self.html.as_mut() {
                        html.push_str(&raw);
                    }
                }
                Event::End(TagEnd::HtmlBlock) => self.end_html(),
                _ => {}
            }
            return;
        }

        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(text) => self.push_span(Span::Code {
                text: text.into_string(),
            }),
            // Inline HTML is kept verbatim as text; W11 escapes it. Nothing interprets it.
            Event::InlineHtml(raw) => self.text(&raw),
            Event::Html(raw) => {
                self.close_implicit_paragraph();
                self.push_block(BlockKind::Html {
                    raw: raw.into_string(),
                });
            }
            Event::FootnoteReference(label) => self.push_span(Span::FootnoteRef {
                label: label.into_string(),
            }),
            Event::SoftBreak => self.push_span(Span::Break { hard: false }),
            Event::HardBreak => self.push_span(Span::Break { hard: true }),
            Event::Rule => {
                self.close_implicit_paragraph();
                self.push_block(BlockKind::Rule);
            }
            Event::TaskListMarker(checked) => self.set_checked(checked),
            // Math is not enabled; if it ever is, the source is better than nothing.
            Event::InlineMath(text) | Event::DisplayMath(text) => self.text(&text),
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {
                self.mark_loose();
                self.spans
                    .push(SpanFrame::new(SpanKind::Paragraph { implicit: false }));
            }
            Tag::Heading { level, .. } => {
                self.spans
                    .push(SpanFrame::new(SpanKind::Heading(level as u8)));
            }
            Tag::BlockQuote(_) => self.push_frame(FrameKind::Quote),
            Tag::CodeBlock(kind) => {
                self.close_implicit_paragraph();
                let language = match kind {
                    CodeBlockKind::Fenced(info) => info
                        .split_whitespace()
                        .next()
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                    CodeBlockKind::Indented => None,
                };
                self.code = Some(CodeAccum {
                    language,
                    code: String::new(),
                });
            }
            Tag::HtmlBlock => {
                self.close_implicit_paragraph();
                self.html = Some(String::new());
            }
            Tag::List(start) => {
                self.close_implicit_paragraph();
                self.push_frame(FrameKind::List {
                    ordered: start.is_some(),
                    start: start.unwrap_or(1),
                    tight: true,
                    items: Vec::new(),
                });
            }
            Tag::Item => self.push_frame(FrameKind::Item { checked: None }),
            Tag::FootnoteDefinition(label) => self.push_frame(FrameKind::Footnote {
                label: label.into_string(),
            }),
            Tag::Table(aligns) => {
                self.close_implicit_paragraph();
                self.push_frame(FrameKind::Table(TableState {
                    align: aligns.into_iter().map(column_align).collect(),
                    header: Vec::new(),
                    rows: Vec::new(),
                    row: Vec::new(),
                    in_head: false,
                }));
            }
            Tag::TableHead => {
                if let Some(table) = self.table() {
                    table.in_head = true;
                    table.row.clear();
                }
            }
            Tag::TableRow => {
                if let Some(table) = self.table() {
                    table.row.clear();
                }
            }
            Tag::TableCell => self.spans.push(SpanFrame::new(SpanKind::Cell)),
            Tag::Emphasis => self.spans.push(SpanFrame::new(SpanKind::Emphasis)),
            Tag::Strong => self.spans.push(SpanFrame::new(SpanKind::Strong)),
            Tag::Strikethrough => self.spans.push(SpanFrame::new(SpanKind::Strike)),
            Tag::Link {
                link_type,
                dest_url,
                title,
                ..
            } => {
                // `<name@example.com>` arrives without its scheme; the app needs one to
                // hand the link to the system.
                let dest = match link_type {
                    LinkType::Email => Cow::Owned(format!("mailto:{dest_url}")),
                    _ => Cow::Borrowed(dest_url.as_ref()),
                };
                let kind = match sanitize_url(&dest) {
                    Some(url) => SpanKind::Link {
                        url,
                        title: non_empty(&title),
                    },
                    None => SpanKind::Downgraded,
                };
                self.spans.push(SpanFrame::new(kind));
            }
            Tag::Image {
                dest_url, title, ..
            } => self.spans.push(SpanFrame::new(SpanKind::Image {
                url: sanitize_url(&dest_url),
                title: non_empty(&title),
            })),
            // Definition lists, metadata blocks and math are not enabled.
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                let spans = self.pop_spans();
                self.flush_paragraph(spans);
            }
            TagEnd::Heading(_) => {
                let frame = self.spans.pop();
                let Some(frame) = frame else { return };
                let SpanKind::Heading(level) = frame.kind else {
                    return;
                };
                let mut spans = frame.spans;
                trim_spans(&mut spans);
                linkify(&mut spans);
                let anchor = self.anchor(&spans);
                self.push_block(BlockKind::Heading {
                    level,
                    spans,
                    anchor,
                });
            }
            TagEnd::BlockQuote(_) => {
                self.close_implicit_paragraph();
                if let Some(frame) = self.pop_frame(|k| matches!(k, FrameKind::Quote)) {
                    self.push_block(BlockKind::Quote {
                        blocks: frame.blocks,
                    });
                }
            }
            TagEnd::List(_) => {
                if let Some(frame) = self.pop_frame(|k| matches!(k, FrameKind::List { .. })) {
                    let FrameKind::List {
                        ordered,
                        start,
                        tight,
                        items,
                    } = frame.kind
                    else {
                        return;
                    };
                    self.push_block(BlockKind::List {
                        ordered,
                        start,
                        tight,
                        items,
                    });
                }
            }
            TagEnd::Item => {
                self.close_implicit_paragraph();
                let Some(frame) = self.pop_frame(|k| matches!(k, FrameKind::Item { .. })) else {
                    return;
                };
                let FrameKind::Item { checked } = frame.kind else {
                    return;
                };
                if let Some(FrameKind::List { items, .. }) =
                    self.frames.last_mut().map(|f| &mut f.kind)
                {
                    items.push(ListItem {
                        checked,
                        blocks: frame.blocks,
                    });
                }
            }
            TagEnd::FootnoteDefinition => {
                self.close_implicit_paragraph();
                if let Some(frame) = self.pop_frame(|k| matches!(k, FrameKind::Footnote { .. })) {
                    let FrameKind::Footnote { label } = frame.kind else {
                        return;
                    };
                    self.push_block(BlockKind::FootnoteDef {
                        label,
                        blocks: frame.blocks,
                    });
                }
            }
            TagEnd::Table => {
                if let Some(frame) = self.pop_frame(|k| matches!(k, FrameKind::Table(_))) {
                    let FrameKind::Table(table) = frame.kind else {
                        return;
                    };
                    self.push_block(BlockKind::Table {
                        align: table.align,
                        header: table.header,
                        rows: table.rows,
                    });
                }
            }
            TagEnd::TableHead => {
                if let Some(table) = self.table() {
                    let row = mem::take(&mut table.row);
                    table.header = row;
                    table.in_head = false;
                }
            }
            TagEnd::TableRow => {
                if let Some(table) = self.table() {
                    let row = mem::take(&mut table.row);
                    if table.in_head {
                        table.header = row;
                    } else {
                        table.rows.push(row);
                    }
                }
            }
            TagEnd::TableCell => {
                let mut spans = self.pop_spans();
                trim_spans(&mut spans);
                linkify(&mut spans);
                if let Some(table) = self.table() {
                    table.row.push(spans);
                }
            }
            TagEnd::Emphasis => self.wrap(|spans| Span::Emphasis { spans }),
            TagEnd::Strong => self.wrap(|spans| Span::Strong { spans }),
            TagEnd::Strikethrough => self.wrap(|spans| Span::Strike { spans }),
            TagEnd::Link => {
                let Some(frame) = self.spans.pop() else {
                    return;
                };
                match frame.kind {
                    SpanKind::Link { url, title } => self.push_span(Span::Link {
                        url,
                        title,
                        spans: frame.spans,
                    }),
                    // Downgraded: the text stays, the link is gone.
                    _ => self.splice(frame.spans),
                }
            }
            TagEnd::Image => self.end_image(),
            _ => {}
        }
    }

    // -- blocks -------------------------------------------------------------------------

    fn push_frame(&mut self, kind: FrameKind) {
        self.frames.push(Frame {
            kind,
            blocks: Vec::new(),
        });
    }

    fn pop_frame(&mut self, matches: fn(&FrameKind) -> bool) -> Option<Frame> {
        if self.frames.len() > 1 && matches(&self.frames[self.frames.len() - 1].kind) {
            self.frames.pop()
        } else {
            None
        }
    }

    fn push_block(&mut self, kind: BlockKind) {
        if let Some(frame) = self.frames.last_mut() {
            let index = frame.blocks.len();
            frame.blocks.push(block(index, kind));
        }
    }

    fn table(&mut self) -> Option<&mut TableState> {
        self.frames
            .iter_mut()
            .rev()
            .find_map(|f| match &mut f.kind {
                FrameKind::Table(t) => Some(t),
                _ => None,
            })
    }

    /// A paragraph announced directly inside a list item means the list is loose.
    fn mark_loose(&mut self) {
        if !matches!(
            self.frames.last().map(|f| &f.kind),
            Some(FrameKind::Item { .. })
        ) {
            return;
        }
        let parent = self.frames.len().wrapping_sub(2);
        if let Some(FrameKind::List { tight, .. }) =
            self.frames.get_mut(parent).map(|f| &mut f.kind)
        {
            *tight = false;
        }
    }

    fn end_code(&mut self) {
        let Some(accum) = self.code.take() else {
            return;
        };
        let tokens = highlight::tokens(accum.language.as_deref(), &accum.code);
        self.push_block(BlockKind::CodeBlock {
            language: accum.language,
            code: accum.code,
            tokens,
        });
    }

    fn end_html(&mut self) {
        let Some(raw) = self.html.take() else { return };
        if raw.trim().is_empty() {
            return;
        }
        self.push_block(BlockKind::Html { raw });
    }

    fn flush_paragraph(&mut self, mut spans: Vec<Span>) {
        trim_spans(&mut spans);
        if spans.is_empty() {
            return;
        }
        linkify(&mut spans);
        self.push_block(BlockKind::Paragraph { spans });
    }

    /// The block tree has no inline image, because an image is a layout object with its own
    /// size and placeholder, not a glyph. An image alone in a paragraph becomes an `Image`
    /// block; one sitting inside a sentence splits the paragraph around it. Deeper than
    /// that — inside a link or a heading — only its alt text survives, which is the one
    /// case the tree genuinely cannot express.
    fn end_image(&mut self) {
        let Some(frame) = self.spans.pop() else {
            return;
        };
        let SpanKind::Image { url, title } = frame.kind else {
            return;
        };
        let alt = plain_text(&frame.spans);
        let Some(url) = url else {
            self.text(&alt);
            return;
        };
        let splittable =
            self.spans.len() == 1 && matches!(self.spans[0].kind, SpanKind::Paragraph { .. });
        if !splittable {
            self.text(&alt);
            return;
        }
        let before = mem::take(&mut self.spans[0].spans);
        self.flush_paragraph(before);
        self.push_block(BlockKind::Image { url, alt, title });
    }

    // -- spans --------------------------------------------------------------------------

    /// Inline content can arrive with no paragraph announced (a tight list item). Open one
    /// implicitly rather than dropping the text.
    fn spans_mut(&mut self) -> &mut Vec<Span> {
        if self.spans.is_empty() {
            self.spans
                .push(SpanFrame::new(SpanKind::Paragraph { implicit: true }));
        }
        let last = self.spans.len() - 1;
        &mut self.spans[last].spans
    }

    fn push_span(&mut self, span: Span) {
        self.spans_mut().push(span);
    }

    fn text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let spans = self.spans_mut();
        // pulldown-cmark splits text at entity and escape boundaries; one run per literal
        // run of prose keeps the tree (and its hash) stable and small.
        if let Some(Span::Text { text: last }) = spans.last_mut() {
            last.push_str(text);
        } else {
            spans.push(Span::Text {
                text: text.to_string(),
            });
        }
    }

    fn pop_spans(&mut self) -> Vec<Span> {
        self.spans.pop().map(|f| f.spans).unwrap_or_default()
    }

    fn wrap(&mut self, build: fn(Vec<Span>) -> Span) {
        let spans = self.pop_spans();
        if spans.is_empty() {
            return;
        }
        self.push_span(build(spans));
    }

    fn splice(&mut self, spans: Vec<Span>) {
        for span in spans {
            match span {
                Span::Text { text } => self.text(&text),
                other => self.push_span(other),
            }
        }
    }

    fn close_implicit_paragraph(&mut self) {
        if matches!(
            self.spans.last().map(|f| &f.kind),
            Some(SpanKind::Paragraph { implicit: true })
        ) {
            let spans = self.pop_spans();
            self.flush_paragraph(spans);
        }
    }

    fn set_checked(&mut self, checked: bool) {
        if let Some(FrameKind::Item { checked: slot }) = self.frames.last_mut().map(|f| &mut f.kind)
        {
            *slot = Some(checked);
        }
    }

    /// GitHub-style slug, deduplicated per document so two `## Usage` headings do not
    /// collide.
    fn anchor(&mut self, spans: &[Span]) -> String {
        let base = slug(&plain_text(spans));
        let count = self.anchors.entry(base.clone()).or_insert(0);
        *count += 1;
        if *count == 1 {
            base
        } else {
            format!("{base}-{}", *count)
        }
    }
}

impl SpanFrame {
    fn new(kind: SpanKind) -> Self {
        Self {
            kind,
            spans: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------

fn column_align(alignment: Alignment) -> ColumnAlign {
    match alignment {
        Alignment::Left => ColumnAlign::Left,
        Alignment::Center => ColumnAlign::Center,
        Alignment::Right => ColumnAlign::Right,
        Alignment::None => ColumnAlign::None,
    }
}

fn non_empty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_string())
}

/// A trailing soft break is the newline before a block boundary; it is not content.
fn trim_spans(spans: &mut Vec<Span>) {
    while matches!(spans.last(), Some(Span::Break { .. })) {
        spans.pop();
    }
    while matches!(spans.first(), Some(Span::Break { .. })) {
        spans.remove(0);
    }
}

fn plain_text(spans: &[Span]) -> String {
    let mut out = String::new();
    write_plain(spans, &mut out);
    out
}

fn write_plain(spans: &[Span], out: &mut String) {
    for span in spans {
        match span {
            Span::Text { text } | Span::Code { text } => out.push_str(text),
            Span::Emphasis { spans } | Span::Strong { spans } | Span::Strike { spans } => {
                write_plain(spans, out);
            }
            Span::Link { spans, .. } => write_plain(spans, out),
            Span::FootnoteRef { .. } => {}
            Span::Break { .. } => out.push(' '),
        }
    }
}

fn slug(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
        } else if ch == '-' || ch == '_' {
            out.push(ch);
        } else if ch.is_whitespace() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// Accept only what the app can act on. The scheme is read from a copy with whitespace and
/// control characters removed, so `java\tscript:` cannot smuggle itself past the check; the
/// URL that survives keeps its spaces (a `file://` path may legitimately contain them) but
/// never its control characters.
fn sanitize_url(raw: &str) -> Option<String> {
    let cleaned: String = raw.chars().filter(|c| !c.is_control()).collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return None;
    }
    let compact: String = cleaned.chars().filter(|c| !c.is_whitespace()).collect();
    let end = compact.find([':', '/', '?', '#'])?;
    if compact.as_bytes().get(end) != Some(&b':') {
        return None; // schemeless: nothing here can resolve it
    }
    let scheme = &compact[..end];
    let valid = !scheme.is_empty()
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'));
    if !valid {
        return None;
    }
    ALLOWED_SCHEMES
        .iter()
        .any(|s| scheme.eq_ignore_ascii_case(s))
        .then(|| cleaned.to_string())
}

// ---------------------------------------------------------------------------------------
// Bare-URL autolinking
// ---------------------------------------------------------------------------------------

fn linkify(spans: &mut Vec<Span>) {
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for span in mem::take(spans) {
        match span {
            Span::Text { text } => match split_urls(&text) {
                Some(parts) => out.extend(parts),
                None => out.push(Span::Text { text }),
            },
            Span::Emphasis { mut spans } => {
                linkify(&mut spans);
                out.push(Span::Emphasis { spans });
            }
            Span::Strong { mut spans } => {
                linkify(&mut spans);
                out.push(Span::Strong { spans });
            }
            Span::Strike { mut spans } => {
                linkify(&mut spans);
                out.push(Span::Strike { spans });
            }
            // Already a link, or not text: leave it alone.
            other => out.push(other),
        }
    }
    *spans = out;
}

fn split_urls(text: &str) -> Option<Vec<Span>> {
    let mut out: Vec<Span> = Vec::new();
    let mut rest = text;
    let mut consumed = 0usize;
    while let Some(found) = find_url(rest) {
        let (start, end) = found;
        let url = &rest[start..end];
        if start > 0 {
            out.push(Span::Text {
                text: rest[..start].to_string(),
            });
        }
        let target = if url.starts_with("www.") {
            format!("https://{url}")
        } else {
            url.to_string()
        };
        out.push(Span::Link {
            url: target,
            title: None,
            spans: vec![Span::Text {
                text: url.to_string(),
            }],
        });
        rest = &rest[end..];
        consumed += 1;
    }
    if consumed == 0 {
        return None;
    }
    if !rest.is_empty() {
        out.push(Span::Text {
            text: rest.to_string(),
        });
    }
    Some(out)
}

fn find_url(text: &str) -> Option<(usize, usize)> {
    const PREFIXES: [&str; 3] = ["https://", "http://", "www."];
    let mut best: Option<(usize, usize)> = None;
    for prefix in PREFIXES {
        let mut from = 0usize;
        while let Some(rel) = text[from..].find(prefix) {
            let start = from + rel;
            from = start + 1;
            // Must begin at a word boundary, or it is the tail of some longer token.
            let preceded_ok = text[..start]
                .chars()
                .next_back()
                .is_none_or(|c| c.is_whitespace() || "([{<\"'".contains(c));
            if !preceded_ok {
                continue;
            }
            let end = url_end(text, start);
            if end <= start + prefix.len() {
                continue;
            }
            if best.is_none_or(|(b, _)| start < b) {
                best = Some((start, end));
            }
            break;
        }
    }
    best
}

/// Consume to whitespace, then give back trailing punctuation that reads as sentence
/// punctuation rather than part of the URL.
fn url_end(text: &str, start: usize) -> usize {
    let tail = &text[start..];
    let mut end = tail
        .find(|c: char| c.is_whitespace() || c == '<' || c == '>')
        .unwrap_or(tail.len());
    while end > 0 {
        let ch = tail[..end].chars().next_back().unwrap_or(' ');
        let trim = match ch {
            '.' | ',' | ';' | ':' | '!' | '?' | '"' | '\'' => true,
            ')' => tail[..end].matches('(').count() < tail[..end].matches(')').count(),
            ']' => tail[..end].matches('[').count() < tail[..end].matches(']').count(),
            _ => false,
        };
        if !trim {
            break;
        }
        end -= ch.len_utf8();
    }
    start + end
}
