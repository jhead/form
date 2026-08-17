import AppKit
import FormCore
import FormDesign
import Foundation

/// Turns a contiguous run of *textual* blocks into one `NSAttributedString` plus the source
/// map that takes a selection in it back to markdown (spec 11 §3).
///
/// Everything is built in a single pass so the rendered and source sides cannot drift: every
/// `append` writes to both, and records the correspondence.
struct MarkdownAttributedBuilder {
    private let metrics: MarkdownMetrics
    /// Prefixed to every source line. `"> "` when this run is the body of a blockquote, so
    /// copying out of a quote still yields a quote.
    private let sourcePrefix: String

    private let out = NSMutableAttributedString()
    private let src = NSMutableString()
    private var roots: [SourceNode] = []

    init(metrics: MarkdownMetrics, sourcePrefix: String = "") {
        self.metrics = metrics
        self.sourcePrefix = sourcePrefix
    }

    static func render(
        _ blocks: [MarkdownBlock], metrics: MarkdownMetrics, sourcePrefix: String = ""
    ) -> RenderedText {
        var builder = MarkdownAttributedBuilder(metrics: metrics, sourcePrefix: sourcePrefix)
        builder.blocks(blocks, depth: 0)
        return builder.finish()
    }

    private func finish() -> RenderedText {
        RenderedText(
            attributed: NSAttributedString(attributedString: out),
            map: MarkdownSourceMap(source: src as String, nodes: roots)
        )
    }

    // MARK: - Blocks

    private mutating func blocks(_ blocks: [MarkdownBlock], depth: Int) {
        for (index, block) in blocks.enumerated() {
            let node = self.block(block, depth: depth, isLast: index == blocks.count - 1)
            if let node { roots.append(node) }
        }
    }

    private mutating func block(
        _ block: MarkdownBlock, depth: Int, isLast: Bool
    ) -> SourceNode? {
        switch block.kind {
        case let .paragraph(spans):
            return wrapped(open: linePrefix(depth), close: "\n\n", trailingNewline: !isLast) {
                $0.paragraphStyled(depth: depth, spacing: isLast ? 0 : $0.metrics.blockSpacing) {
                    $0.inline(spans, attributes: .body($0.metrics))
                }
            }

        case let .heading(level, spans, _):
            let hashes = String(repeating: "#", count: max(1, min(6, level)))
            return wrapped(
                open: "\(linePrefix(depth))\(hashes) ", close: "\n\n", trailingNewline: !isLast
            ) {
                $0.paragraphStyled(
                    depth: depth,
                    spacing: isLast ? 0 : $0.metrics.blockSpacing,
                    spacingBefore: $0.metrics.headingLeading
                ) {
                    $0.inline(
                        spans,
                        attributes: InlineAttributes(
                            style: $0.metrics.heading(level: level),
                            color: $0.metrics.textColor))
                }
            }

        case let .list(ordered, start, tight, items):
            return list(
                ordered: ordered, start: start, tight: tight, items: items,
                depth: depth, isLast: isLast)

        case let .html(raw):
            // Escaped monospace text, never interpreted (spec 11 §2). The core already
            // captured it as text; all we do is refuse to give it any semantics.
            return wrapped(open: linePrefix(depth), close: "\n\n", trailingNewline: !isLast) {
                $0.paragraphStyled(depth: depth, spacing: isLast ? 0 : $0.metrics.blockSpacing) {
                    [
                        $0.literal(
                            raw,
                            attributes: InlineAttributes(
                                style: $0.metrics.code,
                                color: $0.metrics.theme.color.textSecondary))
                    ]
                }
            }

        case let .footnoteDef(label, blocks):
            return wrapped(
                open: "\(linePrefix(depth))[^\(label)]: ", close: "\n\n", trailingNewline: !isLast
            ) { builder in
                var children: [SourceNode] = []
                builder.paragraphStyled(
                    depth: depth, spacing: builder.metrics.listItemSpacing
                ) {
                    [
                        $0.literal(
                            "\(label). ",
                            attributes: InlineAttributes(
                                style: $0.metrics.theme.typography.caption,
                                color: $0.metrics.theme.color.textTertiary),
                            source: "")
                    ]
                }.forEach { children.append($0) }
                for (index, inner) in blocks.enumerated() {
                    if let node = builder.block(
                        inner, depth: depth + 1, isLast: index == blocks.count - 1)
                    {
                        children.append(node)
                    }
                }
                return children
            }

        case .codeBlock, .table, .image, .quote, .rule, .unknown:
            // Rendered as native SwiftUI views; they never reach a text run (`MarkdownRun`).
            return nil
        }
    }

    private mutating func list(
        ordered: Bool, start: Int64, tight: Bool, items: [ListItem], depth: Int, isLast: Bool
    ) -> SourceNode {
        let renderedStart = out.length
        let sourceOpen = NSRange(location: src.length, length: 0)
        var children: [SourceNode] = []

        for (index, item) in items.enumerated() {
            let number = start &+ Int64(index)
            let markerSource = ordered ? "\(number). " : "- "
            let task = item.checked.map { $0 ? "[x] " : "[ ] " } ?? ""
            let lastItem = index == items.count - 1

            let node = wrapped(
                open: "\(linePrefix(depth))\(markerSource)\(task)",
                close: "\n",
                trailingNewline: !(lastItem && isLast)
            ) { builder in
                var parts: [SourceNode] = []
                let marker = ordered ? "\(number)." : builder.metrics.bullet(depth: depth)
                let box = item.checked.map { "\(MarkdownMetrics.checkbox($0)) " } ?? ""

                // The marker is text, not a control: it has to select and copy with the item.
                parts += builder.paragraphStyled(
                    depth: depth,
                    spacing: lastItem && isLast
                        ? 0 : (tight ? builder.metrics.listItemSpacing : builder.metrics.blockSpacing)
                ) {
                    [
                        $0.literal(
                            "\(marker) \(box)",
                            attributes: InlineAttributes(
                                style: $0.metrics.body,
                                color: $0.metrics.theme.color.textSecondary),
                            source: "")
                    ]
                }

                // The first block of the item continues the marker's line; anything after it
                // is a nested block at the item's indent.
                for (blockIndex, inner) in item.blocks.enumerated() {
                    let innerLast = blockIndex == item.blocks.count - 1
                    if blockIndex == 0, case let .paragraph(spans) = inner.kind {
                        parts += builder.paragraphStyled(
                            depth: depth,
                            spacing: innerLast
                                ? 0
                                : (tight
                                    ? builder.metrics.listItemSpacing : builder.metrics.blockSpacing)
                        ) {
                            $0.inline(spans, attributes: .body($0.metrics))
                        }
                        if !innerLast { builder.newline() }
                    } else if let node = builder.block(
                        inner, depth: depth + 1, isLast: innerLast)
                    {
                        parts.append(node)
                    }
                }
                return parts
            }
            children.append(node)
        }

        return .wrapper(
            rendered: NSRange(location: renderedStart, length: out.length - renderedStart),
            open: sourceOpen,
            close: NSRange(location: src.length, length: 0),
            children: children
        )
    }

    // MARK: - Spans

    private mutating func inline(
        _ spans: [Span], attributes: InlineAttributes
    ) -> [SourceNode] {
        var nodes: [SourceNode] = []
        for span in spans {
            switch span {
            case let .text(text):
                nodes.append(literal(text, attributes: attributes))

            case let .emphasis(inner):
                var next = attributes
                next.italic = true
                nodes.append(delimited("*", "*") { $0.inline(inner, attributes: next) })

            case let .strong(inner):
                var next = attributes
                next.bold = true
                nodes.append(delimited("**", "**") { $0.inline(inner, attributes: next) })

            case let .strike(inner):
                var next = attributes
                next.strike = true
                nodes.append(delimited("~~", "~~") { $0.inline(inner, attributes: next) })

            case let .code(text):
                var next = attributes
                next.style = metrics.codeInline
                next.chip = true
                // A fence long enough to survive backticks inside the span.
                let fence = String(repeating: "`", count: longestBacktickRun(in: text) + 1)
                nodes.append(
                    literal(
                        text, attributes: next,
                        source: "\(fence)\(text)\(fence)"))

            case let .link(url, title, inner):
                var next = attributes
                next.color = metrics.theme.color.accent
                next.link = MarkdownLink.url(from: url)
                let close = title.map { "](\(url) \"\($0)\")" } ?? "](\(url))"
                nodes.append(delimited("[", close) { $0.inline(inner, attributes: next) })

            case let .footnoteRef(label):
                var next = attributes
                next.style = metrics.theme.typography.micro
                next.color = metrics.theme.color.accent
                next.superscript = true
                nodes.append(literal(label, attributes: next, source: "[^\(label)]"))

            case let .break(hard):
                // A soft break is a space — the layout decides where lines end. A hard break
                // is the one the author asked for.
                nodes.append(
                    literal(hard ? "\n" : " ", attributes: attributes, source: hard ? "  \n" : "\n"))

            case .unknown:
                // A block/span type from a newer core. Dropping it is better than rendering
                // a placeholder into someone's transcript.
                continue
            }
        }
        return nodes
    }

    // MARK: - Primitives

    /// Appends `text` to the rendered string and `source` (defaulting to `text`) to the
    /// source, and returns the correspondence.
    private mutating func literal(
        _ text: String, attributes: InlineAttributes, source: String? = nil
    ) -> SourceNode {
        let rendered = NSRange(location: out.length, length: (text as NSString).length)
        out.append(NSAttributedString(string: text, attributes: attributes.resolved(metrics)))
        let sourceText = source ?? text
        let sourceRange = NSRange(location: src.length, length: (sourceText as NSString).length)
        src.append(sourceText)
        return .leaf(rendered: rendered, source: sourceRange, literal: source == nil)
    }

    /// A construct whose source has delimiters around its children.
    private mutating func delimited(
        _ open: String, _ close: String, _ body: (inout Self) -> [SourceNode]
    ) -> SourceNode {
        let renderedStart = out.length
        let openRange = NSRange(location: src.length, length: (open as NSString).length)
        src.append(open)
        let children = body(&self)
        let closeRange = NSRange(location: src.length, length: (close as NSString).length)
        src.append(close)
        return .wrapper(
            rendered: NSRange(location: renderedStart, length: out.length - renderedStart),
            open: openRange, close: closeRange, children: children)
    }

    /// A block: delimiters plus the rendered newline that separates it from the next one.
    private mutating func wrapped(
        open: String, close: String, trailingNewline: Bool, _ body: (inout Self) -> [SourceNode]
    ) -> SourceNode {
        let renderedStart = out.length
        let openRange = NSRange(location: src.length, length: (open as NSString).length)
        src.append(open)
        let children = body(&self)
        if trailingNewline { newline() }
        let closeRange = NSRange(location: src.length, length: (close as NSString).length)
        src.append(close)
        return .wrapper(
            rendered: NSRange(location: renderedStart, length: out.length - renderedStart),
            open: openRange, close: closeRange, children: children)
    }

    private mutating func newline() {
        out.append(NSAttributedString(string: "\n"))
    }

    /// Runs `body` and applies one paragraph style to everything it appended. Indents are
    /// the list nesting; `spacing` is the gap to whatever comes next.
    @discardableResult
    private mutating func paragraphStyled(
        depth: Int,
        spacing: CGFloat,
        spacingBefore: CGFloat = 0,
        _ body: (inout Self) -> [SourceNode]
    ) -> [SourceNode] {
        let start = out.length
        let nodes = body(&self)
        guard out.length > start else { return nodes }
        let style = NSMutableParagraphStyle()
        style.lineSpacing = metrics.body.lineSpacing
        style.paragraphSpacing = spacing
        style.paragraphSpacingBefore = spacingBefore
        style.firstLineHeadIndent = CGFloat(depth) * metrics.listIndent
        style.headIndent = CGFloat(depth + 1) * metrics.listIndent
        if depth == 0 { style.headIndent = 0 }
        out.addAttribute(
            .paragraphStyle, value: style,
            range: NSRange(location: start, length: out.length - start))
        return nodes
    }

    private func linePrefix(_ depth: Int) -> String {
        sourcePrefix + String(repeating: "  ", count: max(0, depth))
    }

    private func longestBacktickRun(in text: String) -> Int {
        var longest = 0
        var current = 0
        for character in text {
            if character == "`" {
                current += 1
                longest = max(longest, current)
            } else {
                current = 0
            }
        }
        return longest
    }
}

// MARK: - Inline attributes

/// The inline state a span inherits from its parents. Kept as data rather than as a stack of
/// `NSAttributedString` attributes so nesting (`**bold *and italic***`) composes.
struct InlineAttributes {
    var style: TypeStyle
    var color: ThemeColor
    var bold = false
    var italic = false
    var strike = false
    var superscript = false
    /// Inline code: draws the background chip (`MarkdownLayoutManager` rounds and insets it).
    var chip = false
    var link: URL?

    static func body(_ metrics: MarkdownMetrics) -> InlineAttributes {
        InlineAttributes(style: metrics.body, color: metrics.textColor)
    }

    func resolved(_ metrics: MarkdownMetrics) -> [NSAttributedString.Key: Any] {
        var attributes: [NSAttributedString.Key: Any] = [
            .font: font,
            .foregroundColor: color.nsColor,
        ]
        if strike { attributes[.strikethroughStyle] = NSUnderlineStyle.single.rawValue }
        if superscript { attributes[.superscript] = 1 }
        if chip { attributes[.backgroundColor] = metrics.theme.color.surfaceRaised.nsColor }
        if let link { attributes[.link] = link }
        return attributes
    }

    private var font: NSFont {
        let resolved = bold ? style.weighted(.semibold) : style
        let base = resolved.nsFont
        guard italic else { return base }
        let italicised = NSFontManager.shared.convert(base, toHaveTrait: .italicFontMask)
        // `convert` returns the input when the family has no italic face; the oblique
        // matrix is the fallback so emphasis is never silently invisible.
        guard italicised == base else { return italicised }
        return NSFont(
            descriptor: base.fontDescriptor.withSymbolicTraits(.italic), size: base.pointSize)
            ?? base
    }
}
