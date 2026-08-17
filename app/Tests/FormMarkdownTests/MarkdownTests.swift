import AppKit
import FormCore
import FormDesign
import Foundation
import Testing

@testable import FormMarkdown

/// Spec 11 §5. The fixture (`MarkdownFixture.everything`) is the document under test almost
/// everywhere: it carries every block and every span the wire can produce, so a test that
/// walks it is a test that nothing was quietly dropped.
@MainActor
struct MarkdownRenderingTests {
    let metrics = MarkdownMetrics(theme: .light, style: .default)

    // MARK: Coverage

    @Test("the fixture exercises every block and span kind the wire can carry")
    func fixtureIsComplete() {
        var blockKinds: Set<String> = []
        var spanKinds: Set<String> = []

        func walk(_ blocks: [MarkdownBlock]) {
            for block in blocks {
                blockKinds.insert(block.kind.type)
                switch block.kind {
                case let .paragraph(spans), let .heading(_, spans, _):
                    walk(spans)
                case let .quote(blocks), let .footnoteDef(_, blocks):
                    walk(blocks)
                case let .list(_, _, _, items):
                    items.forEach { walk($0.blocks) }
                case let .table(_, header, rows):
                    header.forEach(walk)
                    rows.forEach { $0.forEach(walk) }
                case .codeBlock, .rule, .image, .html, .unknown:
                    break
                }
            }
        }
        func walk(_ spans: [Span]) {
            for span in spans {
                spanKinds.insert(span.type)
                switch span {
                case let .emphasis(inner), let .strong(inner), let .strike(inner),
                    let .link(_, _, inner):
                    walk(inner)
                case .text, .code, .footnoteRef, .break, .unknown:
                    break
                }
            }
        }
        walk(MarkdownFixture.everything.blocks)

        let expectedBlocks: Set<String> = [
            "paragraph", "heading", "codeBlock", "list", "quote", "table", "rule", "image",
            "html", "footnoteDef",
        ]
        let expectedSpans: Set<String> = [
            "text", "emphasis", "strong", "strike", "code", "link", "footnoteRef", "break",
        ]
        #expect(expectedBlocks.subtracting(blockKinds).isEmpty)
        #expect(expectedSpans.subtracting(spanKinds).isEmpty)
    }

    @Test("every block reaches either a text run or a native view")
    func nothingIsDropped() {
        for run in MarkdownRun.segment(MarkdownFixture.everything.blocks) {
            switch run {
            case let .text(blocks):
                #expect(blocks.allSatisfy { $0.isTextual })
                let rendered = MarkdownAttributedBuilder.render(blocks, metrics: metrics)
                #expect(!rendered.isEmpty, "a text run rendered nothing")
            case let .native(block):
                #expect(!block.isTextual)
            }
        }
    }

    @Test("a list carrying a code block becomes a native list, not a silent drop")
    func listWithCodeIsNative() {
        let list = MarkdownFixture.block(
            0,
            .list(
                ordered: true, start: 1, tight: false,
                items: [
                    MarkdownFixture.item([
                        .paragraph(spans: [.text(text: "step")]),
                        .codeBlock(language: "sh", code: "ls\n", tokens: [], partial: false),
                    ])
                ]))
        #expect(!list.isTextual)
        #expect(MarkdownRun.segment([list]).count == 1)
    }

    // MARK: Selection and copy (F7.4)

    @Test("copying a selection returns markdown source, not rendered text")
    func copyYieldsMarkdown() {
        let doc = MarkdownFixture.doc([
            .paragraph(spans: [
                .text(text: "a "),
                .strong(spans: [.text(text: "bold")]),
                .text(text: " and "),
                .code(text: "code()"),
                .text(text: " end"),
            ])
        ])
        let rendered = MarkdownAttributedBuilder.render(doc.blocks, metrics: metrics)
        let text = rendered.attributed.string

        #expect(text == "a bold and code() end")

        // Whole paragraph.
        let all = NSRange(location: 0, length: (text as NSString).length)
        #expect(rendered.markdown(for: all) == "a **bold** and `code()` end")

        // A construct fully inside the selection keeps its delimiters.
        let bold = (text as NSString).range(of: "bold")
        #expect(rendered.markdown(for: bold) == "**bold**")

        let code = (text as NSString).range(of: "code()")
        #expect(rendered.markdown(for: code) == "`code()`")

        // A construct the selection only straddles contributes its text, never half a
        // delimiter.
        let partial = NSRange(location: bold.location, length: 2)
        #expect(rendered.markdown(for: partial) == "bo")
    }

    @Test("copy reproduces block structure — headings, lists and task boxes")
    func copyReproducesBlocks() {
        let doc = MarkdownFixture.doc([
            .heading(level: 2, spans: [.text(text: "Plan")], anchor: "plan"),
            .list(
                ordered: false, start: 1, tight: true,
                items: [
                    MarkdownFixture.item(
                        [.paragraph(spans: [.text(text: "done")])], checked: true),
                    MarkdownFixture.item(
                        [.paragraph(spans: [.text(text: "todo")])], checked: false),
                ]),
        ])
        let rendered = MarkdownAttributedBuilder.render(doc.blocks, metrics: metrics)
        let all = NSRange(location: 0, length: rendered.attributed.length)
        #expect(
            rendered.markdown(for: all) == """
                ## Plan

                - [x] done
                - [ ] todo
                """)
    }

    @Test("a quote's own run copies back as a quote")
    func quotePrefixSurvivesCopy() {
        let inner = [MarkdownFixture.block(0, .paragraph(spans: [.text(text: "quoted")]))]
        let rendered = MarkdownAttributedBuilder.render(
            inner, metrics: metrics.quoted(), sourcePrefix: "> ")
        let all = NSRange(location: 0, length: rendered.attributed.length)
        #expect(rendered.markdown(for: all) == "> quoted")
    }

    @Test("an empty selection copies nothing")
    func emptySelection() {
        let rendered = MarkdownAttributedBuilder.render(
            MarkdownFixture.everything.blocks.filter(\.isTextual), metrics: metrics)
        #expect(rendered.markdown(for: NSRange(location: 3, length: 0)).isEmpty)
    }

    // MARK: Links (F7.5)

    @Test("javascript: arrives as plain text and is never clickable")
    func javascriptIsNotALink() {
        // The core downgrades it (spec 05 §3); this asserts the renderer adds nothing back.
        let rendered = MarkdownAttributedBuilder.render(
            MarkdownFixture.everything.blocks.filter(\.isTextual), metrics: metrics)
        var links: [URL] = []
        rendered.attributed.enumerateAttribute(
            .link, in: NSRange(location: 0, length: rendered.attributed.length)
        ) { value, _, _ in
            if let url = value as? URL { links.append(url) }
        }
        #expect(rendered.attributed.string.contains("javascript:alert(1)"))
        #expect(links.allSatisfy { $0.scheme != "javascript" })
        #expect(!links.isEmpty, "the fixture should still carry real links")
    }

    @Test("only http, https, mailto and file become links")
    func linkSchemes() {
        #expect(MarkdownLink.url(from: "https://example.com") != nil)
        #expect(MarkdownLink.url(from: "http://example.com") != nil)
        #expect(MarkdownLink.url(from: "mailto:a@example.com") != nil)
        #expect(MarkdownLink.url(from: "file:///tmp/x.rs") != nil)
        #expect(MarkdownLink.url(from: "javascript:alert(1)") == nil)
        #expect(MarkdownLink.url(from: "data:text/html;base64,AAAA") == nil)
        #expect(MarkdownLink.url(from: "example.com") == nil)
    }

    @Test("a file link shows a path, a web link shows the URL")
    func linkTooltips() throws {
        let file = try #require(MarkdownLink.url(from: "file:///tmp/router.rs"))
        #expect(MarkdownLink.display(file) == "/tmp/router.rs")
        let web = try #require(MarkdownLink.url(from: "https://example.com/a"))
        #expect(MarkdownLink.display(web) == "https://example.com/a")
    }

    // MARK: Code blocks (F7.2)

    @Test("scope names become theme colors, never colors from the core")
    func syntaxScopesResolve() {
        let code = "let x = 1 // note\n"
        let tokens = [
            CodeToken(start: 0, len: 3, scope: "keyword.other.rust"),
            CodeToken(start: 8, len: 1, scope: "constant.numeric.rust"),
            CodeToken(start: 10, len: 7, scope: "comment.line.double-slash.rust"),
        ]
        for theme in [Theme.light, Theme.dark] {
            let metrics = MarkdownMetrics(theme: theme, style: .default)
            let attributed = CodeHighlighting.attributed(
                code: code, tokens: tokens, metrics: metrics)
            let colors = attributed.runs.map(\.foregroundColor)
            #expect(colors.contains(theme.syntax.keyword.color))
            #expect(colors.contains(theme.syntax.number.color))
            #expect(colors.contains(theme.syntax.comment.color))
            #expect(String(attributed.characters) == code)
        }
    }

    @Test("UTF-16 token ranges land correctly across emoji and CJK")
    func utf16Ranges() {
        // "🚀" is two UTF-16 units, "日本語" is three. The core measures in UTF-16 precisely
        // so this arithmetic is the renderer's whole job (spec 05 §4).
        let code = "🚀 let 日本語 = \"ok\"\n"
        let ns = code as NSString
        let keyword = ns.range(of: "let")
        let string = ns.range(of: "\"ok\"")
        let attributed = CodeHighlighting.attributed(
            code: code,
            tokens: [
                CodeToken(start: keyword.location, len: keyword.length, scope: "keyword.other"),
                CodeToken(
                    start: string.location, len: string.length, scope: "string.quoted.double"),
            ],
            metrics: metrics)

        #expect(String(attributed.characters) == code)
        let keywordRun = attributed.runs.first { $0.foregroundColor == metrics.theme.syntax.keyword.color }
        #expect(keywordRun.map { String(attributed[$0.range].characters) } == "let")
        let stringRun = attributed.runs.first { $0.foregroundColor == metrics.theme.syntax.string.color }
        #expect(stringRun.map { String(attributed[$0.range].characters) } == "\"ok\"")
    }

    @Test("a token range the core could not have meant is ignored, not fatal")
    func outOfBoundsTokens() {
        let code = "ab\n"
        let attributed = CodeHighlighting.attributed(
            code: code,
            tokens: [
                CodeToken(start: 99, len: 5, scope: "keyword"),
                CodeToken(start: 0, len: 0, scope: "keyword"),
            ],
            metrics: metrics)
        #expect(String(attributed.characters) == code)
    }

    @Test("line numbers count lines, not newlines")
    func lineCounting() {
        #expect(CodeHighlighting.lineCount(of: "") == 1)
        #expect(CodeHighlighting.lineCount(of: "a") == 1)
        #expect(CodeHighlighting.lineCount(of: "a\n") == 1)
        #expect(CodeHighlighting.lineCount(of: "a\nb") == 2)
        #expect(CodeHighlighting.lineCount(of: "a\nb\n") == 2)
    }

    @Test("a partial code block suppresses its copy button")
    func partialSuppressesChrome() throws {
        let last = try #require(MarkdownFixture.streamingTail.blocks.last)
        guard case let .codeBlock(_, _, _, partial) = last.kind else {
            Issue.record("the streaming fixture should end in a code block")
            return
        }
        #expect(partial)

        let view = CodeBlockView(
            language: "rust", code: "x", tokens: [], partial: true, metrics: metrics)
        #expect(!view.showsCopyButton)
        let complete = CodeBlockView(
            language: "rust", code: "x", tokens: [], partial: false, metrics: metrics)
        #expect(complete.showsCopyButton)
    }

    // MARK: Layout invariants

    @Test("a long unbreakable token wraps — the column never scrolls horizontally")
    func textRunNeverExceedsItsWidth() {
        let doc = MarkdownFixture.doc([
            .paragraph(spans: [
                .text(text: String(repeating: "A", count: 400)),
                .text(text: " "),
                .code(text: String(repeating: "b", count: 400)),
            ])
        ])
        let rendered = MarkdownAttributedBuilder.render(doc.blocks, metrics: metrics)
        let (storage, layout, container) = MarkdownTextRun.textKitStack()
        storage.setAttributedString(rendered.attributed)
        let width: CGFloat = 320
        let size = MarkdownTextRun.measure(layout: layout, container: container, width: width)
        #expect(size.width == width)
        #expect(layout.usedRect(for: container).width <= width + 1)
        #expect(size.height > 0)
    }

    @Test("list depth drives the marker and the indent")
    func listDepth() {
        #expect(metrics.bullet(depth: 0) == "•")
        #expect(metrics.bullet(depth: 1) == "◦")
        #expect(metrics.bullet(depth: 2) == "▪")
        #expect(metrics.bullet(depth: 3) == "•")
        #expect(MarkdownMetrics.checkbox(true) == "☑")
        #expect(MarkdownMetrics.checkbox(false) == "☐")

        let doc = MarkdownFixture.doc([
            .list(
                ordered: false, start: 1, tight: true,
                items: [
                    MarkdownFixture.item([
                        .paragraph(spans: [.text(text: "outer")]),
                        .list(
                            ordered: false, start: 1, tight: true,
                            items: [
                                MarkdownFixture.item([.paragraph(spans: [.text(text: "inner")])])
                            ]),
                    ])
                ])
        ])
        let rendered = MarkdownAttributedBuilder.render(doc.blocks, metrics: metrics)
        #expect(rendered.attributed.string.contains("• outer"))
        #expect(rendered.attributed.string.contains("◦ inner"))

        var indents: Set<CGFloat> = []
        rendered.attributed.enumerateAttribute(
            .paragraphStyle, in: NSRange(location: 0, length: rendered.attributed.length)
        ) { value, _, _ in
            if let style = value as? NSParagraphStyle { indents.insert(style.firstLineHeadIndent) }
        }
        #expect(indents.contains(0))
        #expect(indents.contains(metrics.listIndent))
    }

    @Test("an ordered list respects start")
    func orderedStart() {
        let doc = MarkdownFixture.doc([
            .list(
                ordered: true, start: 3, tight: true,
                items: [
                    MarkdownFixture.item([.paragraph(spans: [.text(text: "three")])]),
                    MarkdownFixture.item([.paragraph(spans: [.text(text: "four")])]),
                ])
        ])
        let rendered = MarkdownAttributedBuilder.render(doc.blocks, metrics: metrics)
        #expect(rendered.attributed.string.contains("3. three"))
        #expect(rendered.attributed.string.contains("4. four"))
    }

    @Test("raw html is shown as escaped monospace text, never interpreted")
    func htmlIsInert() {
        let raw = "<script>alert(1)</script>"
        let doc = MarkdownFixture.doc([.html(raw: raw)])
        let rendered = MarkdownAttributedBuilder.render(doc.blocks, metrics: metrics)
        #expect(rendered.attributed.string.trimmingCharacters(in: .newlines) == raw)
        let font = rendered.attributed.attribute(.font, at: 0, effectiveRange: nil) as? NSFont
        #expect(font?.fontDescriptor.symbolicTraits.contains(.monoSpace) == true)
    }

    // MARK: Streaming identity (F7.3)

    @Test("unchanged blocks hit the render cache; a changed tail misses it")
    func cacheTracksBlockIds() {
        let cache = MarkdownRenderCache()
        let stable = MarkdownFixture.block(0, .paragraph(spans: [.text(text: "stable")]))

        func render(_ tail: String) {
            let tailBlock = MarkdownFixture.block(1, .paragraph(spans: [.text(text: tail)]))
            for run in MarkdownRun.segment([stable, tailBlock]) {
                _ = renderedText(
                    run.blocks, metrics: metrics, sourcePrefix: "", depth: 0, cache: cache)
            }
        }

        render("one")
        let afterFirst = cache.count
        render("one two")
        // The stable run is untouched, so only the tail's key is new.
        #expect(cache.count == afterFirst + 1)

        // And a re-render with no change at all adds nothing.
        render("one two")
        #expect(cache.count == afterFirst + 1)
    }

    @Test("the cache evicts by least-recently-used, so a streaming tail cannot flush it")
    func cacheEviction() {
        let cache = MarkdownRenderCache(capacity: 4)
        let hot = MarkdownFixture.block(0, .paragraph(spans: [.text(text: "hot")]))
        for tick in 0 ..< 50 {
            _ = renderedText([hot], metrics: metrics, sourcePrefix: "", depth: 0, cache: cache)
            let tail = MarkdownFixture.block(1, .paragraph(spans: [.text(text: "tick \(tick)")]))
            _ = renderedText([tail], metrics: metrics, sourcePrefix: "", depth: 0, cache: cache)
        }
        #expect(cache.count <= 4)

        // The hot run survived: rendering it again is a hit, so nothing is added.
        let before = cache.count
        _ = renderedText([hot], metrics: metrics, sourcePrefix: "", depth: 0, cache: cache)
        #expect(cache.count == before)
    }

    @Test("run identity is the first block's id, so a growing run is updated not rebuilt")
    func runIdentityIsStable() {
        let first = MarkdownFixture.block(0, .paragraph(spans: [.text(text: "intro")]))
        let short = MarkdownRun.segment([first])
        let grown = MarkdownRun.segment([
            first, MarkdownFixture.block(1, .paragraph(spans: [.text(text: "more")])),
        ])
        #expect(short.first?.id == grown.first?.id)
        #expect(short.first?.contentKey != grown.first?.contentKey)
    }
}
