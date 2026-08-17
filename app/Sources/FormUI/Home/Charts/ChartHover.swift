import Charts
import FormDesign
import SwiftUI

/// What the shared hover treatment shows: a title and every series' value at that x
/// (spec 12 §3).
struct ChartReadout {
    struct Row: Identifiable {
        let label: String
        let value: String
        var colorIndex: Int?

        var id: String { label }
    }

    let title: String
    var rows: [Row]
}

/// The popover card the hover rule carries.
struct ChartReadoutCard: View {
    @Environment(\.theme) private var theme

    let readout: ChartReadout
    var metrics: HomeMetrics = .standard

    var body: some View {
        PopoverContainer(title: readout.title, width: metrics.readoutWidth) {
            VStack(alignment: .leading, spacing: theme.metrics.spacing.xs) {
                ForEach(readout.rows) { row in
                    HStack(spacing: theme.metrics.spacing.sm) {
                        if let index = row.colorIndex {
                            RoundedRectangle(cornerRadius: theme.metrics.radius.sm, style: .continuous)
                                .fill(theme.color.series(index))
                                .frame(width: metrics.legendSwatch, height: metrics.legendSwatch)
                        }
                        Text(row.label)
                            .typeStyle(theme.typography.micro)
                            .foregroundStyle(theme.color.textSecondary)
                        Spacer(minLength: theme.metrics.spacing.md)
                        Text(row.value)
                            .typeStyle(theme.typography.micro)
                            .tabularFigures()
                            .foregroundStyle(theme.color.textPrimary)
                    }
                }
            }
        }
        .allowsHitTesting(false)
    }
}

/// A vertical rule plus a readout card, tracking the pointer across a plot.
///
/// Lives in a `.chartOverlay`, so it gets the `ChartProxy` and can turn a pointer position
/// back into a domain value — which is the only way to be sure the rule lands on the mark
/// the reader thinks they are pointing at.
struct ChartHoverLayer<X: Plottable & Hashable>: View {
    @Environment(\.theme) private var theme

    let proxy: ChartProxy
    /// The plotted domain, in order.
    let values: [X]
    /// Maps a domain value onto the number line, so "nearest" is well defined.
    let position: (X) -> Double
    @Binding var selection: Int?
    let readout: (Int) -> ChartReadout

    var metrics: HomeMetrics = .standard

    var body: some View {
        GeometryReader { geometry in
            let plot = proxy.plotFrame.map { geometry[$0] }

            ZStack(alignment: .topLeading) {
                Rectangle()
                    .fill(.clear)
                    .contentShape(Rectangle())
                    .onContinuousHover { phase in
                        switch phase {
                        case let .active(location):
                            selection = index(at: location, plot: plot)
                        case .ended:
                            selection = nil
                        }
                    }

                if let plot, let index = selection, values.indices.contains(index),
                    let x = proxy.position(forX: values[index])
                {
                    let cursor = plot.minX + x

                    Rectangle()
                        .fill(theme.color.chartAxis)
                        .frame(width: theme.metrics.hairline * 2, height: plot.height)
                        .position(x: cursor, y: plot.midY)
                        .allowsHitTesting(false)

                    ChartReadoutCard(readout: readout(index), metrics: metrics)
                        .fixedSize()
                        .offset(
                            x: cardX(cursor: cursor, width: geometry.size.width),
                            y: 0)
                        .allowsHitTesting(false)
                }
            }
        }
    }

    /// Keeps the card inside the card, flipping it to the other side of the rule near the
    /// trailing edge.
    private func cardX(cursor: CGFloat, width: CGFloat) -> CGFloat {
        let gap = theme.metrics.spacing.lg
        let trailing = cursor + gap
        if trailing + metrics.readoutWidth > width {
            return max(0, cursor - gap - metrics.readoutWidth)
        }
        return trailing
    }

    private func index(at location: CGPoint, plot: CGRect?) -> Int? {
        guard let plot, !values.isEmpty else { return nil }
        let x = location.x - plot.minX
        guard x >= 0, x <= plot.width else { return nil }
        guard let hovered: X = proxy.value(atX: x) else { return nil }
        let target = position(hovered)
        return values.indices.min {
            abs(position(values[$0]) - target) < abs(position(values[$1]) - target)
        }
    }
}

extension ChartHoverLayer where X == Date {
    init(
        proxy: ChartProxy,
        dates: [Date],
        selection: Binding<Int?>,
        readout: @escaping (Int) -> ChartReadout
    ) {
        self.init(
            proxy: proxy, values: dates, position: { $0.timeIntervalSince1970 },
            selection: selection, readout: readout)
    }
}
