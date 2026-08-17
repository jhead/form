import FormCore
import FormDesign
import SwiftUI

/// Models (spec 12 §2): share donut, ranked bars, the per-model table, latency and
/// throughput percentiles and the TTFT distribution.
struct ModelsTab: View {
    @Environment(\.theme) private var theme

    let stats: UsageStats
    var metrics: HomeMetrics = .standard

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.xl) {
            HStack(alignment: .top, spacing: theme.metrics.spacing.xl) {
                ChartCard(
                    title: "Token share",
                    subtitle: "Share of tokens by model",
                    isEmpty: stats.models.isEmpty,
                    emptyIcon: "chart.pie",
                    emptyTitle: "No model has run yet",
                    emptyMessage: "Send a message and its model appears here."
                ) {
                    ModelShareDonut(
                        models: stats.models, totalTokens: stats.headline.totalTokens,
                        metrics: metrics)
                }

                VStack(alignment: .leading, spacing: theme.metrics.spacing.xl) {
                    ChartCard(
                        title: "By tokens",
                        isEmpty: stats.models.isEmpty,
                        emptyTitle: "No models in this range"
                    ) {
                        RankedBarChart(rows: tokenRows, format: .tokens, metrics: metrics)
                    }

                    ChartCard(
                        title: "By turns",
                        subtitle: "Assistant turns served by each model",
                        isEmpty: stats.models.isEmpty,
                        emptyTitle: "No models in this range"
                    ) {
                        RankedBarChart(rows: turnRows, format: .count, metrics: metrics)
                    }
                }
            }

            ChartCard(
                title: "Per model",
                subtitle: "Click a column to sort",
                isEmpty: stats.models.isEmpty,
                emptyIcon: "tablecells",
                emptyTitle: "Nothing to tabulate yet",
                emptyMessage: "Model rows appear once a run reports usage."
            ) {
                ModelTable(models: stats.models, providerNames: providerNames, metrics: metrics)
            }

            HStack(alignment: .top, spacing: theme.metrics.spacing.xl) {
                percentileCard(
                    title: "Time to first token",
                    subtitle: "p50 / p90 / p99 per model",
                    format: .durationMs,
                    entries: ttftEntries)

                percentileCard(
                    title: "Output throughput",
                    subtitle: "p50 / p90 / p99 per model",
                    format: .rate,
                    entries: throughputEntries)
            }

            ChartCard(
                title: "TTFT distribution",
                subtitle: "Turns per latency bucket",
                legend: distributionLegend,
                height: metrics.chart,
                isEmpty: !hasHistogram,
                emptyIcon: "chart.xyaxis.line",
                emptyTitle: "No latency samples yet",
                emptyMessage: "The distribution needs finished turns to bucket."
            ) {
                LatencyDistribution(latency: stats.latency, names: displayNames)
            }
        }
    }

    // MARK: Cards

    @ViewBuilder
    private func percentileCard(
        title: String, subtitle: String, format: AxisFormat, entries: [PercentileBars.Entry]
    ) -> some View {
        ChartCard(
            title: title,
            subtitle: subtitle,
            legend: Percentile.allCases.map { ChartLegendItem($0.label, colorIndex: $0.rawValue) },
            height: metrics.chart,
            isEmpty: entries.isEmpty || isSparse,
            emptyIcon: "timer",
            emptyTitle: isSparse ? "Not enough data yet" : "No latency samples yet",
            emptyMessage: isSparse
                ? "Percentiles need at least three active days before they mean anything."
                : "Percentiles appear once turns have completed."
        ) {
            PercentileBars(entries: entries, format: format)
        }
    }

    // MARK: Rows

    private var isSparse: Bool { stats.headline.activeDays < 3 }

    private var providerNames: [String: String] {
        Dictionary(
            stats.providers.map { ($0.providerId, $0.displayName) }, uniquingKeysWith: { first, _ in first })
    }

    private var displayNames: [String: String] {
        Dictionary(
            stats.models.map { ($0.model.slug, $0.displayName) }, uniquingKeysWith: { first, _ in first }
        )
    }

    private func name(_ stat: ModelStat) -> String {
        stat.displayName.isEmpty ? stat.model.modelId.titleCasedIdentifier : stat.displayName
    }

    private var tokenRows: [RankedBarRow] {
        stats.models.enumerated().map { index, stat in
            RankedBarRow(
                id: stat.id, label: name(stat), value: Double(stat.totalTokens), colorIndex: index,
                detail: "\(StatsFormat.abbreviated(stat.totalTokens)) · \(StatsFormat.percent(stat.share))")
        }
    }

    private var turnRows: [RankedBarRow] {
        stats.models.enumerated().map { index, stat in
            RankedBarRow(
                id: stat.id, label: name(stat), value: Double(stat.turns), colorIndex: index,
                detail: StatsFormat.grouped(stat.turns))
        }
    }

    private var ttftEntries: [PercentileBars.Entry] {
        stats.latency.flatMap { stat in
            let label = displayNames[stat.model.slug] ?? stat.model.modelId.titleCasedIdentifier
            return [
                PercentileBars.Entry(model: label, percentile: .p50, value: Double(stat.ttftP50)),
                PercentileBars.Entry(model: label, percentile: .p90, value: Double(stat.ttftP90)),
                PercentileBars.Entry(model: label, percentile: .p99, value: Double(stat.ttftP99)),
            ]
        }
    }

    private var throughputEntries: [PercentileBars.Entry] {
        stats.latency.flatMap { stat in
            let label = displayNames[stat.model.slug] ?? stat.model.modelId.titleCasedIdentifier
            return [
                PercentileBars.Entry(model: label, percentile: .p50, value: stat.tpsP50),
                PercentileBars.Entry(model: label, percentile: .p90, value: stat.tpsP90),
                PercentileBars.Entry(model: label, percentile: .p99, value: stat.tpsP99),
            ]
        }
    }

    private var hasHistogram: Bool { stats.latency.contains { !$0.histogram.isEmpty } }

    private var distributionLegend: [ChartLegendItem] {
        stats.latency.enumerated().map { index, stat in
            ChartLegendItem(
                displayNames[stat.model.slug] ?? stat.model.modelId.titleCasedIdentifier,
                colorIndex: index,
                detail: "\(StatsFormat.grouped(stat.samples)) samples")
        }
    }
}

#Preview("Models") {
    HomePreviewStage(width: 1_080) {
        ModelsTab(stats: HomePreviewData.allTime)
    }
}

#Preview("Models — empty") {
    HomePreviewStage(theme: .dark, width: 1_080) {
        ModelsTab(stats: HomePreviewData.empty)
    }
}

#Preview("Models — sparse") {
    HomePreviewStage(width: 1_080) {
        ModelsTab(stats: HomePreviewData.sparse)
    }
}
