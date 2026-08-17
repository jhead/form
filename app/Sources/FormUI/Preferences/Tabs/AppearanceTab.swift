import FormCore
import FormDesign
import SwiftUI

struct AppearanceTab: View {
    @Environment(\.theme) private var theme
    let controller: PreferencesController

    /// The steps `⌘+` / `⌘-` walk. Snapping the slider to them means the two ways of changing
    /// text size cannot disagree about what "one notch bigger" is.
    private var ladder: [Double] { ThemeController.textScaleLadder.map(Double.init) }

    private var textScale: Double { controller.settings.appearance.textSizeMultiplier }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.xxl) {
            PreferenceSection(title: "Theme") {
                PreferenceRow(title: "Appearance") {
                    SegmentedToggle(
                        selection: Binding(
                            get: { controller.themeMode },
                            set: { controller.setThemeMode($0) }
                        ),
                        segments: FormDesign.ThemeMode.allCases.map {
                            .init(value: $0, title: $0.label, systemImage: $0.systemImage)
                        },
                        height: theme.metrics.controlHeightMedium
                    )
                }
            }

            PreferenceSection(title: "Text") {
                PreferenceRow(
                    title: "Text size",
                    help: "Also bound to ⌘+, ⌘- and ⌘0."
                ) {
                    PreferenceSlider(
                        value: Binding(
                            get: { textScale },
                            set: { controller.setTextScale(CGFloat($0)) }
                        ),
                        range: (ladder.first ?? 0.85) ... (ladder.last ?? 1.4),
                        ladder: ladder,
                        format: { "\(Int(($0 * 100).rounded()))%" }
                    )
                }
                textPreview
                FormDivider()
                PreferenceRow(
                    title: "Density",
                    help: "Compact tightens row heights and padding throughout."
                ) {
                    SegmentedToggle(
                        selection: controller.binding(\.appearance.density),
                        segments: Density.all.map { .init(value: $0, title: $0.displayName) },
                        height: theme.metrics.controlHeightMedium
                    )
                }
            }

            PreferenceSection(title: "Layout") {
                PreferenceRow(title: "Sidebar width") {
                    PreferenceSlider(
                        value: controller.binding(\.appearance.sidebarWidth),
                        range: AppearanceSettings.sidebarWidthRange,
                        format: { "\(Int($0.rounded())) pt" }
                    )
                }
                FormDivider()
                PreferenceRow(
                    title: "Show turn footers",
                    help: "The duration and token line under each completed turn."
                ) {
                    PreferenceToggle(isOn: controller.binding(\.appearance.showTurnFooters))
                }
            }
        }
        .preferencePane()
    }

    /// Live proof that the multiplier took effect — the sample is rendered with the resolved
    /// theme, so it grows as the slider moves rather than after a relaunch (F9.2).
    private var textPreview: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.xs) {
            Text("The quick brown fox jumps over the lazy dog.")
                .typeStyle(theme.typography.body)
                .foregroundStyle(theme.color.textPrimary)
            Text("Secondary line · 5.9k tokens · 3m 31s")
                .typeStyle(theme.typography.micro)
                .tabularFigures()
                .foregroundStyle(theme.color.textTertiary)
        }
        .padding(theme.metrics.spacing.lg)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: theme.metrics.radius.md, style: .continuous)
                .fill(theme.color.surfaceRaised)
        )
    }
}

#Preview("Appearance") {
    PreferencesTabPreview(tab: .appearance)
}
