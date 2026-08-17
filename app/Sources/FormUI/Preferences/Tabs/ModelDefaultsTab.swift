import FormCore
import FormDesign
import SwiftUI

struct ModelDefaultsTab: View {
    @Environment(\.theme) private var theme
    let controller: PreferencesController

    private var defaultRef: ModelRef { controller.settings.defaults.modelRef }

    private var thinkingLevels: [ThinkingLevel] {
        controller.catalog.thinkingLevels(for: defaultRef)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.xxl) {
            PreferenceSection(
                title: "Default",
                footer: "New sessions start here. A session can override both (F8.4)."
            ) {
                PreferenceRow(title: "Model") {
                    Text(defaultLabel)
                        .typeStyle(theme.typography.uiMedium)
                        .foregroundStyle(theme.color.textPrimary)
                        .frame(maxWidth: .infinity, alignment: .trailing)
                }
                FormDivider()
                PreferenceRow(
                    title: "Reasoning effort",
                    help: thinkingLevels.count > 1
                        ? nil : "This model does not expose a reasoning ladder."
                ) {
                    PreferenceMenu(
                        selection: Binding(
                            get: { defaultRef.thinkingLevel },
                            set: { controller.setThinkingLevel($0) }
                        ),
                        options: thinkingLevels.map { PreferenceOption($0, $0.displayName) }
                    )
                    .disabled(thinkingLevels.count < 2)
                }
            }

            PreferenceSection(title: "All models") {
                FormTextField(
                    text: Binding(
                        get: { controller.modelSearch },
                        set: { controller.modelSearch = $0 }
                    ),
                    placeholder: "Search models…",
                    systemImage: "magnifyingglass",
                    size: .small
                )

                let hits = controller.modelHits
                if hits.isEmpty {
                    EmptyState(
                        systemImage: "magnifyingglass",
                        title: "No matches",
                        message: "Nothing in the catalog matches “\(controller.modelSearch)”.",
                        isCompact: true
                    )
                } else {
                    ModelTableHeader()
                    ForEach(hits) { hit in
                        ModelRowView(
                            hit: hit,
                            isDefault: hit.model.id == defaultRef.modelId
                                && hit.provider.id == defaultRef.providerId,
                            onSetDefault: { controller.setDefaultModel(hit.ref) }
                        )
                    }
                }
            }
        }
        .preferencePane()
    }

    private var defaultLabel: String {
        let name = controller.catalog.displayName(defaultRef)
        let provider = controller.catalog.provider(id: defaultRef.providerId)?.name
            ?? defaultRef.providerId
        return "\(provider) · \(name)"
    }
}

private struct ModelTableHeader: View {
    @Environment(\.theme) private var theme

    var body: some View {
        HStack(spacing: theme.metrics.spacing.lg) {
            Text("Model")
                .frame(maxWidth: .infinity, alignment: .leading)
            Text("Context")
                .frame(width: ModelColumns.numeric, alignment: .trailing)
            Text("Max out")
                .frame(width: ModelColumns.numeric, alignment: .trailing)
            Text("In / Out per 1M")
                .frame(width: ModelColumns.pricing, alignment: .trailing)
        }
        .typeStyle(theme.typography.micro.weighted(.medium))
        .foregroundStyle(theme.color.textTertiary)
        .padding(.horizontal, theme.metrics.spacing.md)
    }
}

private enum ModelColumns {
    static let numeric: CGFloat = 64
    static let pricing: CGFloat = 108
}

private struct ModelRowView: View {
    @Environment(\.theme) private var theme

    let hit: ModelHit
    let isDefault: Bool
    let onSetDefault: () -> Void

    var body: some View {
        ListRow(
            isSelected: isDefault,
            height: theme.metrics.attachmentChipHeight,
            horizontalInset: theme.metrics.spacing.md
        ) { state in
            HStack(spacing: theme.metrics.spacing.lg) {
                VStack(alignment: .leading, spacing: theme.metrics.spacing.xxs) {
                    HStack(spacing: theme.metrics.spacing.sm) {
                        Text(hit.model.name)
                            .typeStyle(theme.typography.uiMedium)
                            .foregroundStyle(theme.color.textPrimary)
                            .lineLimit(1)
                        if hit.model.deprecated {
                            Badge("deprecated", tone: .warning)
                        }
                        if isDefault {
                            Badge("default", tone: .accent, isFilled: true)
                        }
                    }
                    HStack(spacing: theme.metrics.spacing.xs) {
                        Text(hit.provider.name)
                            .typeStyle(theme.typography.micro)
                            .foregroundStyle(theme.color.textTertiary)
                        ForEach(capabilities, id: \.self) { capability in
                            Badge(capability)
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)

                if state.isHovering && !isDefault {
                    FormButton("Set as default", size: .small, action: onSetDefault)
                } else {
                    Text(Self.compact(hit.model.contextWindow))
                        .frame(width: ModelColumns.numeric, alignment: .trailing)
                    Text(Self.compact(hit.model.maxOutput))
                        .frame(width: ModelColumns.numeric, alignment: .trailing)
                    Text(pricing)
                        .frame(width: ModelColumns.pricing, alignment: .trailing)
                }
            }
            .typeStyle(theme.typography.caption)
            .tabularFigures()
            .foregroundStyle(theme.color.textSecondary)
        }
        .accessibilityLabel("\(hit.provider.name) \(hit.model.name)")
    }

    private var capabilities: [String] {
        var flags: [String] = []
        if hit.model.capabilities.vision { flags.append("vision") }
        if hit.model.capabilities.tools { flags.append("tools") }
        if hit.model.capabilities.reasoning { flags.append("reasoning") }
        if hit.model.capabilities.caching { flags.append("caching") }
        return flags
    }

    private var pricing: String {
        "$\(Self.money(hit.model.pricing.input)) / $\(Self.money(hit.model.pricing.output))"
    }

    private static func money(_ value: Double) -> String {
        value >= 10 ? String(Int(value.rounded())) : String(format: "%.2f", value)
    }

    /// `200k`, `1.0M` — the table is dense and the exact count is not the point.
    private static func compact(_ value: Int64) -> String {
        switch value {
        case 1_000_000...: String(format: "%.1fM", Double(value) / 1_000_000)
        case 1_000...: "\(value / 1_000)k"
        default: "\(value)"
        }
    }
}

#Preview("Models") {
    PreferencesTabPreview(tab: .models)
}
