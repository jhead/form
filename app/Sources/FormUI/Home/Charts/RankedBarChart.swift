import Charts
import FormDesign
import SwiftUI

/// One bar of a ranked chart. `colorIndex` is the rank, so the palette runs in order down
/// the list and the same model keeps its color between the donut and the bars.
struct RankedBarRow: Identifiable {
    let id: String
    let label: String
    let value: Double
    let colorIndex: Int
    var detail: String?
}

/// The dashboard's ranked-bar chart: models by tokens, providers by spend, tools by
/// invocation count. Horizontal bars, value annotated at the trailing end.
struct RankedBarChart: View {
    @Environment(\.theme) private var theme

    let rows: [RankedBarRow]
    var format: AxisFormat = .tokens
    var metrics: HomeMetrics = .standard

    var body: some View {
        Chart(rows) { row in
            BarMark(
                x: .value("Value", row.value),
                y: .value("Label", row.label)
            )
            .foregroundStyle(theme.color.series(row.colorIndex))
            .cornerRadius(theme.metrics.radius.sm)
            .annotation(position: .trailing, alignment: .leading) {
                Text(row.detail ?? format.string(row.value))
                    .typeStyle(theme.typography.micro)
                    .tabularFigures()
                    .foregroundStyle(theme.color.textSecondary)
            }
        }
        .chartXScale(domain: 0 ... headroom)
        .chartYAxis {
            AxisMarks(preset: .aligned, position: .leading) { value in
                AxisValueLabel {
                    Text(value.as(String.self) ?? "")
                        .typeStyle(theme.typography.micro)
                        .foregroundStyle(theme.color.textSecondary)
                        .lineLimit(1)
                }
            }
        }
        .chartXAxis {
            // Ticks stop at the data's maximum: the domain carries extra headroom for the
            // trailing annotation, and a tick out there would be clipped by the plot edge.
            AxisMarks(values: ticks) { value in
                AxisTick(stroke: StrokeStyle(lineWidth: theme.metrics.hairline * 2))
                    .foregroundStyle(theme.color.chartAxis)
                AxisValueLabel {
                    Text(format.string(value.as(Double.self) ?? 0))
                        .typeStyle(theme.typography.micro)
                        .tabularFigures()
                        .foregroundStyle(theme.color.chartAxis)
                }
            }
        }
        .frame(height: CGFloat(max(rows.count, 1)) * metrics.rankRowHeight)
        .accessibilityElement(children: .contain)
    }

    /// Room at the trailing end for the annotation, so the longest bar's value is not
    /// clipped by the plot edge.
    private var headroom: Double {
        maximum > 0 ? maximum * 1.28 : 1
    }

    private var maximum: Double { rows.map(\.value).max() ?? 0 }

    private var ticks: [Double] {
        guard maximum > 0 else { return [0] }
        return [0, maximum / 2, maximum]
    }
}

#Preview("Ranked bars") {
    HomePreviewStage {
        ChartCard(title: "By tokens") {
            RankedBarChart(
                rows: HomePreviewData.populated.models.enumerated().map { index, stat in
                    RankedBarRow(
                        id: stat.id, label: stat.displayName, value: Double(stat.totalTokens),
                        colorIndex: index)
                })
        }
    }
}
