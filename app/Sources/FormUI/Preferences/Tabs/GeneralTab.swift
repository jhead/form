import FormCore
import FormDesign
import SwiftUI

struct GeneralTab: View {
    @Environment(\.theme) private var theme
    let controller: PreferencesController

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.xxl) {
            PreferenceSection(title: "Startup") {
                PreferenceRow(title: "Open on launch") {
                    SegmentedToggle(
                        selection: controller.binding(\.startupView),
                        segments: StartupView.allCases.map {
                            .init(value: $0, title: $0.label)
                        },
                        height: theme.metrics.controlHeightMedium
                    )
                }
            }

            PreferenceSection(title: "Sessions") {
                PreferenceRow(
                    title: "Confirm before deleting",
                    help: "Ask before a session or group is removed."
                ) {
                    PreferenceToggle(isOn: controller.binding(\.general.confirmOnDelete))
                }
                FormDivider()
                PreferenceRow(
                    title: "Name sessions automatically",
                    help: "Title a new session from its first message."
                ) {
                    PreferenceToggle(isOn: controller.binding(\.general.autoTitleSessions))
                }
            }

            PreferenceSection(
                title: "Runs",
                footer: "Both apply to new turns; a run already in flight keeps the mode it started with."
            ) {
                PreferenceRow(
                    title: "Sending during a run",
                    help: "Queue the message for the next turn, or interrupt the current one."
                ) {
                    SegmentedToggle(
                        selection: controller.binding(\.queueMode),
                        segments: QueueMode.allCases.map { .init(value: $0, title: $0.label) },
                        height: theme.metrics.controlHeightMedium
                    )
                }
                FormDivider()
                PreferenceRow(
                    title: "Tool execution",
                    help: "Run a turn's tool calls one after another, or together."
                ) {
                    SegmentedToggle(
                        selection: controller.binding(\.toolExecution),
                        segments: ToolExecutionMode.allCases.map {
                            .init(value: $0, title: $0.label)
                        },
                        height: theme.metrics.controlHeightMedium
                    )
                }
            }

            PreferenceSection(title: "Privacy") {
                PreferenceRow(
                    title: "Share anonymous usage data",
                    help: "Off by default. Nothing reads this yet."
                ) {
                    PreferenceToggle(isOn: controller.binding(\.telemetry))
                }
            }
        }
        .preferencePane()
    }
}

#Preview("General") {
    PreferencesTabPreview(tab: .general)
}
