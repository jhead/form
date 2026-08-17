import Charts
import FormCore
import FormDesign
import SwiftUI

/// Share of tokens by model as a donut with a ranked legend (F11.5).
///
/// `share` is the core's number, already summing to 1.0 (spec 03 §3) — the donut plots it
/// rather than dividing tokens by a total of its own.
struct ModelShareDonut: View {
    @Environment(\.theme) private var theme

    let models: [ModelStat]
    let totalTokens: Int64
    var metrics: HomeMetrics = .standard

    @State private var selection: String?

    var body: some View {
        HStack(alignment: .center, spacing: theme.metrics.spacing.xl) {
            donut
            legend
        }
    }

    private var donut: some View {
        Chart(ranked, id: \.stat.id) { entry in
            SectorMark(
                angle: .value("Share", entry.stat.share),
                innerRadius: .ratio(0.62),
                angularInset: metrics.donutInset
            )
            .cornerRadius(theme.metrics.radius.sm)
            .foregroundStyle(theme.color.series(entry.rank))
            .opacity(selection == nil || selection == entry.stat.id ? 1 : 0.35)
        }
        .chartLegend(.hidden)
        .chartBackground { _ in
            VStack(spacing: theme.metrics.spacing.xxs) {
                Text(StatsFormat.abbreviated(totalTokens))
                    .typeStyle(theme.typography.title)
                    .tabularFigures()
                    .foregroundStyle(theme.color.textPrimary)
                Text("tokens")
                    .typeStyle(theme.typography.micro)
                    .foregroundStyle(theme.color.textTertiary)
            }
        }
        .frame(width: metrics.donut, height: metrics.donut)
        .accessibilityLabel("Token share by model")
    }

    private var legend: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.sm) {
            ForEach(ranked, id: \.stat.id) { entry in
                HStack(spacing: theme.metrics.spacing.md) {
                    RoundedRectangle(cornerRadius: theme.metrics.radius.sm, style: .continuous)
                        .fill(theme.color.series(entry.rank))
                        .frame(width: metrics.legendSwatch, height: metrics.legendSwatch)
                    Text(entry.name)
                        .typeStyle(theme.typography.caption)
                        .foregroundStyle(theme.color.textPrimary)
                        .lineLimit(1)
                    Spacer(minLength: theme.metrics.spacing.md)
                    Text(StatsFormat.percent(entry.stat.share))
                        .typeStyle(theme.typography.caption)
                        .tabularFigures()
                        .foregroundStyle(theme.color.textSecondary)
                    Text(StatsFormat.abbreviated(entry.stat.totalTokens))
                        .typeStyle(theme.typography.micro)
                        .tabularFigures()
                        .foregroundStyle(theme.color.textTertiary)
                        .frame(width: metrics.rankLabelWidth / 2, alignment: .trailing)
                }
                .contentShape(Rectangle())
                .onHover { selection = $0 ? entry.stat.id : nil }
                .formTooltip(
                    entry.name,
                    detail:
                        "\(StatsFormat.grouped(entry.stat.totalTokens)) tokens · \(StatsFormat.currency(entry.stat.cost))"
                )
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .animation(theme.motion.animation(.fast), value: selection)
    }

    private var ranked: [RankedModel] {
        models.enumerated().map { index, stat in
            RankedModel(rank: index, stat: stat)
        }
    }
}

struct RankedModel {
    let rank: Int
    let stat: ModelStat

    var name: String {
        stat.displayName.isEmpty ? stat.model.modelId.titleCasedIdentifier : stat.displayName
    }
}

#Preview("Model share") {
    HomePreviewStage {
        ChartCard(title: "Token share", subtitle: "Share of tokens by model") {
            ModelShareDonut(
                models: HomePreviewData.populated.models,
                totalTokens: HomePreviewData.populated.headline.totalTokens)
        }
    }
}
