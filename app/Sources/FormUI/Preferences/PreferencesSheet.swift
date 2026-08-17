import FormCore
import FormDesign
import SwiftUI

/// The preferences modal (F9.1): 720 × 520, a leading tab rail, a scrolling detail pane.
///
/// Presentation is the shell's business — W9 owns `⌘,`. This view only needs a way to close
/// itself, and it flushes any debounced edit on the way out so closing the sheet can never
/// lose the last keystroke.
public struct PreferencesSheet: View {
    @Environment(\.theme) private var theme

    @State private var controller: PreferencesController
    @State private var isConfirmingReset = false

    private let onClose: () -> Void

    public init(
        stores: CoreStores,
        themeController: ThemeController,
        tab: PreferencesTab = .general,
        onClose: @escaping () -> Void
    ) {
        _controller = State(
            wrappedValue: PreferencesController(
                stores: stores, themeController: themeController, tab: tab))
        self.onClose = onClose
    }

    public var body: some View {
        SheetContainer(
            title: "Preferences",
            subtitle: "Changes apply immediately",
            onClose: close
        ) {
            HStack(spacing: 0) {
                tabRail
                FormDivider(.vertical)
                detail
            }
        }
        .overlay {
            if isConfirmingReset {
                ResetConfirmOverlay(
                    onCancel: { isConfirmingReset = false },
                    onConfirm: {
                        isConfirmingReset = false
                        Task { await controller.resetToDefaults() }
                    }
                )
            }
        }
        .task {
            await controller.stores.catalog.load()
            controller.syncThemeFromSettings()
        }
        .onDisappear { Task { await controller.flush() } }
    }

    private func close() {
        Task {
            await controller.flush()
            onClose()
        }
    }

    // MARK: Rail

    private var tabRail: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.xxs) {
            ForEach(PreferencesTab.allCases) { tab in
                ListRow(
                    isSelected: controller.tab == tab,
                    height: theme.metrics.navRowHeight,
                    action: { controller.tab = tab }
                ) { _ in
                    HStack(spacing: theme.metrics.spacing.md) {
                        Image(systemName: tab.systemImage)
                            .typeStyle(theme.typography.ui)
                            .frame(width: theme.metrics.iconMedium)
                        Text(tab.title).typeStyle(theme.typography.ui)
                    }
                    .foregroundStyle(
                        controller.tab == tab ? theme.color.textPrimary : theme.color.textSecondary)
                }
                .accessibilityLabel(tab.title)
            }
            Spacer(minLength: 0)
            if controller.hasPendingEdit {
                Text("Saving…")
                    .typeStyle(theme.typography.micro)
                    .foregroundStyle(theme.color.textTertiary)
                    .padding(.leading, theme.metrics.spacing.md)
            }
        }
        .padding(theme.metrics.spacing.md)
        .frame(width: PreferenceMetrics.tabRailWidth)
        .frame(maxHeight: .infinity, alignment: .top)
        .background(theme.color.backgroundSidebar)
    }

    // MARK: Detail

    @ViewBuilder
    private var detail: some View {
        VStack(spacing: 0) {
            if let error = controller.lastError {
                PreferenceNotice(message: error)
                    .padding(.horizontal, theme.metrics.spacing.xl)
                    .padding(.top, theme.metrics.spacing.lg)
            }
            pane
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    @ViewBuilder
    private var pane: some View {
        switch controller.tab {
        case .general: GeneralTab(controller: controller)
        case .providers: ProvidersTab(controller: controller)
        case .models: ModelDefaultsTab(controller: controller)
        case .appearance: AppearanceTab(controller: controller)
        case .editor: EditorTab(controller: controller)
        case .shortcuts: ShortcutsTab(controller: controller)
        case .advanced:
            AdvancedTab(controller: controller, onRequestReset: { isConfirmingReset = true })
        }
    }
}

// MARK: - Presentation

extension View {
    /// Presents the preferences sheet over the receiver. The shell binds this to `⌘,`.
    public func preferencesSheet(
        isPresented: Binding<Bool>,
        stores: CoreStores,
        themeController: ThemeController,
        tab: PreferencesTab = .general
    ) -> some View {
        sheet(isPresented: isPresented) {
            PreferencesSheet(
                stores: stores, themeController: themeController, tab: tab,
                onClose: { isPresented.wrappedValue = false }
            )
        }
    }
}

#Preview("Preferences") {
    PreferencesSheetPreview()
}

private struct PreferencesSheetPreview: View {
    @State private var stores = CoreStores.preview(.populated)
    @State private var themeController = ThemeController()

    var body: some View {
        PreferencesSheet(stores: stores, themeController: themeController, onClose: {})
            .formTheme(themeController)
    }
}
