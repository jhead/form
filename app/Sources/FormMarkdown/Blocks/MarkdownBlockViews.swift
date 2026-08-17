import FormCore
import FormDesign
import SwiftUI

/// A blockquote: 2 pt leading rule, 12 pt inset, secondary text (spec 11 §2).
///
/// Native rather than part of a text run because the rule is a drawn shape — TextKit has no
/// attribute for it — and because a quote can contain a code block or a table, which are
/// native anyway.
struct MarkdownQuoteView: View {
    let blocks: [MarkdownBlock]
    let metrics: MarkdownMetrics
    let sourcePrefix: String

    var body: some View {
        HStack(alignment: .top, spacing: 0) {
            RoundedRectangle(
                cornerRadius: metrics.theme.metrics.radius.sm, style: .continuous
            )
            .fill(metrics.theme.color.border)
            .frame(width: metrics.quoteRule)

            MarkdownBlocksView(
                blocks: blocks,
                metrics: metrics.quoted(),
                // Copying out of a quote should still be a quote.
                sourcePrefix: sourcePrefix + "> "
            )
            .padding(.leading, metrics.quoteInset)
        }
        .fixedSize(horizontal: false, vertical: true)
    }
}

/// A thematic break.
struct MarkdownRuleView: View {
    let metrics: MarkdownMetrics

    var body: some View {
        Rectangle()
            .fill(metrics.theme.color.border)
            .frame(height: metrics.hairline * 2)
            .padding(.vertical, metrics.theme.metrics.spacing.md)
    }
}

/// A list that cannot live in a text run because something non-textual is nested in it — a
/// code block inside a step, most often. Markers and indents match the text-run rendering
/// so the two are indistinguishable on screen.
struct MarkdownListView: View {
    let ordered: Bool
    let start: Int64
    let tight: Bool
    let items: [ListItem]
    let depth: Int
    let metrics: MarkdownMetrics
    let sourcePrefix: String

    var body: some View {
        VStack(alignment: .leading, spacing: tight ? metrics.listItemSpacing : metrics.blockSpacing)
        {
            ForEach(Array(items.enumerated()), id: \.offset) { index, item in
                HStack(alignment: .top, spacing: metrics.theme.metrics.spacing.sm) {
                    Text(marker(index))
                        .typeStyle(metrics.body)
                        .foregroundStyle(metrics.theme.color.textSecondary)
                        .frame(minWidth: metrics.listIndent, alignment: .leading)
                    MarkdownBlocksView(
                        blocks: item.blocks,
                        metrics: metrics,
                        sourcePrefix: sourcePrefix + "  ",
                        depth: depth + 1
                    )
                }
            }
        }
    }

    private func marker(_ index: Int) -> String {
        let bullet = ordered
            ? "\(start &+ Int64(index))." : metrics.bullet(depth: depth)
        guard let checked = items[index].checked else { return bullet }
        return "\(bullet) \(MarkdownMetrics.checkbox(checked))"
    }
}

/// A footnote definition whose body is not purely textual.
struct MarkdownFootnoteView: View {
    let label: String
    let blocks: [MarkdownBlock]
    let metrics: MarkdownMetrics
    let sourcePrefix: String

    var body: some View {
        HStack(alignment: .top, spacing: metrics.theme.metrics.spacing.sm) {
            Text(label)
                .typeStyle(metrics.theme.typography.micro)
                .foregroundStyle(metrics.theme.color.accent)
            MarkdownBlocksView(
                blocks: blocks, metrics: metrics, sourcePrefix: sourcePrefix + "    ")
        }
    }
}
