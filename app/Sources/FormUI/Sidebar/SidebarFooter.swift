import AppKit
import FormCore
import FormDesign
import SwiftUI

/// The pinned sidebar footer (spec 09 §2, spec 08 §1): a 24 pt monogram avatar, the display
/// name, a `·`, the active provider, and a chevron opening the app menu.
struct SidebarFooter: View {
    @Environment(\.theme) private var theme

    let stores: CoreStores
    let appState: AppState
    let themeController: ThemeController

    var body: some View {
        HStack(spacing: theme.metrics.spacing.md) {
            avatar

            HStack(spacing: theme.metrics.spacing.xs) {
                Text(displayName)
                    .typeStyle(theme.typography.ui)
                    .foregroundStyle(theme.color.textPrimary)
                    .lineLimit(1)
                Text(verbatim: "·")
                    .typeStyle(theme.typography.caption)
                    .foregroundStyle(theme.color.textTertiary)
                Text(providerLabel)
                    .typeStyle(theme.typography.caption)
                    .foregroundStyle(theme.color.textTertiary)
                    .lineLimit(1)
            }

            Spacer(minLength: theme.metrics.spacing.xs)

            SidebarMenuButton(
                systemImage: "chevron.up.chevron.down", accessibilityLabel: "Application menu"
            ) {
                Button("Preferences…") { appState.preferencesPresented = true }
                Menu("Appearance") {
                    ForEach(FormDesign.ThemeMode.allCases) { mode in
                        Button {
                            setThemeMode(mode)
                        } label: {
                            if themeController.mode == mode {
                                Label(mode.label, systemImage: "checkmark")
                            } else {
                                Text(mode.label)
                            }
                        }
                    }
                }
                Divider()
                Button("About form") {
                    NSApp.activate(ignoringOtherApps: true)
                    NSApp.orderFrontStandardAboutPanel(nil)
                }
                Divider()
                Button("Quit form") { NSApp.terminate(nil) }
            }
        }
        .padding(.horizontal, theme.metrics.spacing.lg)
        .frame(height: theme.metrics.navRowHeight + theme.metrics.spacing.md)
        .accessibilityElement(children: .contain)
    }

    private var avatar: some View {
        Circle()
            .fill(theme.color.accentMuted)
            .frame(width: theme.metrics.avatar, height: theme.metrics.avatar)
            .overlay {
                Text(monogram)
                    .typeStyle(theme.typography.micro.weighted(.semibold))
                    .foregroundStyle(theme.color.accent)
            }
            .accessibilityHidden(true)
    }

    private var displayName: String {
        let full = NSFullUserName()
        return full.isEmpty ? NSUserName() : full
    }

    private var monogram: String {
        let initials = displayName
            .split(separator: " ")
            .prefix(2)
            .compactMap { $0.first.map(String.init) }
            .joined()
        return initials.isEmpty ? "f" : initials.uppercased()
    }

    /// The provider behind the session on screen, falling back to the global default.
    private var providerLabel: String {
        let ref =
            stores.sessions.selected?.modelRef
            ?? stores.settings.settings.defaults.modelRef
        return stores.catalog.provider(id: ref.providerId)?.name ?? ref.providerId
    }

    private func setThemeMode(_ mode: FormDesign.ThemeMode) {
        themeController.setMode(mode)
        Task {
            do {
                try await stores.settings.setThemeMode(mode.core)
            } catch {
                Log.ui.error(
                    "setThemeMode failed: \(String(describing: error), privacy: .public)")
            }
        }
    }
}

// MARK: - Theme mode bridging

/// `FormCore.ThemeMode` is an open string (unknown values round-trip); `FormDesign.ThemeMode`
/// is the closed enum the controller resolves. The shell is the only place the two meet.
extension FormDesign.ThemeMode {
    init(_ core: FormCore.ThemeMode) {
        switch core {
        case .light: self = .light
        case .dark: self = .dark
        default: self = .system
        }
    }

    var core: FormCore.ThemeMode {
        switch self {
        case .light: .light
        case .dark: .dark
        case .system: .system
        }
    }
}
