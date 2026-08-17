import FormCore
import FormDesign
import SwiftUI

/// The per-model table (spec 12 §2): model, provider, turns, tokens, share, cost, avg TTFT,
/// avg tok/s, error rate — sortable by column.
struct ModelTable: View {
    @Environment(\.theme) private var theme

    let models: [ModelStat]
    var metrics: HomeMetrics = .standard

    @State private var sort: ModelColumn = .tokens
    @State private var ascending = false

    var body: some View {
        Grid(alignment: .leading, horizontalSpacing: theme.metrics.spacing.xl, verticalSpacing: 0) {
            GridRow {
                ForEach(ModelColumn.allCases) { column in
                    header(column)
                }
            }
            .frame(height: metrics.tableRowHeight)

            FormDivider()

            ForEach(Array(sorted.enumerated()), id: \.element.stat.id) { index, entry in
                GridRow {
                    HStack(spacing: theme.metrics.spacing.md) {
                        RoundedRectangle(cornerRadius: theme.metrics.radius.sm, style: .continuous)
                            .fill(theme.color.series(entry.rank))
                            .frame(width: metrics.legendSwatch, height: metrics.legendSwatch)
                        Text(entry.name)
                            .typeStyle(theme.typography.caption)
                            .foregroundStyle(theme.color.textPrimary)
                            .lineLimit(1)
                    }

                    ForEach(ModelColumn.allCases.dropFirst()) { column in
                        Text(column.value(entry.stat))
                            .typeStyle(theme.typography.caption)
                            .tabularFigures()
                            .foregroundStyle(color(for: column, entry.stat))
                            .lineLimit(1)
                            .frame(maxWidth: .infinity, alignment: column.isNumeric ? .trailing : .leading)
                    }
                }
                .frame(height: metrics.tableRowHeight)
                .background(
                    theme.color.textPrimary.opacity(
                        index.isMultiple(of: 2) ? theme.metrics.zebraOpacity : 0)
                )
            }
        }
        .frame(minWidth: metrics.tableMinWidth, alignment: .leading)
    }

    private func header(_ column: ModelColumn) -> some View {
        Button {
            if sort == column {
                ascending.toggle()
            } else {
                sort = column
                ascending = column == .model || column == .provider
            }
        } label: {
            HStack(spacing: theme.metrics.spacing.xs) {
                if column.isNumeric { Spacer(minLength: 0) }
                Text(column.title)
                    .typeStyle(theme.typography.micro.weighted(.medium))
                    .foregroundStyle(sort == column ? theme.color.textPrimary : theme.color.textTertiary)
                    .lineLimit(1)
                Image(systemName: ascending ? "chevron.up" : "chevron.down")
                    .imageScale(.small)
                    .foregroundStyle(theme.color.textTertiary)
                    .opacity(sort == column ? 1 : 0)
                if !column.isNumeric { Spacer(minLength: 0) }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help("Sort by \(column.title)")
    }

    private func color(for column: ModelColumn, _ stat: ModelStat) -> ThemeColor {
        guard column == .errors, stat.errorRate > 0.05 else { return theme.color.textSecondary }
        return theme.color.danger
    }

    /// Rank — and therefore color — follows the document's own order, so a model keeps the
    /// same swatch as the donut no matter how the table is sorted.
    private var sorted: [RankedModel] {
        let ranked = models.enumerated().map { RankedModel(rank: $0.offset, stat: $0.element) }
        return ranked.sorted { lhs, rhs in
            sort.isBefore(lhs.stat, rhs.stat, ascending: ascending)
        }
    }
}

enum ModelColumn: String, CaseIterable, Identifiable {
    case model, provider, turns, tokens, share, cost, ttft, throughput, errors

    var id: String { rawValue }

    var title: String {
        switch self {
        case .model: "Model"
        case .provider: "Provider"
        case .turns: "Turns"
        case .tokens: "Tokens"
        case .share: "Share"
        case .cost: "Cost"
        case .ttft: "TTFT"
        case .throughput: "tok/s"
        case .errors: "Errors"
        }
    }

    var isNumeric: Bool {
        switch self {
        case .model, .provider: false
        default: true
        }
    }

    func value(_ stat: ModelStat) -> String {
        switch self {
        case .model: stat.displayName.isEmpty ? stat.model.modelId : stat.displayName
        case .provider: stat.model.providerId.titleCasedIdentifier
        case .turns: StatsFormat.grouped(stat.turns)
        case .tokens: StatsFormat.abbreviated(stat.totalTokens)
        case .share: StatsFormat.percent(stat.share)
        case .cost: StatsFormat.currency(stat.cost)
        case .ttft: StatsFormat.duration(ms: stat.avgTtftMs)
        case .throughput: StatsFormat.rate(stat.avgOutputTps)
        case .errors: StatsFormat.percent(stat.errorRate, decimals: 1)
        }
    }

    func isBefore(_ lhs: ModelStat, _ rhs: ModelStat, ascending: Bool) -> Bool {
        switch self {
        case .model, .provider:
            let left = value(lhs).localizedLowercase
            let right = value(rhs).localizedLowercase
            return ascending ? left < right : left > right
        default:
            let left = sortKey(lhs)
            let right = sortKey(rhs)
            return ascending ? left < right : left > right
        }
    }

    private func sortKey(_ stat: ModelStat) -> Double {
        switch self {
        case .model, .provider: 0
        case .turns: Double(stat.turns)
        case .tokens: Double(stat.totalTokens)
        case .share: stat.share
        case .cost: stat.cost
        case .ttft: Double(stat.avgTtftMs)
        case .throughput: stat.avgOutputTps
        case .errors: stat.errorRate
        }
    }
}
