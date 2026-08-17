import Charts
import FormCore
import FormDesign
import SwiftUI

/// Sessions and messages per day, as a bar + line combo (spec 12 §2).
///
/// The two series differ by an order of magnitude, so messages are plotted against a second
/// scale: the line is drawn in message units multiplied onto the bar scale, and the trailing
/// axis is labelled with the inverse. That keeps both shapes readable without either series
/// lying about its own magnitude.
struct SessionsMessagesChart: View {
    @Environment(\.theme) private var theme

    let daily: [DailyBucket]
    var metrics: HomeMetrics = .standard

    @State private var selection: Int?

    private var points: [Dated<DailyBucket>] { daily.dated() }

    /// Bars are sessions; the message line is scaled onto their axis.
    private var scale: Double {
        let sessions = Double(points.map(\.value.sessions).max() ?? 0)
        let messages = Double(points.map(\.value.messages).max() ?? 0)
        guard sessions > 0, messages > 0 else { return 1 }
        return sessions / messages
    }

    var body: some View {
        Chart {
            ForEach(points) { point in
                BarMark(
                    x: .value("Date", point.date, unit: .day),
                    y: .value("Sessions", point.value.sessions)
                )
                .foregroundStyle(ChartSeries.sessions.color(theme))
                .cornerRadius(theme.metrics.radius.sm)
            }
            ForEach(points) { point in
                LineMark(
                    x: .value("Date", point.date),
                    y: .value("Messages", Double(point.value.messages) * scale)
                )
                .foregroundStyle(ChartSeries.messages.color(theme))
                .interpolationMethod(.monotone)
                .lineStyle(StrokeStyle(lineWidth: theme.metrics.hairline * 4, lineJoin: .round))
            }
        }
        .chartYAxis {
            AxisMarks(position: .leading, values: .automatic(desiredCount: 4)) { value in
                AxisGridLine(stroke: StrokeStyle(lineWidth: theme.metrics.hairline * 2))
                    .foregroundStyle(theme.color.chartGrid)
                AxisValueLabel {
                    Text(StatsFormat.abbreviated(value.as(Double.self) ?? 0))
                        .typeStyle(theme.typography.micro)
                        .tabularFigures()
                        .foregroundStyle(theme.color.chartAxis)
                }
            }
            AxisMarks(position: .trailing, values: .automatic(desiredCount: 4)) { value in
                AxisValueLabel {
                    Text(StatsFormat.abbreviated((value.as(Double.self) ?? 0) / scale))
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
        let point = points[index]
        return ChartReadout(
            title: StatsFormat.longDate(point.date),
            rows: [
                ChartReadout.Row(
                    label: ChartSeries.sessions.label,
                    value: StatsFormat.grouped(point.value.sessions),
                    colorIndex: ChartSeries.sessions.rawValue),
                ChartReadout.Row(
                    label: ChartSeries.messages.label,
                    value: StatsFormat.grouped(point.value.messages),
                    colorIndex: ChartSeries.messages.rawValue),
                ChartReadout.Row(
                    label: ChartSeries.turns.label,
                    value: StatsFormat.grouped(point.value.turns),
                    colorIndex: nil),
            ])
    }
}

#Preview("Sessions and messages") {
    HomePreviewStage(theme: .dark) {
        ChartCard(
            title: "Sessions and messages",
            legend: [ChartLegendItem(.sessions), ChartLegendItem(.messages)],
            height: HomeMetrics.standard.chart
        ) {
            SessionsMessagesChart(daily: HomePreviewData.populated.daily)
        }
    }
}
