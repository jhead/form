import SwiftUI
import FormCore
import FormDesign
import FormMarkdown

/// Renders a parsed document, one view per block, keyed by the core's stable block ids.
///
/// **Temporary bridge.** Spec 11 §1 gives `FormMarkdown.MarkdownView` the signature
/// `init(doc:style:)`; W11 has not landed it yet and today's placeholder takes a `String`.
/// Keeping the block loop here means the streaming property spec 10 §2 asks for — only the
/// tail block re-renders — is already true and measurable. When W11 lands, this whole body
/// becomes `MarkdownView(doc: doc)` and `BlockText` goes away.
struct MarkdownDocView: View {
    @Environment(\.theme) private var theme

    let doc: MarkdownDoc

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.lg) {
            ForEach(doc.blocks, id: \.id) { block in
                MarkdownView(text: BlockText.of(block))
                    .typeStyle(theme.typography.body)
                    .foregroundStyle(theme.color.textPrimary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .textSelection(.enabled)
            }
        }
    }
}

/// Flattens a block to the text the placeholder can show. Deleted with the bridge above —
/// this is not a renderer and must not grow into one.
enum BlockText {
    static func of(_ block: MarkdownBlock) -> String {
        switch block.kind {
        case let .paragraph(spans):
            spans.map(\.plainText).joined()
        case let .heading(_, spans, _):
            spans.map(\.plainText).joined()
        case let .codeBlock(_, code, _, _):
            code
        case let .list(ordered, start, _, items):
            items.enumerated()
                .map { index, item in
                    let marker = ordered ? "\(start + Int64(index))." : "•"
                    return "\(marker) " + item.blocks.map(of).joined(separator: " ")
                }
                .joined(separator: "\n")
        case let .quote(blocks):
            blocks.map(of).joined(separator: "\n")
        case let .table(_, header, rows):
            ([header] + rows)
                .map { row in row.map { $0.map(\.plainText).joined() }.joined(separator: "  ") }
                .joined(separator: "\n")
        case .rule:
            "———"
        case let .image(_, alt, _):
            alt
        case let .html(raw):
            raw
        case let .footnoteDef(label, blocks):
            "[\(label)] " + blocks.map(of).joined(separator: " ")
        case .unknown:
            ""
        }
    }
}
