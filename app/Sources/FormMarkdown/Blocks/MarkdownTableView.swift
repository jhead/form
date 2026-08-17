import FormCore
import FormDesign
import SwiftUI

/// A GFM table: header emphasis, per-column alignment, zebra rows, and horizontal scroll
/// when it is wider than the column (spec 11 §2).
struct MarkdownTableView: View {
    let align: [ColumnAlign]
    let header: [[Span]]
    let rows: [[[Span]]]
    let metrics: MarkdownMetrics

    var body: some View {
        ScrollView(.horizontal) {
            Grid(alignment: .topLeading, horizontalSpacing: 0, verticalSpacing: 0) {
                GridRow {
                    ForEach(Array(header.enumerated()), id: \.offset) { index, cell in
                        self.cell(
                            cell, index: index,
                            style: metrics.theme.typography.bodyStrong,
                            color: metrics.theme.color.textPrimary
                        )
                        // The first row fixes each column's alignment for the whole grid.
                        .gridColumnAlignment(alignment(index))
                    }
                }
                .background(metrics.theme.color.surfaceRaised)

                ForEach(Array(rows.enumerated()), id: \.offset) { rowIndex, row in
                    GridRow {
                        ForEach(Array(row.enumerated()), id: \.offset) { index, cell in
                            self.cell(
                                cell, index: index, style: metrics.body, color: metrics.textColor)
                        }
                    }
                    .background(zebra(rowIndex))
                }
            }
            .overlay(
                RoundedRectangle(cornerRadius: metrics.theme.metrics.radius.md, style: .continuous)
                    .strokeBorder(metrics.theme.color.border, lineWidth: metrics.hairline * 2)
            )
            .clipShape(
                RoundedRectangle(cornerRadius: metrics.theme.metrics.radius.md, style: .continuous))
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func cell(
        _ spans: [Span], index: Int, style: TypeStyle, color: ThemeColor
    ) -> some View {
        Text(MarkdownInlineText.attributed(spans, metrics: metrics, style: style, color: color))
            .multilineTextAlignment(textAlignment(index))
            .textSelection(.enabled)
            .padding(.horizontal, metrics.cellPaddingH)
            .padding(.vertical, metrics.cellPaddingV)
    }

    /// Zebra at 3% surface tint (`metrics.zebraOpacity`), on alternate body rows.
    private func zebra(_ index: Int) -> ThemeColor {
        index.isMultiple(of: 2)
            ? metrics.theme.color.surface.opacity(0)
            : metrics.theme.color.textPrimary.opacity(metrics.theme.metrics.zebraOpacity)
    }

    private func columnAlign(_ index: Int) -> ColumnAlign {
        index < align.count ? align[index] : .none
    }

    private func alignment(_ index: Int) -> HorizontalAlignment {
        switch columnAlign(index) {
        case .center: .center
        case .right: .trailing
        case .left, .none: .leading
        }
    }

    private func textAlignment(_ index: Int) -> TextAlignment {
        switch columnAlign(index) {
        case .center: .center
        case .right: .trailing
        case .left, .none: .leading
        }
    }
}
