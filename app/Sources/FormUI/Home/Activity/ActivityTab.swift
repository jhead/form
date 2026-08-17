import FormCore
import FormDesign
import SwiftUI

/// Activity (spec 12 §2): when the work happens, how long turns take, which tools run, and
/// which sessions were the biggest.
struct ActivityTab: View {
    @Environment(\.theme) private var theme

    let stats: UsageStats
    let onOpenSession: (String) -> Void
    var metrics: HomeMetrics = .standard

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.xl) {
            HStack(alignment: .top, spacing: theme.metrics.spacing.xl) {
                ChartCard(
                    title: "Hour of day",
                    subtitle: "Tokens by local hour · peak \(StatsFormat.hour(stats.headline.peakHour))",
                    height: metrics.chartCompact,
                    isEmpty: !hasHourly,
                    emptyIcon: "clock",
                    emptyTitle: "No hourly activity yet",
                    emptyMessage: "Every hour of the day appears here once you've sent a message."
                ) {
                    HourHistogram(hourly: stats.hourly, peakHour: stats.headline.peakHour)
                }

                ChartCard(
                    title: "Turn duration",
                    subtitle: "Mean per day",
                    height: metrics.chartCompact,
                    isEmpty: !hasTurns,
                    emptyIcon: "timer",
                    emptyTitle: "No finished turns yet",
                    emptyMessage: "Turn timings appear once a run completes."
                ) {
                    TurnDurationChart(
                        daily: stats.daily, averageMs: stats.headline.avgTurnDurationMs)
                }
            }

            ChartCard(
                title: "Weekday × hour",
                subtitle: "Tokens by weekday and local hour",
                isEmpty: !hasMatrix,
                emptyIcon: "square.grid.3x3",
                emptyTitle: "Nothing to map yet",
                emptyMessage: "The matrix fills in as your week takes shape."
            ) {
                WeekdayHourMatrix(weekdayHour: stats.weekdayHour, metrics: metrics)
            }

            ChartCard(
                title: "Tool usage",
                subtitle: "Most-invoked tools, success rate and mean duration",
                isEmpty: stats.tools.isEmpty,
                emptyIcon: "wrench.and.screwdriver",
                emptyTitle: "No tools have run yet",
                emptyMessage: "Tool calls are counted here as the agent makes them."
            ) {
                ToolUsageCard(tools: stats.tools, metrics: metrics)
            }

            HStack(alignment: .top, spacing: theme.metrics.spacing.xl) {
                leaderboard(.tokens, ranks: stats.sessionsTop.byTokens)
                leaderboard(.duration, ranks: stats.sessionsTop.byDuration)
                leaderboard(.turns, ranks: stats.sessionsTop.byTurns)
            }
        }
    }

    private func leaderboard(_ metric: SessionLeaderboard.Metric, ranks: [SessionRank]) -> some View {
        ChartCard(
            title: metric.title,
            subtitle: metric.subtitle,
            isEmpty: ranks.isEmpty,
            emptyIcon: metric.icon,
            emptyTitle: "No sessions yet",
            emptyMessage: "Your busiest sessions will be listed here."
        ) {
            SessionLeaderboard(
                metric: metric, ranks: ranks, onOpen: onOpenSession, metrics: metrics)
        }
    }

    private var hasHourly: Bool { stats.hourly.contains { $0.totalTokens > 0 } }
    private var hasTurns: Bool { stats.daily.contains { $0.turns > 0 } }
    private var hasMatrix: Bool { stats.weekdayHour.contains { $0.contains { $0 > 0 } } }
}

#Preview("Activity") {
    HomePreviewStage(width: 1_080) {
        ActivityTab(stats: HomePreviewData.populated, onOpenSession: { _ in })
    }
}

#Preview("Activity — empty") {
    HomePreviewStage(theme: .dark, width: 1_080) {
        ActivityTab(stats: HomePreviewData.empty, onOpenSession: { _ in })
    }
}
