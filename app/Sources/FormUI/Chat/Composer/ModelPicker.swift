import SwiftUI
import FormCore
import FormDesign

/// `Opus 5   High` in the composer bar, opening a searchable model + effort popover (F8.3).
struct ModelPicker: View {
    @Environment(\.theme) private var theme

    let catalog: CatalogStore
    let selection: ModelRef
    let onSelect: (ModelRef) -> Void

    @State private var isShowingPopover = false
    @State private var query = ""

    var body: some View {
        Button { isShowingPopover.toggle() } label: {
            HStack(spacing: theme.metrics.spacing.sm) {
                Text(catalog.displayName(selection))
                    .typeStyle(theme.typography.caption)
                    .foregroundStyle(theme.color.textSecondary)

                if selection.thinkingLevel != .off {
                    Text(selection.thinkingLevel.displayName)
                        .typeStyle(theme.typography.caption)
                        .foregroundStyle(theme.color.textTertiary)
                }

                Image(systemName: "chevron.up.chevron.down")
                    .typeStyle(theme.typography.micro)
                    .foregroundStyle(theme.color.textTertiary)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Model: \(catalog.displayName(selection))")
        .popover(isPresented: $isShowingPopover, arrowEdge: .top) {
            PopoverContainer(width: theme.metrics.paletteWidth / 2) {
                FormTextField(
                    text: $query, placeholder: "Search models…",
                    systemImage: "magnifyingglass", size: .small)

                ScrollView {
                    VStack(alignment: .leading, spacing: theme.metrics.spacing.xxs) {
                        ForEach(catalog.search(query)) { hit in
                            modelRow(hit)
                        }
                    }
                }
                .frame(maxHeight: theme.metrics.sheetHeight / 2)

                FormDivider()

                effortRow
            }
        }
    }

    private func modelRow(_ hit: ModelHit) -> some View {
        let isSelected = hit.model.id == selection.modelId
            && hit.provider.id == selection.providerId
        return Button {
            // Keep the current effort if the model supports it; otherwise take its best.
            let levels = hit.model.thinkingLevels
            let level = levels.contains(selection.thinkingLevel) ? selection.thinkingLevel
                : (levels.contains(.high) ? .high : (levels.first ?? .off))
            onSelect(
                ModelRef(
                    providerId: hit.provider.id, modelId: hit.model.id, thinkingLevel: level))
            isShowingPopover = false
        } label: {
            HStack(spacing: theme.metrics.spacing.md) {
                Text(hit.model.name)
                    .typeStyle(theme.typography.ui)
                    .foregroundStyle(theme.color.textPrimary)
                Text(hit.provider.name)
                    .typeStyle(theme.typography.micro)
                    .foregroundStyle(theme.color.textTertiary)
                Spacer(minLength: theme.metrics.spacing.md)
                Text("\(ChatFormat.compact(hit.model.contextWindow)) ctx")
                    .typeStyle(theme.typography.micro)
                    .tabularFigures()
                    .foregroundStyle(theme.color.textTertiary)
                if isSelected {
                    Image(systemName: "checkmark")
                        .typeStyle(theme.typography.micro)
                        .foregroundStyle(theme.color.accent)
                }
            }
            .padding(.horizontal, theme.metrics.spacing.md)
            .frame(height: theme.metrics.navRowHeight)
            .background(
                RoundedRectangle(cornerRadius: theme.metrics.radius.md, style: .continuous)
                    .fill(isSelected ? theme.color.surfaceSelected : theme.color.surface.opacity(0))
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    private var effortRow: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.sm) {
            Text("Reasoning effort")
                .typeStyle(theme.typography.micro.weighted(.medium))
                .foregroundStyle(theme.color.textTertiary)

            // `pi`'s ladder, filtered to what this model offers (F8.2).
            let levels = catalog.thinkingLevels(for: selection)
            FlowRow(spacing: theme.metrics.spacing.xs) {
                ForEach(ThinkingLevel.ladder.filter(levels.contains), id: \.rawValue) { level in
                    Chip(
                        level.displayName,
                        isSelected: level == selection.thinkingLevel
                    ) {
                        var next = selection
                        next.thinkingLevel = level
                        onSelect(next)
                    }
                }
            }
        }
    }
}

/// A wrapping row. `Layout` rather than a `LazyVGrid` because the chips are all different
/// widths and a grid would column them.
struct FlowRow: Layout {
    var spacing: CGFloat

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout Void) -> CGSize {
        let width = proposal.replacingUnspecifiedDimensions().width
        let rows = layout(subviews, in: width)
        let height = rows.last.map { $0.y + $0.height } ?? 0
        return CGSize(width: width, height: height)
    }

    func placeSubviews(
        in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout Void
    ) {
        for placement in layout(subviews, in: bounds.width) {
            subviews[placement.index].place(
                at: CGPoint(x: bounds.minX + placement.x, y: bounds.minY + placement.y),
                proposal: .unspecified)
        }
    }

    private struct Placement {
        var index: Int
        var x: CGFloat
        var y: CGFloat
        var height: CGFloat
    }

    private func layout(_ subviews: Subviews, in width: CGFloat) -> [Placement] {
        var placements: [Placement] = []
        var x: CGFloat = 0
        var y: CGFloat = 0
        var lineHeight: CGFloat = 0
        for (index, subview) in subviews.enumerated() {
            let size = subview.sizeThatFits(.unspecified)
            if x > 0, x + size.width > width {
                x = 0
                y += lineHeight + spacing
                lineHeight = 0
            }
            placements.append(Placement(index: index, x: x, y: y, height: size.height))
            x += size.width + spacing
            lineHeight = max(lineHeight, size.height)
        }
        return placements
    }
}
