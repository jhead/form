import AppKit
import FormCore
import FormDesign
import SwiftUI

struct AdvancedTab: View {
    @Environment(\.theme) private var theme
    let controller: PreferencesController
    let onRequestReset: () -> Void

    private var settings: FormCore.Settings { controller.settings }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.xxl) {
            PreferenceSection(
                title: "Data",
                footer: "Set by the core at launch; shown here so a support question has an answer."
            ) {
                PreferenceRow(title: "Data directory", controlAlignment: .center) {
                    HStack(spacing: theme.metrics.spacing.md) {
                        Text(displayPath)
                            .typeStyle(theme.typography.micro)
                            .foregroundStyle(theme.color.textSecondary)
                            .lineLimit(1)
                            .truncationMode(.head)
                            .formTooltip(settings.advanced.dataDir)
                        FormButton("Reveal", systemImage: "folder", size: .small) {
                            revealDataDir()
                        }
                        .disabled(settings.advanced.dataDir.isEmpty)
                    }
                    .frame(maxWidth: .infinity, alignment: .trailing)
                }
            }

            PreferenceSection(title: "Diagnostics") {
                PreferenceRow(title: "Log level") {
                    PreferenceMenu(
                        selection: controller.binding(\.advanced.logLevel),
                        options: LogLevel.all.map { PreferenceOption($0, $0.displayName) }
                    )
                }
                // No "Harness speed" row. It only scales the stub harness's timings, and the
                // app always runs the real agent — a control that cannot affect anything is
                // worse than a missing one. The setting stays in the document for tests.
                FormDivider()
                PreferenceRow(title: "Event stream", controlAlignment: .center) {
                    HStack(spacing: theme.metrics.spacing.sm) {
                        Badge(
                            "\(controller.stores.diagnostics.eventsDelivered) delivered",
                            tone: .neutral)
                        if !controller.stores.diagnostics.isHealthy {
                            Badge(
                                "\(controller.stores.diagnostics.droppedEvents) dropped",
                                tone: .danger)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .trailing)
                }
            }

            PreferenceSection(
                title: "Settings file",
                footer: "Export writes the document only — API keys stay in the Keychain."
            ) {
                PreferenceRow(
                    title: "Import or export",
                    help: "An import is validated before it is applied; problems are reported here.",
                    controlAlignment: .center
                ) {
                    HStack(spacing: theme.metrics.spacing.md) {
                        FormButton("Export…", size: .small) {
                            Task { await controller.exportSettings() }
                        }
                        FormButton("Import…", size: .small) {
                            Task { await controller.importSettings() }
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .trailing)
                }

                if let report = controller.transferReport {
                    PreferenceNotice(
                        message: report.summary,
                        tone: report.isFailure ? .danger : .success,
                        details: report.notes
                    )
                }
            }

            PreferenceSection(title: "Reset") {
                PreferenceRow(
                    title: "Reset all settings",
                    help: "Requires typing a confirmation. Keychain entries are not removed.",
                    controlAlignment: .center
                ) {
                    FormButton("Reset…", kind: .destructive, size: .small, action: onRequestReset)
                        .frame(maxWidth: .infinity, alignment: .trailing)
                }
            }
        }
        .preferencePane()
    }

    private var displayPath: String {
        let path = settings.advanced.dataDir
        guard !path.isEmpty else { return "Not reported yet" }
        return (path as NSString).abbreviatingWithTildeInPath
    }

    private func revealDataDir() {
        let path = settings.advanced.dataDir
        guard !path.isEmpty else { return }
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
    }

}

#Preview("Advanced") {
    PreferencesTabPreview(tab: .advanced)
}
