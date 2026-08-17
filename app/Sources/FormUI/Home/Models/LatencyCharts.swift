import Charts
import FormCore
import FormDesign
import SwiftUI

/// Which percentile a bar belongs to. Its index is its palette slot, so p50 is the same
/// color in the latency chart and the throughput chart beside it.
enum Percentile: Int, CaseIterable, Identifiable {
    case p50, p90, p99

    var id: Int { rawValue }
    var label: String { ["p50", "p90", "p99"][rawValue] }
}

/// Grouped bars for p50 / p90 / p99, per model (F11.6). One view, two uses: time to first
/// token, and output throughput.
struct PercentileBars: View {
    @Environment(\.theme) private var theme

    struct Entry: Identifiable {
        let model: String
        let percentile: Percentile
        let value: Double

        var id: String { "\(model)-\(percentile.rawValue)" }
    }

    let entries: [Entry]
    let format: AxisFormat

    var body: some View {
        Chart(entries) { entry in
            BarMark(
                x: .value("Model", entry.model),
                y: .value(format == .rate ? "Tokens per second" : "Latency", entry.value)
            )
            .position(by: .value("Percentile", entry.percentile.label))
            .foregroundStyle(by: .value("Percentile", entry.percentile.label))
            .cornerRadius(theme.metrics.radius.sm)
        }
        .chartForegroundStyleScale(
            domain: Percentile.allCases.map(\.label),
            range: Percentile.allCases.map { theme.color.series($0.rawValue).color }
        )
        .chartLegend(.hidden)
        .formChartYAxis(theme, format)
        .formChartCategoryAxis(theme)
    }
}

/// The time-to-first-token distribution, one line per model, from `LatencyStat.histogram`.
///
/// Bins come from the core already bucketed; the chart plots their counts and never
/// re-buckets. Bin edges are read through the Swift mirror's `lower` / `upper` — see the
/// note in the workstream report about the core's `lowerMs` / `upperMs`.
struct LatencyDistribution: View {
    @Environment(\.theme) private var theme

    let latency: [LatencyStat]
    let names: [String: String]

    private struct Point: Identifiable {
        let model: String
        let bin: Double
        let count: Int64

        var id: String { "\(model)-\(bin)" }
    }

    var body: some View {
        Chart(points) { point in
            LineMark(
                x: .value("TTFT", point.bin),
                y: .value("Turns", point.count)
            )
            .foregroundStyle(by: .value("Model", point.model))
            .interpolationMethod(.monotone)
            .lineStyle(StrokeStyle(lineWidth: theme.metrics.hairline * 4, lineJoin: .round))

            // Unstacked: three models' distributions are compared, not summed.
            AreaMark(
                x: .value("TTFT", point.bin),
                y: .value("Turns", point.count),
                stacking: .unstacked
            )
            .foregroundStyle(by: .value("Model", point.model))
            .opacity(0.12)
            .interpolationMethod(.monotone)
        }
        .chartForegroundStyleScale(domain: modelNames, range: modelColors)
        .chartLegend(.hidden)
        .formChartYAxis(theme, .count, desiredCount: 3)
        .formChartValueXAxis(theme, .durationMs, desiredCount: 5)
    }

    private var points: [Point] {
        latency.enumerated().flatMap { index, stat in
            stat.histogram.map { bin in
                Point(model: name(stat, index), bin: (bin.lower + bin.upper) / 2, count: bin.count)
            }
        }
    }

    private var modelNames: [String] {
        latency.enumerated().map { name($1, $0) }
    }

    private var modelColors: [Color] {
        latency.indices.map { theme.color.series($0).color }
    }

    private func name(_ stat: LatencyStat, _ index: Int) -> String {
        names[stat.model.slug] ?? stat.model.modelId.titleCasedIdentifier
    }
}

#Preview("Latency percentiles") {
    HomePreviewStage {
        ChartCard(
            title: "Time to first token",
            legend: Percentile.allCases.map { ChartLegendItem($0.label, colorIndex: $0.rawValue) },
            height: HomeMetrics.standard.chart
        ) {
            PercentileBars(entries: previewEntries, format: .durationMs)
        }
    }
}

#Preview("TTFT distribution") {
    HomePreviewStage(theme: .dark) {
        ChartCard(title: "TTFT distribution", height: HomeMetrics.standard.chart) {
            LatencyDistribution(
                latency: HomePreviewData.populated.latency,
                names: Dictionary(
                    HomePreviewData.populated.models.map { ($0.model.slug, $0.displayName) },
                    uniquingKeysWith: { first, _ in first }))
        }
    }
}

private var previewEntries: [PercentileBars.Entry] {
    HomePreviewData.populated.latency.flatMap { stat in
        [
            PercentileBars.Entry(
                model: stat.model.modelId.titleCasedIdentifier, percentile: .p50,
                value: Double(stat.ttftP50)),
            PercentileBars.Entry(
                model: stat.model.modelId.titleCasedIdentifier, percentile: .p90,
                value: Double(stat.ttftP90)),
            PercentileBars.Entry(
                model: stat.model.modelId.titleCasedIdentifier, percentile: .p99,
                value: Double(stat.ttftP99)),
        ]
    }
}
