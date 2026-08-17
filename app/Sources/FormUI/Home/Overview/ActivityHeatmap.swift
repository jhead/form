import FormCore
import FormDesign
import SwiftUI

/// The GitHub-style contribution calendar (F11.2): a day × week grid, 11 pt cells, 3 pt
/// gaps, five stops from `heatmapScale`, month labels along the top and weekday labels at
/// the leading edge.
///
/// Intensity comes from `HeatmapCell.level`, which the core computes as quintiles of the
/// non-zero distribution — so a light week still shows contrast (spec 03 §3). Swift picks
/// the stop; it never decides what is intense.
struct ActivityHeatmap: View {
    @Environment(\.theme) private var theme

    let cells: [HeatmapCell]
    var metrics: HomeMetrics = .standard

    @State private var hovered: HeatmapCell?

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.md) {
            HStack(alignment: .top, spacing: metrics.heatmapGap) {
                weekdayLabels
                // As many weeks as the card can hold, most recent last. A scroller would
                // hide history behind a gesture; dropping the oldest weeks keeps the
                // calendar readable at any window width and at any history length.
                GeometryReader { geometry in
                    let visible = columns.suffix(capacity(for: geometry.size.width))
                    VStack(alignment: .leading, spacing: metrics.heatmapGap) {
                        monthLabels(Array(visible))
                        grid(Array(visible))
                    }
                }
                .frame(height: gridHeight)
            }

            HStack(spacing: theme.metrics.spacing.md) {
                Text(hoverLabel)
                    .typeStyle(theme.typography.micro)
                    .foregroundStyle(theme.color.textSecondary)
                    .lineLimit(1)
                Spacer(minLength: theme.metrics.spacing.md)
                scaleKey
            }
        }
    }

    // MARK: Grid

    private var columns: [HeatmapColumn] { HeatmapColumn.build(from: cells) }

    /// One month row, seven day rows, and the gaps between them.
    private var gridHeight: CGFloat {
        metrics.heatmapCell * 8 + metrics.heatmapGap * 7
    }

    private func capacity(for width: CGFloat) -> Int {
        let pitch = metrics.heatmapCell + metrics.heatmapGap
        guard pitch > 0 else { return columns.count }
        return max(1, Int((width + metrics.heatmapGap) / pitch))
    }

    private func grid(_ columns: [HeatmapColumn]) -> some View {
        HStack(alignment: .top, spacing: metrics.heatmapGap) {
            ForEach(columns) { column in
                VStack(spacing: metrics.heatmapGap) {
                    ForEach(0 ..< 7, id: \.self) { weekday in
                        cellView(column.days[weekday])
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func cellView(_ cell: HeatmapCell?) -> some View {
        let shape = RoundedRectangle(cornerRadius: theme.metrics.radius.sm / 2, style: .continuous)
        if let cell {
            shape
                .fill(theme.color.heatmapScale[min(max(cell.level, 0), theme.color.heatmapScale.count - 1)])
                .frame(width: metrics.heatmapCell, height: metrics.heatmapCell)
                .overlay {
                    if hovered?.date == cell.date {
                        shape.strokeBorder(theme.color.borderStrong, lineWidth: theme.metrics.hairline * 2)
                    }
                }
                .onHover { hovered = $0 ? cell : (hovered?.date == cell.date ? nil : hovered) }
                .help(detail(for: cell))
                .accessibilityLabel(detail(for: cell))
        } else {
            shape
                .fill(theme.color.surfaceRaised.opacity(0.4))
                .frame(width: metrics.heatmapCell, height: metrics.heatmapCell)
                .accessibilityHidden(true)
        }
    }

    // MARK: Chrome

    private var weekdayLabels: some View {
        VStack(spacing: metrics.heatmapGap) {
            // The month row above the grid, matched so the labels line up with their rows.
            Text(" ")
                .typeStyle(theme.typography.micro)
                .frame(height: metrics.heatmapCell)
            ForEach(0 ..< 7, id: \.self) { weekday in
                Text(weekday % 2 == 0 ? StatsFormat.weekdayNames[weekday] : "")
                    .typeStyle(theme.typography.micro)
                    .foregroundStyle(theme.color.textTertiary)
                    .frame(height: metrics.heatmapCell, alignment: .trailing)
            }
        }
        .frame(alignment: .trailing)
    }

    private func monthLabels(_ columns: [HeatmapColumn]) -> some View {
        HStack(alignment: .bottom, spacing: metrics.heatmapGap) {
            ForEach(columns) { column in
                Text(column.monthLabel ?? "")
                    .typeStyle(theme.typography.micro)
                    .foregroundStyle(theme.color.textTertiary)
                    .fixedSize()
                    .frame(width: metrics.heatmapCell, height: metrics.heatmapCell, alignment: .leading)
            }
        }
    }

    private var scaleKey: some View {
        HStack(spacing: metrics.heatmapGap) {
            Text("Less")
                .typeStyle(theme.typography.micro)
                .foregroundStyle(theme.color.textTertiary)
            ForEach(Array(theme.color.heatmapScale.enumerated()), id: \.offset) { _, stop in
                RoundedRectangle(cornerRadius: theme.metrics.radius.sm / 2, style: .continuous)
                    .fill(stop)
                    .frame(width: metrics.heatmapCell, height: metrics.heatmapCell)
            }
            Text("More")
                .typeStyle(theme.typography.micro)
                .foregroundStyle(theme.color.textTertiary)
        }
        .accessibilityHidden(true)
    }

    private var hoverLabel: String {
        guard let hovered else {
            guard let first = cells.first, let last = cells.last,
                let start = StatsFormat.date(first.date), let end = StatsFormat.date(last.date)
            else { return "" }
            return "\(StatsFormat.shortDate(start)) – \(StatsFormat.shortDate(end))"
        }
        return detail(for: hovered)
    }

    private func detail(for cell: HeatmapCell) -> String {
        let date = StatsFormat.date(cell.date).map(StatsFormat.longDate) ?? cell.date
        guard cell.tokens > 0 else { return "\(date) · no activity" }
        let sessions = cell.sessions == 1 ? "1 session" : "\(cell.sessions) sessions"
        return "\(date) · \(StatsFormat.grouped(cell.tokens)) tokens · \(sessions)"
    }
}

/// One week of the calendar, Monday first, padded at both ends so a partial week keeps its
/// weekday alignment.
struct HeatmapColumn: Identifiable {
    let id: Int
    var days: [HeatmapCell?]
    var monthLabel: String?

    static func build(from cells: [HeatmapCell]) -> [HeatmapColumn] {
        var columns: [HeatmapColumn] = []
        var current = HeatmapColumn(id: 0, days: Array(repeating: nil, count: 7))
        var seenMonths = Set<String>()

        for cell in cells {
            guard let date = StatsFormat.date(cell.date) else { continue }
            let weekday = mondayFirstWeekday(date)

            // A new column starts on Monday; the first one may begin mid-week, which is why
            // the days array is padded rather than appended to.
            if weekday == 0, current.days.contains(where: { $0 != nil }) {
                columns.append(current)
                current = HeatmapColumn(id: columns.count, days: Array(repeating: nil, count: 7))
            }

            current.days[weekday] = cell

            let month = StatsFormat.monthName(date)
            if current.monthLabel == nil, !seenMonths.contains(month) {
                seenMonths.insert(month)
                current.monthLabel = month
            }
        }

        if current.days.contains(where: { $0 != nil }) { columns.append(current) }
        return columns
    }

    /// `Calendar` numbers Sunday as 1; the calendar reads better Monday-first, and that is
    /// also the order `weekdayHour` uses (spec 03 §2).
    private static func mondayFirstWeekday(_ date: Date) -> Int {
        (Calendar.current.component(.weekday, from: date) + 5) % 7
    }
}

#Preview("Activity heatmap") {
    HomePreviewStage {
        FormCard(title: "Activity", subtitle: "Tokens per day") {
            ActivityHeatmap(cells: HomePreviewData.allTime.heatmap)
        }
    }
}

#Preview("Activity heatmap — dark") {
    HomePreviewStage(theme: .dark) {
        FormCard(title: "Activity", subtitle: "Tokens per day") {
            ActivityHeatmap(cells: HomePreviewData.allTime.heatmap)
        }
    }
}
