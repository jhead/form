import FormCore
import FormDesign
import SwiftUI

struct ProvidersTab: View {
    @Environment(\.theme) private var theme
    let controller: PreferencesController

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.xxl) {
            if controller.catalog.providers.isEmpty {
                EmptyState(
                    systemImage: "key",
                    title: "No providers",
                    message: "The catalog has not loaded yet.",
                    isCompact: true
                )
            } else {
                ForEach(controller.catalog.providers) { provider in
                    ProviderCard(controller: controller, provider: provider)
                }
            }
        }
        .preferencePane()
    }
}

private struct ProviderCard: View {
    @Environment(\.theme) private var theme
    let controller: PreferencesController
    let provider: Provider

    /// The typed key, held for exactly as long as it takes to reach the Keychain. It is never
    /// logged, never copied to the pasteboard, and never put in the settings document (F8.5).
    @State private var entry = ""
    @State private var isEditingKey = false
    @State private var isWorking = false

    private var settings: ProviderSettings { controller.providerSettings(provider.id) }
    private var hasKey: Bool { controller.hasAPIKey(for: provider.id) }

    var body: some View {
        PreferenceSection(title: provider.name, footer: envHint) {
            PreferenceRow(
                title: "Enabled",
                help: "\(provider.models.count) model(s) · \(provider.baseUrl)"
            ) {
                PreferenceToggle(
                    isOn: Binding(
                        get: { settings.enabled },
                        set: { on in controller.editProvider(provider.id) { $0.enabled = on } }
                    ))
            }

            FormDivider()

            PreferenceRow(title: "Base URL", help: "Leave empty to use the catalog default.") {
                FormTextField(
                    text: Binding(
                        get: { settings.baseUrlOverride ?? "" },
                        set: { value in
                            controller.editProvider(provider.id) {
                                $0.baseUrlOverride = value.isEmpty ? nil : value
                            }
                        }
                    ),
                    placeholder: provider.baseUrl,
                    size: .small
                )
            }

            if provider.needsApiKey {
                FormDivider()
                keyRow
            }
        }
    }

    private var envHint: String? {
        guard provider.needsApiKey, let variable = provider.envVars.first else { return nil }
        return "Also read from \(variable) when no key is stored."
    }

    // MARK: Key

    @ViewBuilder
    private var keyRow: some View {
        PreferenceRow(
            title: "API key",
            help: "Stored in the macOS Keychain. It never reaches the core or settings.json.",
            controlAlignment: .center
        ) {
            HStack(spacing: theme.metrics.spacing.md) {
                if isEditingKey {
                    // `isSecure` means the field itself never renders the characters; the
                    // value leaves this view only by being handed to `SettingsStore`.
                    FormTextField(
                        text: $entry, placeholder: "Paste key", isSecure: true, size: .small,
                        onSubmit: save)
                    FormButton("Save", kind: .primary, size: .small, action: save)
                        .disabled(entry.isEmpty || isWorking)
                    FormButton("Cancel", kind: .ghost, size: .small) {
                        entry = ""
                        isEditingKey = false
                    }
                } else {
                    // The mask is a fixed-width placeholder, not a redaction of the real key —
                    // its length would leak information the readback is not allowed to give.
                    Badge(
                        hasKey ? "••••••••" : "Not set",
                        systemImage: hasKey ? "checkmark.circle" : nil,
                        tone: hasKey ? .success : .neutral
                    )
                    FormButton(hasKey ? "Replace" : "Set", size: .small) {
                        entry = ""
                        isEditingKey = true
                    }
                    if hasKey {
                        FormButton("Clear", kind: .destructive, size: .small, action: clear)
                            .disabled(isWorking)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .trailing)
        }
    }

    private func save() {
        let key = entry
        guard !key.isEmpty else { return }
        entry = ""
        isEditingKey = false
        isWorking = true
        Task {
            await controller.setAPIKey(key, for: provider.id)
            isWorking = false
        }
    }

    private func clear() {
        entry = ""
        isEditingKey = false
        isWorking = true
        Task {
            await controller.clearAPIKey(for: provider.id)
            isWorking = false
        }
    }
}

#Preview("Providers") {
    PreferencesTabPreview(tab: .providers)
}
