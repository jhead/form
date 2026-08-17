import Charts
import FormCore
import FormDesign
import SwiftUI

/// Tokens by hour of day (F11.4). Always 24 bars — the core guarantees the array's length,
/// so a quiet hour is a zero bar rather than a hole.
struct HourHistogram: View {
    @Environment(\.theme) private var theme

    let hourly: [HourlyBucket]
    let peakHour: Int

    @State private var selection: Int?

    var body: some View {
        Chart(hourly) { bucket in
            BarMark(
                x: .value("Hour", bucket.hour),
                y: .value("Tokens", bucket.totalTokens)
            )
            .foregroundStyle(
                bucket.hour == peakHour
                    ? theme.color.accent
                    : ChartSeries.input.color(theme)
            )
            .cornerRadius(theme.metrics.radius.sm)
        }
        .chartXScale(domain: -1 ... 24)
        .formChartYAxis(theme, .tokens, desiredCount: 3)
        .formChartHourAxis(theme)
        .chartOverlay { proxy in
            ChartHoverLayer(
                proxy: proxy,
                values: hourly.map(\.hour),
                position: { Double($0) },
                selection: $selection,
                readout: readout)
        }
    }

    private func readout(_ index: Int) -> ChartReadout {
        let bucket = hourly[index]
        return ChartReadout(
            title: StatsFormat.hour(bucket.hour),
            rows: [
                ChartReadout.Row(
                    label: "Tokens", value: StatsFormat.grouped(bucket.totalTokens),
                    colorIndex: ChartSeries.input.rawValue),
                ChartReadout.Row(
                    label: "Turns", value: StatsFormat.grouped(bucket.turns), colorIndex: nil),
            ])
    }
}

/// The weekday × hour matrix (F11.4) as a heat grid: rows Monday-first, columns local hours,
/// intensity relative to the busiest cell in the range.
struct WeekdayHourMatrix: View {
    @Environment(\.theme) private var theme

    let weekdayHour: [[Int64]]
    var metrics: HomeMetrics = .standard

    @State private var hovered: MatrixCell?

    struct MatrixCell: Equatable {
        let weekday: Int
        let hour: Int
        let tokens: Int64
    }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.md) {
            VStack(alignment: .leading, spacing: metrics.heatmapGap) {
                ForEach(Array(weekdayHour.enumerated()), id: \.offset) { weekday, hours in
                    HStack(spacing: metrics.heatmapGap) {
                        Text(StatsFormat.weekdayNames[min(weekday, 6)])
                            .typeStyle(theme.typography.micro)
                            .foregroundStyle(theme.color.textTertiary)
                            .frame(width: metrics.rankLabelWidth / 3, alignment: .leading)

                        ForEach(Array(hours.enumerated()), id: \.offset) { hour, tokens in
                            cell(weekday: weekday, hour: hour, tokens: tokens)
                        }
                    }
                }

                HStack(spacing: metrics.heatmapGap) {
                    Spacer().frame(width: metrics.rankLabelWidth / 3)
                    ForEach(0 ..< 24, id: \.self) { hour in
                        Text(hour.isMultiple(of: 6) ? StatsFormat.hourShort(hour) : "")
                            .typeStyle(theme.typography.micro)
                            .tabularFigures()
                            .foregroundStyle(theme.color.textTertiary)
                            .frame(width: metrics.matrixCell, alignment: .leading)
                            .fixedSize()
                    }
                }
            }

            Text(hoverLabel)
                .typeStyle(theme.typography.micro)
                .foregroundStyle(theme.color.textSecondary)
                .lineLimit(1)
        }
    }

    private func cell(weekday: Int, hour: Int, tokens: Int64) -> some View {
        let shape = RoundedRectangle(cornerRadius: theme.metrics.radius.sm / 2, style: .continuous)
        let cell = MatrixCell(weekday: weekday, hour: hour, tokens: tokens)
        return shape
            .fill(theme.color.heatmap(intensity(tokens)))
            .frame(width: metrics.matrixCell, height: metrics.matrixCell)
            .overlay {
                if hovered == cell {
                    shape.strokeBorder(theme.color.borderStrong, lineWidth: theme.metrics.hairline * 2)
                }
            }
            .onHover { hovered = $0 ? cell : (hovered == cell ? nil : hovered) }
            .help(label(for: cell))
            .accessibilityLabel(label(for: cell))
    }

    private var maximum: Double {
        Double(weekdayHour.flatMap { $0 }.max() ?? 0)
    }

    /// Zero is the empty stop; everything else is spread across the remaining four so a
    /// quiet week still shows shape.
    private func intensity(_ tokens: Int64) -> Double {
        guard tokens > 0, maximum > 0 else { return 0 }
        return 0.25 + 0.75 * (Double(tokens) / maximum)
    }

    private var hoverLabel: String {
        guard let hovered else { return "Tokens by weekday and hour" }
        return label(for: hovered)
    }

    private func label(for cell: MatrixCell) -> String {
        let weekday = StatsFormat.weekdayNames[min(cell.weekday, 6)]
        let hour = StatsFormat.hour(cell.hour)
        guard cell.tokens > 0 else { return "\(weekday) \(hour) · no activity" }
        return "\(weekday) \(hour) · \(StatsFormat.grouped(cell.tokens)) tokens"
    }
}

#Preview("Hour of day") {
    HomePreviewStage {
        ChartCard(title: "Hour of day", height: HomeMetrics.standard.chartCompact) {
            HourHistogram(
                hourly: HomePreviewData.populated.hourly,
                peakHour: HomePreviewData.populated.headline.peakHour)
        }
    }
}

#Preview("Weekday × hour") {
    HomePreviewStage(theme: .dark) {
        ChartCard(title: "Weekday × hour") {
            WeekdayHourMatrix(weekdayHour: HomePreviewData.populated.weekdayHour)
        }
    }
}
