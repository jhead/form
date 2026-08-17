import FormCore
import FormDesign
import SwiftUI

/// Overview (spec 12 §2): headline tiles, the contribution calendar, tokens over time,
/// sessions and messages per day, and the footnote.
struct OverviewTab: View {
    @Environment(\.theme) private var theme

    let stats: UsageStats
    var metrics: HomeMetrics = .standard

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.xl) {
            HeadlineTiles(stats: stats, metrics: metrics)

            ChartCard(
                title: "Activity",
                subtitle: "Tokens per day",
                height: nil,
                isEmpty: stats.heatmap.isEmpty,
                emptyIcon: "square.grid.3x3",
                emptyTitle: "No days to show yet",
                emptyMessage: "Each square is a day. They fill in as you use form."
            ) {
                ActivityHeatmap(cells: stats.heatmap, metrics: metrics)
            }

            HStack(alignment: .top, spacing: theme.metrics.spacing.xl) {
                ChartCard(
                    title: "Tokens over time",
                    subtitle: "Input, output and cache traffic per day",
                    legend: ChartSeries.tokenSeries.map { ChartLegendItem($0) },
                    height: metrics.chart,
                    isEmpty: !hasTokens,
                    emptyIcon: "chart.xyaxis.line",
                    emptyTitle: "No tokens in this range",
                    emptyMessage: "Send a message and the day's usage lands here."
                ) {
                    TokensOverTimeChart(daily: stats.daily, metrics: metrics)
                }

                ChartCard(
                    title: "Sessions and messages",
                    subtitle: "Bars are sessions; the line is messages",
                    legend: [
                        ChartLegendItem(.sessions), ChartLegendItem(.messages),
                    ],
                    height: metrics.chart,
                    isEmpty: !hasSessions,
                    emptyIcon: "chart.bar",
                    emptyTitle: "No sessions in this range",
                    emptyMessage: "Start a chat and the daily counts appear here."
                ) {
                    SessionsMessagesChart(daily: stats.daily, metrics: metrics)
                }
            }

            if let footnote = TokenComparison.sentence(forTokens: stats.headline.totalTokens) {
                Text(footnote)
                    .typeStyle(theme.typography.micro)
                    .foregroundStyle(theme.color.textTertiary)
                    .frame(maxWidth: .infinity, alignment: .center)
                    .padding(.top, theme.metrics.spacing.xs)
            }
        }
    }

    private var hasTokens: Bool { stats.daily.contains { $0.totalTokens > 0 } }
    private var hasSessions: Bool { stats.daily.contains { $0.sessions > 0 || $0.messages > 0 } }
}

#Preview("Overview") {
    HomePreviewStage(width: 1_080) {
        OverviewTab(stats: HomePreviewData.populated)
    }
}

#Preview("Overview — empty") {
    HomePreviewStage(theme: .dark, width: 1_080) {
        OverviewTab(stats: HomePreviewData.empty)
    }
}

#Preview("Overview — sparse") {
    HomePreviewStage(width: 1_080) {
        OverviewTab(stats: HomePreviewData.sparse)
    }
}
