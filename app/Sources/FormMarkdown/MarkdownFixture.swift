import FormCore
import Foundation

/// A hand-built block tree exercising every block and span the wire can carry.
///
/// It is built the way the core builds one — including ids of the form `b{index}-{hash}`,
/// which change exactly when the block changes — so previews, snapshot tests and the
/// streaming budget all run with **no Rust build and no parser** (spec 15 §3: every view
/// gets a preview that works without one).
public enum MarkdownFixture {
    /// Every block kind and every span kind, in one document.
    public static let everything = doc([
        .heading(level: 1, spans: [.text(text: "Markdown fixture")], anchor: "markdown-fixture"),

        .paragraph(spans: [
            .text(text: "Body text with "),
            .strong(spans: [.text(text: "strong")]),
            .text(text: ", "),
            .emphasis(spans: [.text(text: "emphasis")]),
            .text(text: ", "),
            .strike(spans: [.text(text: "struck")]),
            .text(text: ", "),
            .code(text: "inline_code()"),
            .text(text: ", a "),
            .link(
                url: "https://example.com/docs", title: "The docs",
                spans: [.text(text: "web link")]),
            .text(text: ", "),
            .link(url: "mailto:someone@example.com", title: nil, spans: [.text(text: "mail")]),
            .text(text: ", "),
            .link(url: "file:///tmp/router.rs", title: nil, spans: [.text(text: "a file")]),
            .text(text: ", and a footnote"),
            .footnoteRef(label: "note"),
            .text(text: "."),
            .break(hard: true),
            .text(text: "A hard break precedes this line;"),
            .break(hard: false),
            .text(text: "a soft one precedes this one."),
        ]),

        // The core downgrades a `javascript:` URL to plain text (spec 05 §3), so what the
        // renderer must do is nothing at all — this block asserts that by example.
        .paragraph(spans: [
            .text(text: "javascript:alert(1) arrives as text, never as a link."),
        ]),

        .heading(level: 2, spans: [.text(text: "Lists")], anchor: "lists"),

        .list(
            ordered: false, start: 1, tight: true,
            items: [
                item([.paragraph(spans: [.text(text: "First bullet")])]),
                item([
                    .paragraph(spans: [.text(text: "Second, with a nested list")]),
                    .list(
                        ordered: false, start: 1, tight: true,
                        items: [
                            item([.paragraph(spans: [.text(text: "Depth one")])]),
                            item([
                                .paragraph(spans: [.text(text: "Depth two")]),
                                .list(
                                    ordered: false, start: 1, tight: true,
                                    items: [
                                        item([.paragraph(spans: [.text(text: "Depth three")])])
                                    ]),
                            ]),
                        ]),
                ]),
                item([.paragraph(spans: [.text(text: "Done")])], checked: true),
                item([.paragraph(spans: [.text(text: "Not done")])], checked: false),
            ]),

        .list(
            ordered: true, start: 3, tight: false,
            items: [
                item([.paragraph(spans: [.text(text: "Numbering starts at three")])]),
                item([
                    .paragraph(spans: [.text(text: "An item carrying a code block")]),
                    rustBlock,
                ]),
            ]),

        .heading(level: 3, spans: [.text(text: "Quotes")], anchor: "quotes"),

        .quote(blocks: [
            block(0, .paragraph(spans: [.text(text: "A quote renders secondary, with a rule.")])),
            block(1, .quote(blocks: [
                block(0, .paragraph(spans: [.text(text: "And quotes nest.")]))
            ])),
        ]),

        .heading(level: 4, spans: [.text(text: "Code")], anchor: "code"),
        rustBlock,
        .codeBlock(language: nil, code: "no language, no tokens\n", tokens: [], partial: false),

        .heading(level: 5, spans: [.text(text: "Tables")], anchor: "tables"),

        .table(
            align: [.left, .center, .right],
            header: [
                [.text(text: "Language")], [.text(text: "Status")], [.text(text: "Tokens")],
            ],
            rows: [
                [
                    [.code(text: "rust")], [.text(text: "shipped")], [.text(text: "1,204")],
                ],
                [
                    [.code(text: "swift")],
                    [.emphasis(spans: [.text(text: "in progress")])],
                    [.text(text: "986")],
                ],
                [
                    [.code(text: "typescript")],
                    [.link(url: "https://example.com", title: nil, spans: [.text(text: "queued")])],
                    [.text(text: "12")],
                ],
            ]),

        .heading(level: 6, spans: [.text(text: "Everything else")], anchor: "everything-else"),
        .rule,
        .image(
            url: "https://example.com/diagram.png", alt: "An architecture diagram",
            title: "Diagram"),
        .image(url: "file:///tmp/screenshot.png", alt: "A local screenshot", title: nil),
        .html(raw: "<div class=\"callout\">raw html is shown, never interpreted</div>"),

        // UTF-16 ranges have to survive astral-plane and CJK content (spec 05 §4).
        .paragraph(spans: [
            .text(text: "Unicode: 🚀 emoji, 日本語 text, and "),
            .code(text: "let 変数 = \"🎯\""),
            .text(text: " inline."),
        ]),

        .footnoteDef(
            label: "note",
            blocks: [
                block(0, .paragraph(spans: [.text(text: "The footnote body.")]))
            ]),
    ])

    /// A response caught mid-stream: the trailing code block is still being written, so its
    /// copy button and trailing chrome are suppressed (spec 11 §4).
    public static let streamingTail = doc([
        .paragraph(spans: [.text(text: "I'll add the endpoint and wire it into the router.")]),
        .codeBlock(
            language: "rust",
            code: "async fn healthz() -> impl IntoRespo",
            tokens: [
                CodeToken(start: 0, len: 5, scope: "keyword.control.rust"),
                CodeToken(start: 6, len: 2, scope: "storage.type.function.rust"),
                CodeToken(start: 9, len: 7, scope: "entity.name.function.rust"),
            ],
            partial: true),
    ])

    /// Just the code blocks, for the line-numbers and soft-wrap previews.
    public static let codeOnly = doc([rustBlock, longLineBlock])

    /// A table with all three column alignments and zebra rows.
    public static let tableOnly = doc(
        everything.blocks.compactMap { block in
            if case .table = block.kind { return block.kind }
            return nil
        })

    /// Quotes, a rule, and lists nested three deep — including one whose item carries a code
    /// block, which is the case that renders natively instead of inside a text run.
    public static let quotesAndLists = doc(
        everything.blocks.compactMap { block in
            switch block.kind {
            case .quote, .rule, .list: return block.kind
            default: return nil
            }
        })

    /// Remote and local images, to check the reserved placeholder space.
    public static let imagesOnly = doc(
        everything.blocks.compactMap { block in
            if case .image = block.kind { return block.kind }
            return nil
        })

    // MARK: Pieces

    public static let rustCode = """
        async fn healthz() -> impl IntoResponse {
            Json(json!({ "status": "ok" }))
        }

        """

    public static let rustTokens = tokens(
        in: rustCode,
        [
            ("async", "keyword.control.rust"),
            ("fn", "storage.type.function.rust"),
            ("healthz", "entity.name.function.rust"),
            ("impl", "storage.modifier.rust"),
            ("IntoResponse", "entity.name.type.rust"),
            ("Json", "support.class.rust"),
            ("json!", "support.function.rust"),
            ("\"status\"", "string.quoted.double.rust"),
            ("\"ok\"", "string.quoted.double.rust"),
        ])

    static let rustBlock: BlockKind = .codeBlock(
        language: "rust", code: rustCode, tokens: rustTokens, partial: false)

    static let longLineBlock: BlockKind = .codeBlock(
        language: "sh",
        code: "curl -sS https://example.com/a/very/long/path/that/keeps/going/and/going"
            + "/and/going/and/going?query=1&and=2&more=3 | jq '.data[] | {id, name}'\n",
        tokens: [],
        partial: false)

    // MARK: Construction

    public static func doc(_ kinds: [BlockKind]) -> MarkdownDoc {
        MarkdownDoc(blocks: kinds.enumerated().map { block($0.offset, $0.element) })
    }

    /// Mirrors the core's id scheme (spec 05 §2): index plus a hash of the block, so the id
    /// changes exactly when the rendering would.
    public static func block(_ index: Int, _ kind: BlockKind) -> MarkdownBlock {
        MarkdownBlock(id: "b\(index)-\(fingerprint(kind))", kind: kind)
    }

    static func item(_ blocks: [BlockKind], checked: Bool? = nil) -> ListItem {
        ListItem(
            checked: checked,
            blocks: blocks.enumerated().map { block($0.offset, $0.element) })
    }

    private static func fingerprint(_ kind: BlockKind) -> String {
        // `.sortedKeys` is load-bearing: without it the encoder's key order is not stable
        // between calls, and an id that changes on its own would defeat the very identity
        // guarantee this fixture exists to reproduce.
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        let data = (try? encoder.encode(kind)) ?? Data()
        var hash: UInt64 = 0xcbf2_9ce4_8422_2325
        for byte in data {
            hash ^= UInt64(byte)
            hash = hash &* 0x100_0000_01b3
        }
        return String(hash, radix: 16)
    }

    /// Locates the fixture's highlight ranges by search rather than by hand-counted offsets —
    /// the point of the fixture is the *ranges being right*, and hand-counting UTF-16 is how
    /// they end up wrong.
    static func tokens(in code: String, _ pairs: [(String, String)]) -> [CodeToken] {
        let text = code as NSString
        var found: [CodeToken] = []
        for (needle, scope) in pairs {
            var searchFrom = 0
            while searchFrom < text.length {
                let range = text.range(
                    of: needle,
                    range: NSRange(location: searchFrom, length: text.length - searchFrom))
                guard range.location != NSNotFound else { break }
                found.append(
                    CodeToken(start: range.location, len: range.length, scope: scope))
                searchFrom = NSMaxRange(range)
            }
        }
        // Sorted and de-overlapped, which is what the core guarantees.
        var result: [CodeToken] = []
        for token in found.sorted(by: { $0.start < $1.start })
        where result.last.map({ $0.start + $0.len <= token.start }) ?? true {
            result.append(token)
        }
        return result
    }
}
