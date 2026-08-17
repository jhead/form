import Charts
import FormCore
import FormDesign
import SwiftUI

/// How long a turn takes, day by day (spec 12 §2, "turn duration distribution").
///
/// The document reports total duration and turn count per day, not a per-turn histogram, so
/// this plots the mean turn duration for each day against the range's overall average. When
/// the core grows a turn-duration histogram this becomes a distribution plot without the
/// card moving — see the workstream report.
struct TurnDurationChart: View {
    @Environment(\.theme) private var theme

    let daily: [DailyBucket]
    let averageMs: Int64

    @State private var selection: Int?

    private var points: [Dated<DailyBucket>] {
        daily.dated().filter { $0.value.turns > 0 }
    }

    var body: some View {
        Chart {
            ForEach(points) { point in
                BarMark(
                    x: .value("Date", point.date, unit: .day),
                    y: .value("Mean turn", mean(point.value))
                )
                .foregroundStyle(ChartSeries.turns.color(theme))
                .cornerRadius(theme.metrics.radius.sm)
            }

            if averageMs > 0 {
                RuleMark(y: .value("Average", Double(averageMs)))
                    .lineStyle(StrokeStyle(lineWidth: theme.metrics.hairline * 2, dash: [4, 3]))
                    .foregroundStyle(theme.color.chartAxis)
                    .annotation(position: .top, alignment: .trailing) {
                        Text("avg \(StatsFormat.duration(ms: averageMs))")
                            .typeStyle(theme.typography.micro)
                            .foregroundStyle(theme.color.textTertiary)
                    }
            }
        }
        .formChartYAxis(theme, .durationMs, desiredCount: 3)
        .formChartDateAxis(theme)
        .chartOverlay { proxy in
            ChartHoverLayer(
                proxy: proxy, dates: points.map(\.date), selection: $selection, readout: readout)
        }
    }

    private func mean(_ bucket: DailyBucket) -> Double {
        bucket.turns > 0 ? Double(bucket.durationMs) / Double(bucket.turns) : 0
    }

    private func readout(_ index: Int) -> ChartReadout {
        let point = points[index]
        return ChartReadout(
            title: StatsFormat.longDate(point.date),
            rows: [
                ChartReadout.Row(
                    label: "Mean turn", value: StatsFormat.duration(ms: mean(point.value)),
                    colorIndex: ChartSeries.turns.rawValue),
                ChartReadout.Row(
                    label: "Turns", value: StatsFormat.grouped(point.value.turns), colorIndex: nil),
                ChartReadout.Row(
                    label: "Total", value: StatsFormat.duration(ms: point.value.durationMs),
                    colorIndex: nil),
            ])
    }
}
