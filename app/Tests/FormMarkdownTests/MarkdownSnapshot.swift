import AppKit
import FormCore
import FormDesign
import Foundation
import SwiftUI

@testable import FormMarkdown

/// A deterministic, readable dump of what `MarkdownView` would draw.
///
/// A pixel snapshot is not available here: `ImageRenderer` does not rasterise
/// `NSViewRepresentable` content, and the whole text-run half of this module *is* an
/// `NSTextView`. So the snapshot captures the thing a pixel diff would be a proxy for — the
/// resolved font, weight, color and indentation of every piece of content, per theme. That
/// makes it strictly more useful for the property spec 11 §5 actually asks about ("renders
/// correctly in both themes"), because a color regression names itself instead of showing up
/// as a diff of grey squares.
///
/// Fonts are described by size and symbolic traits rather than by PostScript name, so the
/// golden files do not churn when the system font is renamed.
@MainActor
enum MarkdownSnapshot {
    static func describe(
        _ doc: MarkdownDoc, theme: Theme, style: MarkdownStyle = .default
    ) -> String {
        let metrics = MarkdownMetrics(theme: theme, style: style)
        var out = ["theme: \(theme.id)"]
        describe(doc.blocks, metrics: metrics, indent: 0, into: &out)
        return out.joined(separator: "\n") + "\n"
    }

    private static func describe(
        _ blocks: [MarkdownBlock], metrics: MarkdownMetrics, indent: Int, into out: inout [String]
    ) {
        let pad = String(repeating: "  ", count: indent)
        for run in MarkdownRun.segment(blocks) {
            switch run {
            case let .text(blocks):
                out.append("\(pad)textRun(\(blocks.count) blocks)")
                let rendered = MarkdownAttributedBuilder.render(blocks, metrics: metrics)
                out.append(contentsOf: attributeRuns(rendered, pad: pad + "  "))
                out.append("\(pad)  source: \(escape(rendered.map.source))")
            case let .native(block):
                describe(block, metrics: metrics, indent: indent, into: &out)
            }
        }
    }

    private static func describe(
        _ block: MarkdownBlock, metrics: MarkdownMetrics, indent: Int, into out: inout [String]
    ) {
        let pad = String(repeating: "  ", count: indent)
        switch block.kind {
        case let .codeBlock(language, code, tokens, partial):
            out.append(
                "\(pad)code(language: \(language ?? "-"), lines: "
                    + "\(CodeHighlighting.lineCount(of: code)), partial: \(partial))")
            let attributed = CodeHighlighting.attributed(
                code: code, tokens: tokens, metrics: metrics)
            for piece in attributed.runs {
                let text = String(attributed[piece.range].characters)
                out.append(
                    "\(pad)  \(escape(text))  color=\(hex(piece.foregroundColor))")
            }

        case let .table(align, header, rows):
            out.append("\(pad)table(align: \(align.map(\.rawValue).joined(separator: ",")))")
            out.append("\(pad)  header: \(header.map { plain($0) }.joined(separator: " | "))")
            for (index, row) in rows.enumerated() {
                let zebra = index.isMultiple(of: 2) ? "-" : "zebra"
                out.append(
                    "\(pad)  row[\(index)] \(zebra): "
                        + row.map { plain($0) }.joined(separator: " | "))
            }

        case let .image(url, alt, title):
            out.append(
                "\(pad)image(url: \(url), alt: \(alt), title: \(title ?? "-"), "
                    + "reserved: \(metrics.imageMaxHeight))")

        case let .quote(blocks):
            out.append("\(pad)quote(rule: \(metrics.quoteRule), inset: \(metrics.quoteInset))")
            describe(blocks, metrics: metrics.quoted(), indent: indent + 1, into: &out)

        case .rule:
            out.append("\(pad)rule")

        case let .list(ordered, start, tight, items):
            out.append("\(pad)list(ordered: \(ordered), start: \(start), tight: \(tight))")
            for item in items {
                out.append("\(pad)  item(checked: \(item.checked.map(String.init) ?? "-"))")
                describe(item.blocks, metrics: metrics, indent: indent + 2, into: &out)
            }

        case let .footnoteDef(label, blocks):
            out.append("\(pad)footnote(\(label))")
            describe(blocks, metrics: metrics, indent: indent + 1, into: &out)

        case .paragraph, .heading, .html, .unknown:
            out.append("\(pad)unrendered(\(block.kind.type))")
        }
    }

    // MARK: Attribute runs

    private static func attributeRuns(_ rendered: RenderedText, pad: String) -> [String] {
        var lines: [String] = []
        let string = rendered.attributed
        string.enumerateAttributes(
            in: NSRange(location: 0, length: string.length), options: []
        ) { attributes, range, _ in
            let text = string.attributedSubstring(from: range).string
            var parts: [String] = [escape(text)]
            if let font = attributes[.font] as? NSFont { parts.append(describe(font)) }
            if let color = attributes[.foregroundColor] as? NSColor {
                parts.append("color=\(hex(color))")
            }
            if let background = attributes[.backgroundColor] as? NSColor {
                parts.append("chip=\(hex(background))")
            }
            if attributes[.strikethroughStyle] != nil { parts.append("strike") }
            if attributes[.superscript] != nil { parts.append("superscript") }
            if let link = attributes[.link] as? URL { parts.append("link=\(link.absoluteString)") }
            if let style = attributes[.paragraphStyle] as? NSParagraphStyle {
                parts.append(
                    "indent=\(style.firstLineHeadIndent)/\(style.headIndent) "
                        + "after=\(style.paragraphSpacing) before=\(style.paragraphSpacingBefore)")
            }
            lines.append(pad + parts.joined(separator: "  "))
        }
        return lines
    }

    private static func describe(_ font: NSFont) -> String {
        let traits = font.fontDescriptor.symbolicTraits
        var flags: [String] = []
        if traits.contains(.bold) { flags.append("bold") }
        if traits.contains(.italic) { flags.append("italic") }
        if traits.contains(.monoSpace) { flags.append("mono") }
        return "font=\(font.pointSize)\(flags.isEmpty ? "" : "[\(flags.joined(separator: ","))]")"
    }

    // MARK: Helpers

    static func plain(_ spans: [Span]) -> String {
        spans.map(\.plainText).joined()
    }

    private static func escape(_ text: String) -> String {
        "\"" + text.replacingOccurrences(of: "\n", with: "\\n") + "\""
    }

    private static func hex(_ color: NSColor?) -> String {
        guard let srgb = color?.usingColorSpace(.sRGB) else { return "-" }
        let value = (
            Int((srgb.redComponent * 255).rounded()),
            Int((srgb.greenComponent * 255).rounded()),
            Int((srgb.blueComponent * 255).rounded()),
            Int((srgb.alphaComponent * 255).rounded())
        )
        return value.3 == 255
            ? String(format: "#%02X%02X%02X", value.0, value.1, value.2)
            : String(format: "#%02X%02X%02X%02X", value.0, value.1, value.2, value.3)
    }

    private static func hex(_ color: Color?) -> String {
        guard let color else { return "-" }
        return hex(NSColor(color))  // FormDesign-allow: reading back a resolved token in a test
    }
}
