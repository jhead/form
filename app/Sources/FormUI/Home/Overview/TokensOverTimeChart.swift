import Charts
import FormCore
import FormDesign
import SwiftUI

/// Tokens per day as a stacked area — input / output / cache read / cache write (F11.3).
///
/// The stack order and the colors are `ChartSeries.tokenSeries`, so input is the same color
/// here, in the cache chart and in every legend on the dashboard.
struct TokensOverTimeChart: View {
    @Environment(\.theme) private var theme

    let daily: [DailyBucket]
    var metrics: HomeMetrics = .standard

    @State private var selection: Int?

    private var points: [Dated<DailyBucket>] { daily.dated() }

    var body: some View {
        Chart {
            ForEach(points) { point in
                ForEach(ChartSeries.tokenSeries) { series in
                    AreaMark(
                        x: .value("Date", point.date),
                        y: .value("Tokens", series.tokens(point.value)),
                        stacking: .standard
                    )
                    .foregroundStyle(by: .value("Series", series.label))
                    .interpolationMethod(.monotone)
                }
            }
        }
        .chartForegroundStyleScale(
            domain: ChartSeries.tokenSeries.map(\.label),
            range: ChartSeries.tokenSeries.map { $0.color(theme).color }
        )
        .chartLegend(.hidden)
        .formChartYAxis(theme, .tokens)
        .formChartDateAxis(theme)
        .chartOverlay { proxy in
            ChartHoverLayer(
                proxy: proxy, dates: points.map(\.date), selection: $selection, readout: readout)
        }
    }

    private func readout(_ index: Int) -> ChartReadout {
        let point = points[index]
        var rows = ChartSeries.tokenSeries.map { series in
            ChartReadout.Row(
                label: series.label,
                value: StatsFormat.grouped(series.tokens(point.value)),
                colorIndex: series.rawValue)
        }
        rows.append(
            ChartReadout.Row(
                label: "Total", value: StatsFormat.grouped(point.value.totalTokens), colorIndex: nil))
        return ChartReadout(title: StatsFormat.longDate(point.date), rows: rows)
    }
}

#Preview("Tokens over time") {
    HomePreviewStage {
        ChartCard(
            title: "Tokens over time",
            legend: ChartSeries.tokenSeries.map { ChartLegendItem($0) },
            height: HomeMetrics.standard.chart
        ) {
            TokensOverTimeChart(daily: HomePreviewData.populated.daily)
        }
    }
}
