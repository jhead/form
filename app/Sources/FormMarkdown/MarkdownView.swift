import FormCore
import FormDesign
import SwiftUI

/// Renders the block tree the core produces (spec 11). **Parses nothing** — parsing and
/// syntax highlighting run in Rust (spec 05) and what arrives here is structure plus scope
/// names, so three future platforms share one parser and all color decisions stay in
/// `FormDesign`.
///
/// Blocks are keyed by the core's stable ids: an id is a hash of the block, so it changes
/// exactly when the block's rendering would (spec 05 §2). That is what keeps a streaming
/// response from rebuilding every view in the transcript on every token (F7.3).
public struct MarkdownView: View {
    @Environment(\.theme) private var theme

    private let doc: MarkdownDoc
    private let style: MarkdownStyle

    public init(doc: MarkdownDoc, style: MarkdownStyle = .default) {
        self.doc = doc
        self.style = style
    }

    public var body: some View {
        MarkdownBlocksView(
            blocks: doc.blocks, metrics: MarkdownMetrics(theme: theme, style: style))
    }
}

/// The engine: segments blocks into selectable text runs and native blocks, and recurses for
/// quotes, list items and footnotes.
struct MarkdownBlocksView: View {
    let blocks: [MarkdownBlock]
    let metrics: MarkdownMetrics
    var sourcePrefix: String = ""
    /// List nesting, for markers and for the source indentation copy reproduces.
    var depth: Int = 0

    var body: some View {
        VStack(alignment: .leading, spacing: metrics.blockSpacing) {
            ForEach(MarkdownRun.segment(blocks)) { run in
                view(for: run)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        // Blocks appear as they stream; the caret is the motion, not the layout (spec 11 §4).
        .transaction { $0.animation = nil }
        .environment(
            \.openURL,
            OpenURLAction { url in
                MarkdownLink.open(url)
                return .handled
            })
    }

    @ViewBuilder
    private func view(for run: MarkdownRun) -> some View {
        switch run {
        case let .text(blocks):
            MarkdownTextRun(
                rendered: renderedText(
                    blocks, metrics: metrics, sourcePrefix: sourcePrefix, depth: depth),
                metrics: metrics,
                contentKey: run.contentKey)
        case let .native(block):
            native(block)
        }
    }

    @ViewBuilder
    private func native(_ block: MarkdownBlock) -> some View {
        switch block.kind {
        case let .codeBlock(language, code, tokens, partial):
            CodeBlockView(
                language: language, code: code, tokens: tokens, partial: partial,
                metrics: metrics)

        case let .table(align, header, rows):
            MarkdownTableView(align: align, header: header, rows: rows, metrics: metrics)

        case let .image(url, alt, title):
            MarkdownImageView(url: url, alt: alt, title: title, metrics: metrics)

        case let .quote(blocks):
            MarkdownQuoteView(blocks: blocks, metrics: metrics, sourcePrefix: sourcePrefix)

        case .rule:
            MarkdownRuleView(metrics: metrics)

        case let .list(ordered, start, tight, items):
            MarkdownListView(
                ordered: ordered, start: start, tight: tight, items: items, depth: depth,
                metrics: metrics, sourcePrefix: sourcePrefix)

        case let .footnoteDef(label, blocks):
            MarkdownFootnoteView(
                label: label, blocks: blocks, metrics: metrics, sourcePrefix: sourcePrefix)

        case .paragraph, .heading, .html, .unknown:
            // Textual kinds never reach here; `unknown` is a block from a newer core, and
            // dropping it beats rendering a placeholder into someone's transcript.
            EmptyView()
        }
    }
}

#Preview("markdown — everything") {
    ScrollView {
        ThemePreview {
            MarkdownView(doc: MarkdownFixture.everything)
        }
    }
    .frame(width: 900, height: 900)
}

#Preview("markdown — streaming tail") {
    ThemePreview {
        MarkdownView(doc: MarkdownFixture.streamingTail)
    }
    .frame(width: 900)
}

#Preview("markdown — line numbers and wrap") {
    ThemePreview {
        MarkdownView(
            doc: MarkdownFixture.codeOnly,
            style: MarkdownStyle(showLineNumbers: true, wrapCode: false))
    }
    .frame(width: 900)
}
