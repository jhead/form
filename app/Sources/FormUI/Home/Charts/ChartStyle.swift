import Charts
import FormDesign
import SwiftUI

/// How a value prints on an axis and in a tooltip. Sharing one enum is what keeps a
/// `1.2M` tick and the readout that explains it in agreement (spec 12 §3).
enum AxisFormat: Sendable {
    case tokens
    case count
    case currency
    case durationMs
    case percent
    case rate

    func string(_ value: Double) -> String {
        switch self {
        case .tokens: StatsFormat.abbreviated(value)
        case .count: StatsFormat.abbreviated(value)
        case .currency: StatsFormat.currency(value)
        case .durationMs: StatsFormat.duration(ms: value)
        case .percent: StatsFormat.percent(value)
        case .rate: StatsFormat.rate(value)
        }
    }

    /// The un-abbreviated form, for a tooltip behind an abbreviated label.
    func exact(_ value: Double) -> String {
        switch self {
        case .tokens, .count: StatsFormat.grouped(value)
        case .currency: StatsFormat.currencyExact(value)
        case .durationMs: StatsFormat.duration(ms: value)
        case .percent: StatsFormat.percent(value, decimals: 1)
        case .rate: StatsFormat.rate(value)
        }
    }
}

/// Grid, axes and plot chrome, applied the same way by every chart on the dashboard:
/// horizontal grid lines only, at `chartGrid`; `micro` labels at `chartAxis`.
extension View {
    func formChartYAxis(_ theme: Theme, _ format: AxisFormat, desiredCount: Int = 4) -> some View {
        chartYAxis {
            AxisMarks(position: .leading, values: .automatic(desiredCount: desiredCount)) { value in
                AxisGridLine(stroke: StrokeStyle(lineWidth: theme.metrics.hairline * 2))
                    .foregroundStyle(theme.color.chartGrid)
                AxisValueLabel {
                    Text(format.string(value.as(Double.self) ?? 0))
                        .typeStyle(theme.typography.micro)
                        .tabularFigures()
                        .foregroundStyle(theme.color.chartAxis)
                }
            }
        }
    }

    /// Vertical grid lines are deliberately absent — a time axis reads better with ticks.
    func formChartDateAxis(_ theme: Theme, desiredCount: Int = 5) -> some View {
        chartXAxis {
            AxisMarks(preset: .aligned, values: .automatic(desiredCount: desiredCount)) { value in
                AxisTick(stroke: StrokeStyle(lineWidth: theme.metrics.hairline * 2))
                    .foregroundStyle(theme.color.chartAxis)
                AxisValueLabel {
                    Text(value.as(Date.self).map(StatsFormat.shortDate) ?? "")
                        .typeStyle(theme.typography.micro)
                        .foregroundStyle(theme.color.chartAxis)
                }
            }
        }
    }

    func formChartCategoryAxis(_ theme: Theme) -> some View {
        chartXAxis {
            AxisMarks(preset: .aligned, position: .bottom) { value in
                AxisValueLabel {
                    Text(value.as(String.self) ?? "")
                        .typeStyle(theme.typography.micro)
                        .foregroundStyle(theme.color.chartAxis)
                }
            }
        }
    }

    func formChartValueXAxis(
        _ theme: Theme, _ format: AxisFormat, desiredCount: Int = 5
    ) -> some View {
        chartXAxis {
            AxisMarks(values: .automatic(desiredCount: desiredCount)) { value in
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
    }

    /// Hours 0…23 with a label every three hours, which is as dense as 11 pt survives.
    func formChartHourAxis(_ theme: Theme) -> some View {
        chartXAxis {
            AxisMarks(preset: .aligned, values: Array(stride(from: 0, to: 24, by: 3))) { value in
                AxisValueLabel {
                    Text(value.as(Int.self).map(StatsFormat.hourShort) ?? "")
                        .typeStyle(theme.typography.micro)
                        .tabularFigures()
                        .foregroundStyle(theme.color.chartAxis)
                }
            }
        }
    }
}

/// The period token every card animates against. Changing period changes this string, and
/// `ChartCard` interpolates its contents over `motion.emphasized` (spec 12 §3).
private struct StatsTokenKey: EnvironmentKey {
    static let defaultValue = ""
}

extension EnvironmentValues {
    var statsToken: String {
        get { self[StatsTokenKey.self] }
        set { self[StatsTokenKey.self] = newValue }
    }
}
