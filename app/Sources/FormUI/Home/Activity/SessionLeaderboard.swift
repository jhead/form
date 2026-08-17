import FormCore
import FormDesign
import SwiftUI

/// One of the three ranked session lists (F11.9). Clicking a row opens that session.
struct SessionLeaderboard: View {
    @Environment(\.theme) private var theme

    enum Metric {
        case tokens, duration, turns

        var title: String {
            switch self {
            case .tokens: "Most tokens"
            case .duration: "Longest running"
            case .turns: "Most turns"
            }
        }

        var subtitle: String {
            switch self {
            case .tokens: "Sessions by total tokens"
            case .duration: "Sessions by wall time"
            case .turns: "Sessions by turn count"
            }
        }

        var icon: String {
            switch self {
            case .tokens: "number"
            case .duration: "clock"
            case .turns: "arrow.triangle.2.circlepath"
            }
        }

        func value(_ rank: SessionRank) -> String {
            switch self {
            case .tokens: StatsFormat.abbreviated(rank.tokens)
            case .duration: StatsFormat.duration(ms: rank.durationMs)
            case .turns: StatsFormat.grouped(rank.turns)
            }
        }
    }

    let metric: Metric
    let ranks: [SessionRank]
    let onOpen: (String) -> Void
    var metrics: HomeMetrics = .standard

    var body: some View {
        VStack(spacing: 0) {
            ForEach(Array(ranks.enumerated()), id: \.element.id) { index, rank in
                ListRow(height: metrics.leaderboardRowHeight, action: { onOpen(rank.sessionId) }) { _ in
                    HStack(spacing: theme.metrics.spacing.lg) {
                        Text("\(index + 1)")
                            .typeStyle(theme.typography.micro)
                            .tabularFigures()
                            .foregroundStyle(theme.color.textTertiary)
                            .frame(width: theme.metrics.spacing.xl, alignment: .trailing)

                        Text(rank.title.isEmpty ? "Untitled session" : rank.title)
                            .typeStyle(theme.typography.caption)
                            .foregroundStyle(theme.color.textPrimary)
                            .lineLimit(1)
                            .truncationMode(.tail)

                        Spacer(minLength: theme.metrics.spacing.md)

                        Text(metric.value(rank))
                            .typeStyle(theme.typography.caption)
                            .tabularFigures()
                            .foregroundStyle(theme.color.textSecondary)

                        Image(systemName: "chevron.right")
                            .imageScale(.small)
                            .foregroundStyle(theme.color.textTertiary)
                    }
                }
                .formTooltip(
                    rank.title,
                    detail:
                        "\(StatsFormat.grouped(rank.tokens)) tokens · \(StatsFormat.duration(ms: rank.durationMs)) · \(StatsFormat.grouped(rank.turns)) turns"
                )
                .accessibilityLabel("\(rank.title), \(metric.value(rank))")
                .accessibilityHint("Opens this session")
            }
        }
    }
}

#Preview("Leaderboard") {
    HomePreviewStage(width: 520) {
        ChartCard(title: "Most tokens", subtitle: "Sessions by total tokens") {
            SessionLeaderboard(
                metric: .tokens, ranks: HomePreviewData.populated.sessionsTop.byTokens,
                onOpen: { _ in })
        }
    }
}

#Preview("Leaderboard — empty") {
    HomePreviewStage(theme: .dark, width: 520) {
        ChartCard(
            title: "Most tokens", isEmpty: true, emptyIcon: "number",
            emptyTitle: "No sessions yet",
            emptyMessage: "Your busiest sessions will be listed here."
        ) {
            EmptyView()
        }
    }
}
