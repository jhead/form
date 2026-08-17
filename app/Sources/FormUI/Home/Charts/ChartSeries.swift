import FormCore
import FormDesign
import SwiftUI

/// Series identity for the whole dashboard (spec 12 §3).
///
/// The raw value **is** the index into `color.chartSeries`, which is what makes "input is
/// always the same color everywhere" true by construction rather than by discipline: a chart
/// asks for `.input`, never for a color.
enum ChartSeries: Int, CaseIterable, Identifiable, Sendable {
    case input = 0
    case output = 1
    case cacheRead = 2
    case cacheWrite = 3
    case sessions = 4
    case messages = 5
    case cost = 6
    case turns = 7

    var id: Int { rawValue }

    var label: String {
        switch self {
        case .input: "Input"
        case .output: "Output"
        case .cacheRead: "Cache read"
        case .cacheWrite: "Cache write"
        case .sessions: "Sessions"
        case .messages: "Messages"
        case .cost: "Cost"
        case .turns: "Turns"
        }
    }

    func color(_ theme: Theme) -> ThemeColor { theme.color.series(rawValue) }

    /// The four token series a stacked area plots, bottom to top.
    static let tokenSeries: [ChartSeries] = [.input, .output, .cacheRead, .cacheWrite]

    func tokens(_ bucket: DailyBucket) -> Int64 {
        switch self {
        case .input: bucket.input
        case .output: bucket.output
        case .cacheRead: bucket.cacheRead
        case .cacheWrite: bucket.cacheWrite
        case .sessions: bucket.sessions
        case .messages: bucket.messages
        case .turns: bucket.turns
        case .cost: Int64(bucket.cost.rounded())
        }
    }
}

/// One legend entry. Ranked charts (models, providers, tools) build these from a rank index
/// so their colors follow the same ordered palette.
struct ChartLegendItem: Identifiable, Sendable {
    let label: String
    let colorIndex: Int
    var detail: String?

    var id: String { "\(colorIndex)-\(label)" }

    init(_ label: String, colorIndex: Int, detail: String? = nil) {
        self.label = label
        self.colorIndex = colorIndex
        self.detail = detail
    }

    init(_ series: ChartSeries, detail: String? = nil) {
        self.init(series.label, colorIndex: series.rawValue, detail: detail)
    }
}

/// Legend below the chart: wrapping, `micro`, a swatch per entry (spec 12 §3).
struct ChartLegend: View {
    @Environment(\.theme) private var theme

    let items: [ChartLegendItem]
    var metrics: HomeMetrics = .standard

    var body: some View {
        WrappingRow(
            horizontalSpacing: theme.metrics.spacing.lg,
            verticalSpacing: theme.metrics.spacing.xs
        ) {
            ForEach(items) { item in
                HStack(spacing: theme.metrics.spacing.sm) {
                    RoundedRectangle(cornerRadius: theme.metrics.radius.sm, style: .continuous)
                        .fill(theme.color.series(item.colorIndex))
                        .frame(width: metrics.legendSwatch, height: metrics.legendSwatch)
                    Text(item.label)
                        .typeStyle(theme.typography.micro)
                        .foregroundStyle(theme.color.textSecondary)
                    if let detail = item.detail {
                        Text(detail)
                            .typeStyle(theme.typography.micro)
                            .tabularFigures()
                            .foregroundStyle(theme.color.textTertiary)
                    }
                }
                .accessibilityElement(children: .combine)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
