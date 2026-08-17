use std::time::Instant;

use super::*;

// ---------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------

const KITCHEN_SINK: &str = include_str!("../../tests/fixtures/markdown/kitchen-sink.md");

fn block_kinds(doc: &MarkdownDoc) -> Vec<&'static str> {
    let mut out = Vec::new();
    collect_block_kinds(&doc.blocks, &mut out);
    out
}

fn collect_block_kinds(blocks: &[MarkdownBlock], out: &mut Vec<&'static str>) {
    for b in blocks {
        match &b.kind {
            BlockKind::Paragraph { spans } => {
                out.push("paragraph");
                collect_span_kinds_into(spans, out);
            }
            BlockKind::Heading { spans, .. } => {
                out.push("heading");
                collect_span_kinds_into(spans, out);
            }
            BlockKind::CodeBlock { .. } => out.push("codeBlock"),
            BlockKind::List { items, .. } => {
                out.push("list");
                for item in items {
                    collect_block_kinds(&item.blocks, out);
                }
            }
            BlockKind::Quote { blocks } => {
                out.push("quote");
                collect_block_kinds(blocks, out);
            }
            BlockKind::Table { header, rows, .. } => {
                out.push("table");
                for cell in header.iter().chain(rows.iter().flatten()) {
                    collect_span_kinds_into(cell, out);
                }
            }
            BlockKind::Rule => out.push("rule"),
            BlockKind::Image { .. } => out.push("image"),
            BlockKind::Html { .. } => out.push("html"),
            BlockKind::FootnoteDef { blocks, .. } => {
                out.push("footnoteDef");
                collect_block_kinds(blocks, out);
            }
        }
    }
}

fn collect_span_kinds_into(spans: &[Span], out: &mut Vec<&'static str>) {
    for s in spans {
        match s {
            Span::Text { .. } => out.push("text"),
            Span::Emphasis { spans } => {
                out.push("emphasis");
                collect_span_kinds_into(spans, out);
            }
            Span::Strong { spans } => {
                out.push("strong");
                collect_span_kinds_into(spans, out);
            }
            Span::Strike { spans } => {
                out.push("strike");
                collect_span_kinds_into(spans, out);
            }
            Span::Code { .. } => out.push("code"),
            Span::Link { spans, .. } => {
                out.push("link");
                collect_span_kinds_into(spans, out);
            }
            Span::FootnoteRef { .. } => out.push("footnoteRef"),
            Span::Break { .. } => out.push("break"),
        }
    }
}

struct Code {
    language: Option<String>,
    code: String,
    tokens: Vec<CodeToken>,
    partial: bool,
}

fn first_code_block(doc: &MarkdownDoc) -> Code {
    doc.blocks
        .iter()
        .find_map(|b| match &b.kind {
            BlockKind::CodeBlock {
                language,
                code,
                tokens,
                partial,
            } => Some(Code {
                language: language.clone(),
                code: code.clone(),
                tokens: tokens.clone(),
                partial: *partial,
            }),
            _ => None,
        })
        .expect("expected a code block")
}

fn plain_of(spans: &[Span]) -> String {
    let mut out = String::new();
    for s in spans {
        match s {
            Span::Text { text } | Span::Code { text } => out.push_str(text),
            Span::Emphasis { spans } | Span::Strong { spans } | Span::Strike { spans } => {
                out.push_str(&plain_of(spans));
            }
            Span::Link { spans, .. } => out.push_str(&plain_of(spans)),
            Span::FootnoteRef { label } => out.push_str(label),
            Span::Break { .. } => out.push(' '),
        }
    }
    out
}

fn doc_text(doc: &MarkdownDoc) -> String {
    doc.blocks
        .iter()
        .map(|b| match &b.kind {
            BlockKind::Paragraph { spans } | BlockKind::Heading { spans, .. } => plain_of(spans),
            BlockKind::CodeBlock { code, .. } | BlockKind::Html { raw: code } => code.clone(),
            other => format!("{other:?}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The whole point of `CodeToken`: Swift slices an `AttributedString` by UTF-16 units.
fn utf16_slice(text: &str, start: u32, len: u32) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let end = (start + len) as usize;
    assert!(end <= units.len(), "token {start}+{len} past end of code");
    String::from_utf16(&units[start as usize..end]).expect("token split a surrogate pair")
}

// ---------------------------------------------------------------------------------------
// Block and span coverage
// ---------------------------------------------------------------------------------------

#[test]
fn every_block_and_span_variant_is_produced() {
    let doc = parse(KITCHEN_SINK);
    let seen = block_kinds(&doc);
    for expected in [
        "paragraph",
        "heading",
        "codeBlock",
        "list",
        "quote",
        "table",
        "rule",
        "image",
        "html",
        "footnoteDef",
        "text",
        "emphasis",
        "strong",
        "strike",
        "code",
        "link",
        "footnoteRef",
        "break",
    ] {
        assert!(
            seen.contains(&expected),
            "kitchen sink produced no {expected}"
        );
    }
}

#[test]
fn every_variant_round_trips_through_json() {
    let doc = parse(KITCHEN_SINK);
    let json = serde_json::to_string(&doc).expect("serialize");
    let back: MarkdownDoc = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(doc, back);
    // Keys are camelCase and tags are internally tagged on `type` (spec 00 §1).
    assert!(json.contains(r#""type":"footnoteDef""#));
    assert!(json.contains(r#""type":"codeBlock""#));
    // Absent optionals are omitted, never null.
    assert!(!json.contains("null"));
}

#[test]
fn list_metadata_and_task_markers_survive() {
    let doc = parse("1. one\n2. two\n\n- [ ] todo\n- [x] done\n\n5. five\n\n   loose\n");
    let lists: Vec<_> = doc
        .blocks
        .iter()
        .filter_map(|b| match &b.kind {
            BlockKind::List {
                ordered,
                start,
                tight,
                items,
            } => Some((*ordered, *start, *tight, items.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(lists.len(), 3);
    assert_eq!((lists[0].0, lists[0].1, lists[0].2), (true, 1, true));
    assert_eq!(
        lists[1].3.iter().map(|i| i.checked).collect::<Vec<_>>(),
        vec![Some(false), Some(true)]
    );
    assert_eq!((lists[2].0, lists[2].1), (true, 5));
    assert!(
        !lists[2].2,
        "an item with two paragraphs makes a loose list"
    );
}

#[test]
fn headings_get_deduplicated_slugs() {
    let doc = parse("# Usage & Setup\n\n## usage & setup\n\n### Ünïcode Heading\n");
    let anchors: Vec<_> = doc
        .blocks
        .iter()
        .filter_map(|b| match &b.kind {
            BlockKind::Heading { anchor, .. } => Some(anchor.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        anchors,
        vec!["usage-setup", "usage-setup-2", "ünïcode-heading"]
    );
}

#[test]
fn smart_punctuation_is_off() {
    let doc = parse(r#"He said "no" -- really... don't"#);
    assert_eq!(doc_text(&doc), r#"He said "no" -- really... don't"#);
}

#[test]
fn hard_and_soft_breaks_are_distinguished() {
    let doc = parse("one\ntwo  \nthree\n");
    let BlockKind::Paragraph { spans } = &doc.blocks[0].kind else {
        panic!("expected a paragraph");
    };
    let breaks: Vec<bool> = spans
        .iter()
        .filter_map(|s| match s {
            Span::Break { hard } => Some(*hard),
            _ => None,
        })
        .collect();
    assert_eq!(breaks, vec![false, true]);
}

#[test]
fn bare_urls_become_links() {
    let doc = parse("see https://example.com/a?b=1, and www.example.org (fine).\n");
    let BlockKind::Paragraph { spans } = &doc.blocks[0].kind else {
        panic!("expected a paragraph");
    };
    let urls: Vec<&str> = spans
        .iter()
        .filter_map(|s| match s {
            Span::Link { url, .. } => Some(url.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        urls,
        vec!["https://example.com/a?b=1", "https://www.example.org"]
    );
    assert_eq!(
        doc_text(&doc),
        "see https://example.com/a?b=1, and www.example.org (fine)."
    );
}

#[test]
fn inline_images_split_their_paragraph() {
    let doc = parse("before ![alt](https://e.com/a.png) after\n");
    let kinds = block_kinds(&doc);
    assert_eq!(
        kinds.iter().filter(|k| **k == "image").count(),
        1,
        "the image became its own block: {kinds:?}"
    );
    assert!(matches!(doc.blocks[1].kind, BlockKind::Image { .. }));
}

// ---------------------------------------------------------------------------------------
// Safety
// ---------------------------------------------------------------------------------------

#[test]
fn disallowed_link_schemes_are_downgraded_to_text() {
    for source in [
        "[click](javascript:alert(1))",
        "[click](JaVaScRiPt:alert(1))",
        // Entity references are decoded in link destinations, so the scheme check has to
        // run on the decoded value with its whitespace stripped.
        "[click](java&#9;script:alert(1))",
        "[click](&#106;avascript:alert(1))",
        "[click](data:text/html,<script>x</script>)",
        "[click](vbscript:msgbox)",
        "[click](./relative.md)",
    ] {
        let doc = parse(source);
        let BlockKind::Paragraph { spans } = &doc.blocks[0].kind else {
            panic!("expected a paragraph for {source}");
        };
        assert!(
            !spans.iter().any(|s| matches!(s, Span::Link { .. })),
            "{source} should not survive as a link"
        );
        assert_eq!(plain_of(spans), "click", "{source} should keep its text");
    }
}

#[test]
fn allowed_link_schemes_survive() {
    for (source, expected) in [
        ("[a](https://example.com/x)", "https://example.com/x"),
        ("[a](http://example.com)", "http://example.com"),
        ("[a](mailto:x@example.com)", "mailto:x@example.com"),
        ("[a](file:///Users/x/a.txt)", "file:///Users/x/a.txt"),
        ("<https://example.com>", "https://example.com"),
        ("<x@example.com>", "mailto:x@example.com"),
    ] {
        let doc = parse(source);
        let BlockKind::Paragraph { spans } = &doc.blocks[0].kind else {
            panic!("expected a paragraph for {source}");
        };
        let url = spans.iter().find_map(|s| match s {
            Span::Link { url, .. } => Some(url.clone()),
            _ => None,
        });
        assert_eq!(url.as_deref(), Some(expected), "for {source}");
    }
}

#[test]
fn disallowed_image_urls_keep_only_their_alt_text() {
    let doc = parse("![boom](javascript:alert(1))\n");
    assert!(!block_kinds(&doc).contains(&"image"));
    assert_eq!(doc_text(&doc), "boom");
}

#[test]
fn raw_html_is_captured_and_never_interpreted() {
    let doc = parse("<div onclick=\"x()\"><script>evil()</script></div>\n\ninline <b>b</b> tag\n");
    let BlockKind::Html { raw } = &doc.blocks[0].kind else {
        panic!("expected an html block, got {:?}", doc.blocks[0].kind);
    };
    assert!(raw.contains("<script>evil()</script>"), "raw kept verbatim");
    // Inline HTML survives as text, so the renderer escapes it like any other string.
    let BlockKind::Paragraph { spans } = &doc.blocks[1].kind else {
        panic!("expected a paragraph");
    };
    assert_eq!(plain_of(spans), "inline <b>b</b> tag");
}

// ---------------------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------------------

#[test]
fn unterminated_fence_is_already_a_code_block() {
    let doc = parse_streaming("intro\n\n```rust\nfn main() {\n", false);
    let block = first_code_block(&doc);
    assert_eq!(block.language.as_deref(), Some("rust"));
    assert_eq!(block.code, "fn main() {\n");
    assert!(block.partial, "the fence is still being written");
    assert!(!block.tokens.is_empty(), "a partial fence still highlights");
}

#[test]
fn half_written_table_renders_as_a_table() {
    for (source, rows) in [
        ("| Name | Size |", 0),
        ("| Name | Size |\n|--", 0),
        ("| Name | Size |\n| a | 1 |", 1),
    ] {
        let doc = parse_streaming(source, false);
        let table = doc.blocks.iter().find_map(|b| match &b.kind {
            BlockKind::Table { header, rows, .. } => Some((header.clone(), rows.clone())),
            _ => None,
        });
        let (header, body) = table.unwrap_or_else(|| panic!("{source:?} did not become a table"));
        assert_eq!(header.len(), 2);
        assert_eq!(plain_of(&header[0]), "Name");
        assert_eq!(body.len(), rows);
    }
    // A complete parse of the same text is a paragraph — the repair is streaming-only.
    assert!(matches!(
        parse("| Name | Size |").blocks[0].kind,
        BlockKind::Paragraph { .. }
    ));
}

#[test]
fn dangling_emphasis_renders_as_emphasis() {
    let doc = parse_streaming("the **bold tex", false);
    let BlockKind::Paragraph { spans } = &doc.blocks[0].kind else {
        panic!("expected a paragraph");
    };
    assert!(
        spans.iter().any(|s| matches!(s, Span::Strong { .. })),
        "got {spans:?}"
    );
    assert_eq!(plain_of(spans), "the bold tex");
    // …and stays plain text when the document is finished.
    assert_eq!(doc_text(&parse("the **bold tex")), "the **bold tex");
}

#[test]
fn dangling_inline_code_and_emphasis_do_not_leak_markers() {
    for (source, expected) in [
        ("call `foo(", "call foo("),
        ("a *slanted", "a slanted"),
        ("a ~~struck", "a struck"),
        ("trailing **", "trailing **"),
    ] {
        assert_eq!(doc_text(&parse_streaming(source, false)), expected);
    }
}

#[test]
fn incomplete_link_keeps_its_text_and_drops_the_syntax() {
    for source in ["see [the docs](htt", "see [the docs](", "see [the docs"] {
        assert_eq!(doc_text(&parse_streaming(source, false)), "see the docs");
    }
    // An image mid-write degrades to its alt text rather than an empty box.
    assert_eq!(
        doc_text(&parse_streaming("![diagram](htt", false)),
        "diagram"
    );
}

#[test]
fn code_fence_contents_are_never_repaired() {
    let source = "```python\nx = a[1] * b ** 2  # ok\n";
    let doc = parse_streaming(source, false);
    assert_eq!(first_code_block(&doc).code, "x = a[1] * b ** 2  # ok\n");
}

#[test]
fn only_the_trailing_code_block_is_partial() {
    let doc = parse_streaming("```sh\nls\n```\n\n```sh\ncd /\n", false);
    let partials: Vec<bool> = doc
        .blocks
        .iter()
        .map(|b| match &b.kind {
            BlockKind::CodeBlock { partial, .. } => *partial,
            _ => false,
        })
        .collect();
    assert_eq!(partials, vec![false, true]);

    // A finished document never marks anything partial.
    let finished = parse("```sh\nls\n```\n\n```sh\ncd /\n```\n");
    assert!(finished
        .blocks
        .iter()
        .all(|b| !matches!(&b.kind, BlockKind::CodeBlock { partial: true, .. })));
}

// ---------------------------------------------------------------------------------------
// Stable ids
// ---------------------------------------------------------------------------------------

#[test]
fn ids_are_stable_across_incremental_parses() {
    let full = "# Title\n\nFirst paragraph.\n\n```rust\nfn a() {}\n```\n\nClosing words here.\n";
    let mut settled: Vec<String> = Vec::new();

    for end in (1..=full.len()).filter(|i| full.is_char_boundary(*i)) {
        let doc = parse_streaming(&full[..end], false);
        // Every block before the trailing one is final: once seen, its id never changes.
        let ids: Vec<String> = doc.blocks.iter().map(|b| b.id.clone()).collect();
        let stable = &ids[..ids.len().saturating_sub(1)];
        for (i, id) in stable.iter().enumerate() {
            match settled.get(i) {
                Some(known) => assert_eq!(
                    known, id,
                    "block {i} changed identity at prefix length {end}"
                ),
                None => settled.push(id.clone()),
            }
        }
    }

    let final_ids: Vec<String> = parse(full).blocks.iter().map(|b| b.id.clone()).collect();
    assert_eq!(
        final_ids[..settled.len().min(final_ids.len())],
        settled[..settled.len().min(final_ids.len())],
        "the finished document keeps the ids streaming settled on"
    );
}

#[test]
fn ids_change_only_for_the_block_that_changed() {
    let before = parse("alpha\n\nbeta\n\ngamma\n");
    let after = parse("alpha\n\nbeta\n\ngamma!\n");
    assert_eq!(before.blocks[0].id, after.blocks[0].id);
    assert_eq!(before.blocks[1].id, after.blocks[1].id);
    assert_ne!(before.blocks[2].id, after.blocks[2].id);
}

#[test]
fn ids_are_unique_within_a_container() {
    let doc = parse("same\n\nsame\n\nsame\n");
    let ids: Vec<&str> = doc.blocks.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(ids.len(), 3);
    assert!(
        ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2],
        "identical content at different indices still gets distinct ids: {ids:?}"
    );
}

#[test]
fn finishing_a_code_block_changes_its_id_so_its_chrome_appears() {
    let source = "```rust\nfn a() {}\n```\n";
    let streaming = parse_streaming(source, false);
    let finished = parse(source);
    assert_ne!(streaming.blocks[0].id, finished.blocks[0].id);
    assert!(first_code_block(&streaming).partial && !first_code_block(&finished).partial);
}

// ---------------------------------------------------------------------------------------
// Highlighting
// ---------------------------------------------------------------------------------------

#[test]
fn ranges_are_utf16_code_units_with_emoji_and_cjk() {
    // The emoji comes first on purpose: every offset after it differs between bytes,
    // scalars and UTF-16 units, which is where this is easy to get subtly wrong.
    let code = "let party = \"🎉 日本語\";\nlet cafe = \"café\";\n";
    let doc = parse(&format!("```rust\n{code}```\n"));
    let block = first_code_block(&doc);
    let tokens = &block.tokens;
    assert_eq!(block.code, code);

    // Every token must address a real UTF-16 range landing on character boundaries.
    for token in tokens {
        let text = utf16_slice(code, token.start, token.len);
        assert!(!text.is_empty(), "empty token {token:?}");
        assert!(code.contains(&text), "token {token:?} sliced to {text:?}");
    }

    let strings: Vec<&CodeToken> = tokens
        .iter()
        .filter(|t| t.scope.starts_with("string.quoted"))
        .collect();
    assert_eq!(strings.len(), 2, "both literals are scoped: {tokens:?}");
    assert_eq!(
        utf16_slice(code, strings[0].start, strings[0].len),
        "🎉 日本語"
    );
    assert_eq!(utf16_slice(code, strings[1].start, strings[1].len), "café");

    // 🎉 is one scalar but two UTF-16 units; 日 is one unit but three bytes.
    assert_eq!(strings[0].len, 6);
    assert_eq!(strings[1].len, 4);

    // The proof that these are not byte offsets: past the multi-byte text they diverge.
    let byte_start = code.rfind("café").expect("literal present") as u32;
    let utf16_start = code[..byte_start as usize].encode_utf16().count() as u32;
    assert_ne!(
        byte_start, utf16_start,
        "the fixture must exercise the difference"
    );
    assert_eq!(strings[1].start, utf16_start);
}

#[test]
fn plain_token_spans_the_whole_block_in_utf16() {
    let code = "🎉 日本語 plain text\n";
    let doc = parse(&format!("```\n{code}```\n"));
    let block = first_code_block(&doc);
    assert!(block.language.is_none());
    assert_eq!(block.tokens.len(), 1);
    assert_eq!(block.tokens[0].scope, "plain");
    assert_eq!(block.tokens[0].start, 0);
    assert_eq!(block.tokens[0].len, code.encode_utf16().count() as u32);
}

#[test]
fn unknown_language_falls_back_to_one_plain_token() {
    let doc = parse("```wingdings\nnothing knows this\n```\n");
    let block = first_code_block(&doc);
    assert_eq!(block.language.as_deref(), Some("wingdings"));
    assert_eq!(block.tokens.len(), 1);
    assert_eq!(block.tokens[0].scope, "plain");
}

#[test]
fn language_aliases_resolve_to_a_grammar() {
    // The aliases spec 05 §4 names, plus the ones a model actually types. Most resolve
    // natively against two-face's set; this is what catches it if one stops.
    for alias in [
        "ts", "tsx", "js", "jsx", "sh", "zsh", "yml", "objc", "c++", "rust", "python", "python3",
        "rb", "golang", "bash", "console", "shell", "cpp", "csharp", "markdown", "jsonc", "docker",
        "mjs", "obj-c", "ksh",
    ] {
        let tokens = highlight::tokens(Some(alias), "x = 1;\nfoo(bar)\n");
        assert!(
            tokens.len() != 1 || tokens[0].scope != "plain",
            "`{alias}` fell through to plain"
        );
    }
}

/// The reason `two-face` is a dependency at all: syntect's default set has no Swift, and a
/// Swift coding-agent client rendering Swift as flat monospace is the regression that would
/// otherwise return silently. Same for the other grammars the default set is missing.
#[test]
fn the_languages_the_default_syntax_set_lacks_are_highlighted() {
    let cases = [
        (
            "swift",
            "struct Point: Sendable {\n    let x: Int\n    func moved(by d: Int) -> Point {\n        Point(x: x + d)\n    }\n}\n",
        ),
        (
            "typescript",
            "export const add = (a: number, b: number): number => a + b;\n",
        ),
        ("tsx", "const V = () => <div className=\"x\">{count}</div>;\n"),
        ("toml", "[package]\nname = \"form-core\"\nedition = \"2021\"\n"),
        ("kotlin", "fun main() { val x: Int = 1; println(x) }\n"),
        ("dockerfile", "FROM rust:1.85\nRUN cargo build --release\n"),
        ("nix", "{ pkgs ? import <nixpkgs> {} }: pkgs.mkShell { }\n"),
        ("zig", "const std = @import(\"std\");\npub fn main() void {}\n"),
    ];
    for (language, code) in cases {
        let doc = parse(&format!("```{language}\n{code}```\n"));
        let block = first_code_block(&doc);
        assert_eq!(block.language.as_deref(), Some(language));
        assert!(
            block.tokens.len() > 1,
            "`{language}` produced {} token(s): {:?}",
            block.tokens.len(),
            block.tokens
        );
        assert!(
            block.tokens.iter().all(|t| t.scope != "plain"),
            "`{language}` fell back to plain: {:?}",
            block.tokens
        );
        // The scopes must still be scopes, and still be applicable to the block's text.
        for token in &block.tokens {
            assert!(!utf16_slice(&block.code, token.start, token.len).is_empty());
        }
    }
}

#[test]
fn warming_loads_the_syntax_set_and_is_idempotent() {
    warm();
    let after = Instant::now();
    warm();
    assert!(
        after.elapsed() < std::time::Duration::from_millis(5),
        "a second warm must not reload the dump"
    );
    // And a code block still highlights normally afterwards.
    assert!(
        first_code_block(&parse("```swift\nlet x = 1\n```\n"))
            .tokens
            .len()
            > 1
    );
}

/// A fence that asks for no highlighting gets the same answer an unknown one does, rather
/// than the set's `Plain Text` grammar quietly emitting nothing at all.
#[test]
fn explicitly_plain_fences_get_one_plain_token() {
    for language in ["text", "plaintext", "txt", "none", "log", "output"] {
        let doc = parse(&format!("```{language}\nnot code, just output\n```\n"));
        let block = first_code_block(&doc);
        assert_eq!(block.tokens.len(), 1, "for {language}");
        assert_eq!(block.tokens[0].scope, "plain", "for {language}");
    }
}

#[test]
fn fence_info_string_takes_only_the_language() {
    let doc = parse("```rust,ignore no_run\nfn a() {}\n```\n");
    let block = first_code_block(&doc);
    assert_eq!(block.language.as_deref(), Some("rust,ignore"));
    assert_eq!(block.tokens.len(), 1, "an unknown compound token is plain");
}

#[test]
fn diff_fences_get_line_level_scopes() {
    let doc = parse("```diff\n-gone\n+added\n context\n```\n");
    let block = first_code_block(&doc);
    let scoped: Vec<(&str, String)> = block
        .tokens
        .iter()
        .map(|t| (t.scope.as_str(), utf16_slice(&block.code, t.start, t.len)))
        .collect();
    assert_eq!(
        scoped,
        vec![
            ("markup.deleted", "-gone".to_string()),
            ("markup.inserted", "+added".to_string()),
        ]
    );
}

#[test]
fn highlighting_is_capped_for_runaway_pastes() {
    let wide = format!("```rust\n{}\n```\n", "let x = 1; ".repeat(20_000));
    let tokens = first_code_block(&parse(&wide)).tokens;
    assert_eq!(tokens.len(), 1, "over 200 KB is one plain token");
    assert_eq!(tokens[0].scope, "plain");

    let tall = format!("```rust\n{}```\n", "let x = 1;\n".repeat(6_000));
    let tokens = first_code_block(&parse(&tall)).tokens;
    assert_eq!(tokens.len(), 1, "over 5000 lines is one plain token");
    assert_eq!(tokens[0].scope, "plain");
}

#[test]
fn tokens_carry_scopes_and_never_colors() {
    let doc = parse("```rust\nfn main() { let x: u8 = 1; }\n```\n");
    let tokens = first_code_block(&doc).tokens;
    assert!(tokens.iter().any(|t| t.scope.starts_with("keyword.")
        || t.scope.starts_with("storage.")
        || t.scope.starts_with("entity.")));
    assert!(
        tokens.iter().all(|t| !t.scope.contains('#')),
        "a colour got into the token stream"
    );
    // Tokens are ordered and never overlap, so Swift can apply them in one pass.
    for pair in tokens.windows(2) {
        assert!(pair[0].start + pair[0].len <= pair[1].start, "{pair:?}");
    }
}

// ---------------------------------------------------------------------------------------
// Performance
// ---------------------------------------------------------------------------------------

/// A document shaped like a long assistant answer: 120 top-level blocks over ~60 KB, so
/// roughly 500 bytes a block. `salt` makes every block's content unique, which is what
/// forces the highlight cache to miss.
fn generated_document(blocks: usize, salt: &str) -> String {
    const FILLER: &str = "Everything here is deliberately verbose: the point of the fixture is to reach the size and shape of a real long answer, not to read well.";
    let mut out = String::new();
    for i in 0..blocks {
        match i % 6 {
            0 => out.push_str(&format!(
                "## Section {i}: configuring the {salt} pipeline end to end, step by step, \
                 with the caveats called out where they matter\n\n"
            )),
            1 => out.push_str(&format!(
                "Paragraph {i} of {salt} with **bold**, *emphasis*, `inline_code({i})`, a \
                 [link](https://example.com/{salt}/{i}) and ~~a struck clause~~, followed by \
                 enough ordinary prose to reach a realistic length: the sentence keeps going \
                 the way an explanation keeps going, naming things, qualifying them, and \
                 arriving somewhere useful. A second sentence covers the edge case that the \
                 first one glossed over, because a real answer almost always has one, and a \
                 third closes the thought so the paragraph does not trail off mid-idea. {FILLER}\n\n"
            )),
            2 => out.push_str(&format!(
                "```rust\n\
                 /// Generated helper {i} for {salt}.\n\
                 pub fn generated_{i}(input: &str, limit: usize) -> Result<Vec<String>, Error> {{\n\
                 \x20   let parsed: Vec<&str> = input.split(',').map(str::trim).collect();\n\
                 \x20   if parsed.len() > limit {{\n\
                 \x20       return Err(Error::TooMany {{ found: parsed.len(), limit }});\n\
                 \x20   }}\n\
                 \x20   let mut owned: Vec<String> = Vec::with_capacity(parsed.len());\n\
                 \x20   for (index, field) in parsed.into_iter().enumerate() {{\n\
                 \x20       owned.push(format!(\"{{index}}:{{field}}\"));\n\
                 \x20   }}\n\
                 \x20   owned.sort_unstable();\n\
                 \x20   owned.dedup();\n\
                 \x20   debug_assert!(owned.len() <= limit, \"{{salt}} invariant {i}\");\n\
                 \x20   Ok(owned)\n\
                 }}\n```\n\n"
            )),
            3 => out.push_str(&format!(
                "- item {i} of {salt}, with a clause long enough to wrap in a real column\n\
                 - item {i} again, this time mentioning `a_symbol_{i}` and a \
                 [link](https://example.com/{i}) that points somewhere plausible\n\
                 \x20 - a nested note about {salt} that carries its own explanatory tail\n\
                 \x20 - a second nested note, because one is never enough in practice\n\
                 - [x] item {i} finished, checked off, and described in passing\n\
                 - [ ] item {i} still outstanding, with the reason spelled out\n\
                 - a closing item that restates the point: {FILLER}\n\
                 - and one more, mentioning `option_{i}` for good measure: {FILLER}\n\n"
            )),
            4 => out.push_str(&format!(
                "| Setting | Default | Notes for {salt} |\n|---|---:|:---|\n\
                 | `option_{i}` | {i} | what this one controls and when to change it |\n\
                 | `option_{i}_alt` | none | the alternative form, and why it exists |\n\
                 | `option_{i}_legacy` | off | kept for compatibility with older sessions |\n\
                 | `option_{i}_debug` | off | emits the intermediate plan to the log |\n\
                 | `option_{i}_note` | off | {FILLER} |\n\n"
            )),
            _ => out.push_str(&format!(
                "> Note {i} on {salt}: *emphasised* guidance about the step above, spelled out \
                 at the length a reader actually needs rather than the length that fits in a \
                 tooltip.\n>\n\
                 > It runs to a second paragraph, because real callouts usually do, and the \
                 second one is where the caveat lives. {FILLER}\n\n"
            )),
        }
    }
    out
}

#[test]
fn parses_a_120_block_60kb_document_within_budget() {
    // Warm the syntax set: deserializing syntect's syntax dump costs tens of milliseconds
    // once per process, which the app pays before the first token arrives rather than on
    // any one parse. The warm-up must include a code block, or the load lands inside the
    // measurement instead.
    let load = Instant::now();
    let _ = parse(&generated_document(6, "warmup"));
    let load = load.elapsed();

    let source = generated_document(120, "budget");
    assert!(
        source.len() >= 60 * 1024,
        "fixture is only {} bytes",
        source.len()
    );

    // Cold: this content has never been highlighted, so every code block misses the LRU.
    let cold_start = Instant::now();
    let doc = parse(&source);
    let cold = cold_start.elapsed();
    assert_eq!(doc.blocks.len(), 120);

    // Warm: the streaming steady state, where only the tail has changed.
    let warm_start = Instant::now();
    let again = parse(&source);
    let warm = warm_start.elapsed();
    assert_eq!(doc, again);

    println!(
        "{} bytes, {} blocks — syntax set load {:?}, cold {:?}, warm {:?}",
        source.len(),
        doc.blocks.len(),
        load,
        cold,
        warm
    );

    // Spec 05 §5's 16 ms budget is a per-tick budget on the streaming path, where all but
    // the tail is already highlighted — that is the `warm` figure, and it is the one the
    // chat view lives or dies by. A first-ever parse additionally highlights every code
    // block in the document (~9 KB of Rust here), which syntect's pure-Rust regex engine
    // costs about 20 ms for; that happens once, when an old session is reopened, so it
    // gets a looser ceiling that still catches a real regression.
    //
    // `cargo test` is unoptimised, where syntect runs an order of magnitude slower, so the
    // numbers that matter are asserted per profile rather than skipped.
    // The unoptimised ceilings are loose on purpose: they are regression tripwires, not
    // budgets, and a loaded machine must not turn them into flakes.
    let (warm_budget, cold_budget) = if cfg!(debug_assertions) {
        (150, 800)
    } else {
        (16, 40)
    };
    assert!(
        warm.as_millis() <= warm_budget,
        "steady-state parse took {warm:?}, budget {warm_budget} ms"
    );
    assert!(
        cold.as_millis() <= cold_budget,
        "first parse took {cold:?}, ceiling {cold_budget} ms"
    );
}

/// The measurement that actually mirrors F7.3: the same document delivered the way a run
/// delivers it, re-parsed on every debounce tick.
#[test]
fn every_streaming_tick_stays_within_budget() {
    let _ = parse(&generated_document(6, "warmup"));
    let source = generated_document(120, "ticks");

    let mut worst = std::time::Duration::ZERO;
    let mut worst_at = 0usize;
    let mut end = 0usize;
    while end < source.len() {
        end = (end + 2048).min(source.len());
        while !source.is_char_boundary(end) {
            end += 1;
        }
        let start = Instant::now();
        let doc = parse_streaming(&source[..end], end == source.len());
        let elapsed = start.elapsed();
        assert!(!doc.blocks.is_empty());
        if elapsed > worst {
            worst = elapsed;
            worst_at = end;
        }
    }
    println!(
        "worst tick {worst:?} at {worst_at} bytes of {}",
        source.len()
    );

    let budget = if cfg!(debug_assertions) { 150 } else { 16 };
    assert!(
        worst.as_millis() <= budget,
        "worst tick took {worst:?} at {worst_at} bytes, budget {budget} ms"
    );
}

#[test]
fn unchanged_code_blocks_are_not_rehighlighted() {
    let source = "```rust\nfn cached(x: u32) -> u32 { x + 1 }\n```\n";
    let first = parse(source);
    let second = parse(&format!("{source}\ntrailing text\n"));
    let a = first_code_block(&first).tokens;
    let b = first_code_block(&second).tokens;
    assert_eq!(a, b, "the memoized tokens are byte-identical");
}
