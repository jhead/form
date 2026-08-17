import Charts
import FormCore
import FormDesign
import SwiftUI

/// Spend per day with a cumulative overlay (F11.7).
///
/// Daily spend and the running total differ by an order of magnitude, so the cumulative line
/// is drawn against a second scale — bars keep their shape, and the trailing axis is
/// labelled in real dollars.
struct SpendOverTimeChart: View {
    @Environment(\.theme) private var theme

    let byDay: [CostPoint]

    @State private var selection: Int?

    private var points: [Dated<CostPoint>] { byDay.dated() }
    private var cumulative: [Double] { points.runningTotal { $0.value.cost } }

    private var scale: Double {
        let daily = points.map(\.value.cost).max() ?? 0
        let total = cumulative.last ?? 0
        guard daily > 0, total > 0 else { return 1 }
        return daily / total
    }

    var body: some View {
        Chart {
            ForEach(Array(points.enumerated()), id: \.element.id) { index, point in
                AreaMark(
                    x: .value("Date", point.date),
                    y: .value("Spend", point.value.cost)
                )
                .foregroundStyle(ChartSeries.cost.color(theme).opacity(0.35))
                .interpolationMethod(.monotone)

                LineMark(
                    x: .value("Date", point.date),
                    y: .value("Spend", point.value.cost)
                )
                .foregroundStyle(ChartSeries.cost.color(theme))
                .interpolationMethod(.monotone)
                .lineStyle(StrokeStyle(lineWidth: theme.metrics.hairline * 4, lineJoin: .round))

                LineMark(
                    x: .value("Date", point.date),
                    y: .value("Cumulative", cumulative[index] * scale),
                    series: .value("Series", "cumulative")
                )
                .foregroundStyle(ChartSeries.output.color(theme))
                .lineStyle(
                    StrokeStyle(lineWidth: theme.metrics.hairline * 3, dash: [4, 3]))
            }
        }
        .chartYAxis {
            AxisMarks(position: .leading, values: .automatic(desiredCount: 4)) { value in
                AxisGridLine(stroke: StrokeStyle(lineWidth: theme.metrics.hairline * 2))
                    .foregroundStyle(theme.color.chartGrid)
                AxisValueLabel {
                    Text(StatsFormat.currency(value.as(Double.self) ?? 0))
                        .typeStyle(theme.typography.micro)
                        .tabularFigures()
                        .foregroundStyle(theme.color.chartAxis)
                }
            }
            AxisMarks(position: .trailing, values: .automatic(desiredCount: 4)) { value in
                AxisValueLabel {
                    Text(StatsFormat.currency((value.as(Double.self) ?? 0) / scale))
                        .typeStyle(theme.typography.micro)
                        .tabularFigures()
                        .foregroundStyle(theme.color.chartAxis)
                }
            }
        }
        .formChartDateAxis(theme)
        .chartOverlay { proxy in
            ChartHoverLayer(
                proxy: proxy, dates: points.map(\.date), selection: $selection, readout: readout)
        }
    }

    private func readout(_ index: Int) -> ChartReadout {
        ChartReadout(
            title: StatsFormat.longDate(points[index].date),
            rows: [
                ChartReadout.Row(
                    label: "Spend", value: StatsFormat.currencyExact(points[index].value.cost),
                    colorIndex: ChartSeries.cost.rawValue),
                ChartReadout.Row(
                    label: "Cumulative", value: StatsFormat.currencyExact(cumulative[index]),
                    colorIndex: ChartSeries.output.rawValue),
            ])
    }
}

/// Cache read vs write over time (F11.10). Read is the win; write is what buys it.
struct CacheOverTimeChart: View {
    @Environment(\.theme) private var theme

    let daily: [CachePoint]

    @State private var selection: Int?

    private var points: [Dated<CachePoint>] { daily.dated() }

    var body: some View {
        Chart {
            ForEach(points) { point in
                AreaMark(
                    x: .value("Date", point.date),
                    y: .value("Tokens", point.value.read),
                    stacking: .standard
                )
                .foregroundStyle(by: .value("Series", ChartSeries.cacheRead.label))
                .interpolationMethod(.monotone)

                AreaMark(
                    x: .value("Date", point.date),
                    y: .value("Tokens", point.value.write),
                    stacking: .standard
                )
                .foregroundStyle(by: .value("Series", ChartSeries.cacheWrite.label))
                .interpolationMethod(.monotone)
            }
        }
        .chartForegroundStyleScale(
            domain: [ChartSeries.cacheRead.label, ChartSeries.cacheWrite.label],
            range: [
                ChartSeries.cacheRead.color(theme).color, ChartSeries.cacheWrite.color(theme).color,
            ]
        )
        .chartLegend(.hidden)
        .formChartYAxis(theme, .tokens, desiredCount: 3)
        .formChartDateAxis(theme)
        .chartOverlay { proxy in
            ChartHoverLayer(
                proxy: proxy, dates: points.map(\.date), selection: $selection, readout: readout)
        }
    }

    private func readout(_ index: Int) -> ChartReadout {
        let point = points[index].value
        let total = point.read + point.write
        return ChartReadout(
            title: StatsFormat.longDate(points[index].date),
            rows: [
                ChartReadout.Row(
                    label: ChartSeries.cacheRead.label, value: StatsFormat.grouped(point.read),
                    colorIndex: ChartSeries.cacheRead.rawValue),
                ChartReadout.Row(
                    label: ChartSeries.cacheWrite.label, value: StatsFormat.grouped(point.write),
                    colorIndex: ChartSeries.cacheWrite.rawValue),
                ChartReadout.Row(
                    label: "Hit ratio",
                    value: total > 0 ? StatsFormat.percent(Double(point.read) / Double(total)) : "—",
                    colorIndex: nil),
            ])
    }
}

#Preview("Spend over time") {
    HomePreviewStage {
        ChartCard(
            title: "Spend over time",
            legend: [
                ChartLegendItem("Daily", colorIndex: ChartSeries.cost.rawValue),
                ChartLegendItem("Cumulative", colorIndex: ChartSeries.output.rawValue),
            ],
            height: HomeMetrics.standard.chart
        ) {
            SpendOverTimeChart(byDay: HomePreviewData.populated.cost.byDay)
        }
    }
}

#Preview("Cache effectiveness") {
    HomePreviewStage(theme: .dark) {
        ChartCard(
            title: "Cache effectiveness",
            legend: [ChartLegendItem(.cacheRead), ChartLegendItem(.cacheWrite)],
            height: HomeMetrics.standard.chart
        ) {
            CacheOverTimeChart(daily: HomePreviewData.populated.cache.daily)
        }
    }
}
