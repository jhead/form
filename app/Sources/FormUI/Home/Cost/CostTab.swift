import FormCore
import FormDesign
import SwiftUI

/// Cost (spec 12 §2): spend over time, by provider, by model, the projected run rate, and
/// cache effectiveness.
struct CostTab: View {
    @Environment(\.theme) private var theme

    let stats: UsageStats
    var metrics: HomeMetrics = .standard

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.xl) {
            HStack(alignment: .top, spacing: theme.metrics.spacing.xl) {
                figureCard(
                    title: "Total spend",
                    value: StatsFormat.currencyExact(stats.cost.total),
                    caption: "Across this period",
                    icon: "dollarsign.circle")

                projectionCard

                figureCard(
                    title: "Cache savings",
                    value: StatsFormat.currencyExact(stats.cache.estimatedSavings),
                    caption:
                        "\(StatsFormat.percent(stats.cache.hitRatio)) of cache traffic was reads",
                    icon: "bolt.horizontal.circle")
            }

            ChartCard(
                title: "Spend over time",
                subtitle: "Daily spend, with the running total dashed",
                legend: [
                    ChartLegendItem("Daily", colorIndex: ChartSeries.cost.rawValue),
                    ChartLegendItem("Cumulative", colorIndex: ChartSeries.output.rawValue),
                ],
                height: metrics.chart,
                isEmpty: !hasSpend,
                emptyIcon: "chart.xyaxis.line",
                emptyTitle: "No spend in this range",
                emptyMessage: "Cost appears once a run reports usage against a priced model."
            ) {
                SpendOverTimeChart(byDay: stats.cost.byDay)
            }

            HStack(alignment: .top, spacing: theme.metrics.spacing.xl) {
                ChartCard(
                    title: "By provider",
                    isEmpty: stats.cost.byProvider.isEmpty,
                    emptyIcon: "building.2",
                    emptyTitle: "No provider spend yet"
                ) {
                    RankedBarChart(rows: providerRows, format: .currency, metrics: metrics)
                }

                ChartCard(
                    title: "By model",
                    isEmpty: stats.cost.byModel.isEmpty,
                    emptyIcon: "cpu",
                    emptyTitle: "No model spend yet"
                ) {
                    RankedBarChart(rows: modelRows, format: .currency, metrics: metrics)
                }
            }

            ChartCard(
                title: "Cache effectiveness",
                subtitle:
                    "\(StatsFormat.abbreviated(stats.cache.read)) read · \(StatsFormat.abbreviated(stats.cache.write)) written",
                legend: [
                    ChartLegendItem(.cacheRead), ChartLegendItem(.cacheWrite),
                ],
                height: metrics.chart,
                isEmpty: !hasCache,
                emptyIcon: "bolt.horizontal",
                emptyTitle: "No cache traffic yet",
                emptyMessage: "Prompt caching shows up here once a session is long enough to reuse."
            ) {
                CacheOverTimeChart(daily: stats.cache.daily)
            } accessory: {
                Badge(
                    "\(StatsFormat.percent(stats.cache.hitRatio)) hit ratio",
                    tone: stats.cache.hitRatio >= 0.5 ? .success : .neutral)
            }
        }
    }

    // MARK: Figures

    private func figureCard(title: String, value: String, caption: String, icon: String) -> some View {
        FormCard(title: title) {
            VStack(alignment: .leading, spacing: theme.metrics.spacing.xs) {
                Text(value)
                    .typeStyle(theme.typography.display)
                    .tabularFigures()
                    .foregroundStyle(theme.color.textPrimary)
                    .lineLimit(1)
                    .minimumScaleFactor(0.6)
                Text(caption)
                    .typeStyle(theme.typography.micro)
                    .foregroundStyle(theme.color.textTertiary)
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        } accessory: {
            Image(systemName: icon)
                .imageScale(.large)
                .foregroundStyle(theme.color.textTertiary)
        }
    }

    /// The projection states its basis, and refuses to guess from too little data
    /// (spec 03 §3, spec 12 §4).
    private var projectionCard: some View {
        FormCard(title: "Projected monthly") {
            VStack(alignment: .leading, spacing: theme.metrics.spacing.xs) {
                if isSparse || stats.cost.projectedMonthly <= 0 {
                    SparseValue(
                        reason:
                            "A run rate needs at least three active days; the projection is a 14-day average × 30."
                    )
                } else {
                    Text(StatsFormat.currencyExact(stats.cost.projectedMonthly))
                        .typeStyle(theme.typography.display)
                        .tabularFigures()
                        .foregroundStyle(theme.color.textPrimary)
                        .lineLimit(1)
                        .minimumScaleFactor(0.6)
                }
                Text("14-day average × 30")
                    .typeStyle(theme.typography.micro)
                    .foregroundStyle(theme.color.textTertiary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        } accessory: {
            Image(systemName: "chart.line.uptrend.xyaxis")
                .imageScale(.large)
                .foregroundStyle(theme.color.textTertiary)
        }
    }

    // MARK: Rows

    private var isSparse: Bool { stats.headline.activeDays < 3 }
    private var hasSpend: Bool { stats.cost.byDay.contains { $0.cost > 0 } }
    private var hasCache: Bool { stats.cache.daily.contains { $0.read > 0 || $0.write > 0 } }

    private var providerRows: [RankedBarRow] {
        let names = Dictionary(
            stats.providers.map { ($0.providerId, $0.name) }, uniquingKeysWith: { first, _ in first })
        return stats.cost.byProvider.sorted { $0.cost > $1.cost }.enumerated().map { index, entry in
            let name = names[entry.key].flatMap { $0.isEmpty ? nil : $0 }
            return RankedBarRow(
                id: entry.key, label: name ?? entry.key.titleCasedIdentifier, value: entry.cost,
                colorIndex: index, detail: StatsFormat.currencyExact(entry.cost))
        }
    }

    private var modelRows: [RankedBarRow] {
        let names = Dictionary(
            stats.models.map { ($0.model.slug, $0.displayName) }, uniquingKeysWith: { first, _ in first }
        )
        return stats.cost.byModel.sorted { $0.cost > $1.cost }.enumerated().map { index, entry in
            let name = names[entry.key.slug].flatMap { $0.isEmpty ? nil : $0 }
            return RankedBarRow(
                id: entry.key.slug, label: name ?? entry.key.modelId.titleCasedIdentifier,
                value: entry.cost, colorIndex: index, detail: StatsFormat.currencyExact(entry.cost))
        }
    }
}

#Preview("Cost") {
    HomePreviewStage(width: 1_080) {
        CostTab(stats: HomePreviewData.allTime)
    }
}

#Preview("Cost — empty") {
    HomePreviewStage(theme: .dark, width: 1_080) {
        CostTab(stats: HomePreviewData.empty)
    }
}

#Preview("Cost — sparse") {
    HomePreviewStage(width: 1_080) {
        CostTab(stats: HomePreviewData.sparse)
    }
}
