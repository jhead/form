import FormCore
import FormDesign
import SwiftUI

/// The 4 × 2 tile grid at the top of Overview (F11.1).
///
/// Every value is abbreviated for the tile and exact in the tooltip, because `21.8M` is the
/// number you read at a glance and `21,844,203` is the one you quote.
struct HeadlineTiles: View {
    @Environment(\.theme) private var theme

    let stats: UsageStats
    var metrics: HomeMetrics = .standard

    var body: some View {
        LazyVGrid(
            columns: [GridItem(.adaptive(minimum: metrics.tileMinWidth), spacing: theme.metrics.spacing.xl)],
            spacing: theme.metrics.spacing.xl
        ) {
            ForEach(tiles) { tile in
                HeadlineTile(tile: tile)
            }
        }
    }

    private var headline: Headline { stats.headline }

    private var tiles: [HeadlineTileModel] {
        [
            HeadlineTileModel(
                label: "Sessions", value: StatsFormat.abbreviated(headline.sessions),
                exact: StatsFormat.grouped(headline.sessions), icon: "bubble.left.and.bubble.right"),
            HeadlineTileModel(
                label: "Messages", value: StatsFormat.abbreviated(headline.messages),
                exact: StatsFormat.grouped(headline.messages), icon: "text.bubble"),
            HeadlineTileModel(
                label: "Total tokens", value: StatsFormat.abbreviated(headline.totalTokens),
                exact: StatsFormat.grouped(headline.totalTokens),
                detail: "\(StatsFormat.abbreviated(headline.input)) in · \(StatsFormat.abbreviated(headline.output)) out",
                icon: "number"),
            HeadlineTileModel(
                label: "Active days", value: "\(headline.activeDays)",
                exact: "\(headline.activeDays) days with at least one turn", icon: "calendar"),
            HeadlineTileModel(
                label: "Current streak", value: "\(headline.currentStreak)",
                exact: "\(headline.currentStreak) consecutive days", detail: dayWord(headline.currentStreak),
                icon: "flame"),
            HeadlineTileModel(
                label: "Longest streak", value: "\(headline.longestStreak)",
                exact: "\(headline.longestStreak) consecutive days", detail: dayWord(headline.longestStreak),
                icon: "trophy"),
            HeadlineTileModel(
                label: "Peak hour", value: StatsFormat.hour(headline.peakHour),
                exact: "Most tokens are spent in the hour starting \(StatsFormat.hour(headline.peakHour))",
                icon: "clock"),
            HeadlineTileModel(
                label: "Favorite model", value: favoriteModelName,
                exact: favoriteModelDetail, icon: "sparkle"),
        ]
    }

    private func dayWord(_ count: Int) -> String { count == 1 ? "day" : "days" }

    private var favoriteModelName: String {
        guard let favorite = headline.favoriteModel else { return "—" }
        if let match = stats.models.first(where: { $0.model.slug == favorite.slug }),
            !match.displayName.isEmpty
        {
            return match.displayName
        }
        return favorite.modelId.titleCasedIdentifier
    }

    private var favoriteModelDetail: String {
        guard let favorite = headline.favoriteModel else {
            return "No model has run in this range yet."
        }
        return favorite.slug
    }
}

struct HeadlineTileModel: Identifiable {
    let label: String
    let value: String
    let exact: String
    var detail: String?
    let icon: String

    var id: String { label }
}

private struct HeadlineTile: View {
    @Environment(\.theme) private var theme

    let tile: HeadlineTileModel

    var body: some View {
        FormCard {
            VStack(alignment: .leading, spacing: theme.metrics.spacing.xs) {
                HStack(spacing: theme.metrics.spacing.sm) {
                    Image(systemName: tile.icon)
                        .imageScale(.small)
                        .foregroundStyle(theme.color.textTertiary)
                    Text(tile.label)
                        .typeStyle(theme.typography.micro)
                        .foregroundStyle(theme.color.textTertiary)
                }

                Text(tile.value)
                    .typeStyle(theme.typography.title)
                    .tabularFigures()
                    .foregroundStyle(theme.color.textPrimary)
                    .lineLimit(1)
                    .minimumScaleFactor(0.7)

                if let detail = tile.detail {
                    Text(detail)
                        .typeStyle(theme.typography.micro)
                        .foregroundStyle(theme.color.textTertiary)
                        .lineLimit(1)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .formTooltip(tile.label, detail: tile.exact)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(tile.label): \(tile.exact)")
    }
}

#Preview("Headline tiles") {
    HomePreviewStage(width: 1_000) {
        HeadlineTiles(stats: HomePreviewData.populated)
    }
}

#Preview("Headline tiles — empty") {
    HomePreviewStage(theme: .dark, width: 1_000) {
        HeadlineTiles(stats: HomePreviewData.empty)
    }
}
