import FormCore
import FormDesign
import SwiftUI

/// Most-invoked tools with their success rate and mean duration (F11.8).
///
/// The bars rank by invocation count; the rows beside them carry the two numbers a bar
/// cannot show without lying about its scale.
struct ToolUsageCard: View {
    @Environment(\.theme) private var theme

    let tools: [ToolStat]
    var metrics: HomeMetrics = .standard

    /// Ranked here rather than trusted from the document: the core sorts by invocations,
    /// but a card titled "most-invoked" must not depend on that promise. Ordering is
    /// presentation — no number is recomputed.
    private var ranked: [ToolStat] { tools.sorted { $0.invocations > $1.invocations } }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.lg) {
            RankedBarChart(rows: rows, format: .count, metrics: metrics)

            FormDivider()

            VStack(spacing: 0) {
                ForEach(Array(ranked.enumerated()), id: \.element.id) { index, tool in
                    HStack(spacing: theme.metrics.spacing.md) {
                        RoundedRectangle(cornerRadius: theme.metrics.radius.sm, style: .continuous)
                            .fill(theme.color.series(index))
                            .frame(width: metrics.legendSwatch, height: metrics.legendSwatch)
                        Text(tool.name)
                            .typeStyle(theme.typography.caption)
                            .foregroundStyle(theme.color.textPrimary)
                            .lineLimit(1)
                        Spacer(minLength: theme.metrics.spacing.md)
                        Text(StatsFormat.duration(ms: tool.meanDurationMs))
                            .typeStyle(theme.typography.micro)
                            .tabularFigures()
                            .foregroundStyle(theme.color.textTertiary)
                        Badge(
                            StatsFormat.percent(tool.successRate),
                            tone: tool.successRate >= 0.95 ? .success : .warning)
                    }
                    .frame(height: metrics.tableRowHeight)
                    .formTooltip(
                        tool.name,
                        detail:
                            "\(StatsFormat.grouped(tool.invocations)) invocations · \(StatsFormat.percent(tool.successRate, decimals: 1)) succeeded · \(StatsFormat.duration(ms: tool.meanDurationMs)) mean"
                    )
                }
            }
        }
    }

    private var rows: [RankedBarRow] {
        ranked.enumerated().map { index, tool in
            RankedBarRow(
                id: tool.id, label: tool.name, value: Double(tool.invocations), colorIndex: index,
                detail: StatsFormat.grouped(tool.invocations))
        }
    }
}

#Preview("Tool usage") {
    HomePreviewStage {
        ChartCard(title: "Tool usage") {
            ToolUsageCard(tools: HomePreviewData.populated.tools)
        }
    }
}

#Preview("Tool usage — dark") {
    HomePreviewStage(theme: .dark) {
        ChartCard(title: "Tool usage") {
            ToolUsageCard(tools: HomePreviewData.populated.tools)
        }
    }
}
