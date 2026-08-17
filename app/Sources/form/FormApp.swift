import FormCore
import FormDesign
import FormUI
import SwiftUI

/// **Owner: W9.** Window, menus, and the single place the core is constructed.
@main
struct FormApp: App {
    @State private var launch = AppLaunch()

    var body: some Scene {
        WindowGroup {
            LaunchGate(launch: launch)
                .formTheme(launch.themeController)
                .task { await launch.start() }
        }
        .windowStyle(.hiddenTitleBar)
        // Spec 09 §1. `FormDesign` names the minimum (`windowMinWidth`/`windowMinHeight`,
        // applied in `RootView`) but has no token for the default size — see the W9 report.
        .defaultSize(width: 1_280, height: 860)
        .commands {
            // W14 owns the shortcut table and generates every menu from it (F12.3); no other
            // file in the app declares a `keyboardShortcut` (spec 14 §1). The center only
            // exists once the core is up, so the menu bar is the stock one until then.
            if let center = launch.commandCenter {
                AppCommandMenus(center: center)
            }
        }
    }
}

/// Routes the window between the three launch phases so a core that fails to open is a
/// screen, not a crash.
private struct LaunchGate: View {
    @Bindable var launch: AppLaunch

    var body: some View {
        switch launch.phase {
        case .starting:
            LaunchProgressView()
        case let .failed(message):
            LaunchFailureView(
                message: message,
                onRetry: { launch.retry() },
                onQuit: { launch.quit() }
            )
        case let .ready(stores, appState):
            // W10's `ChatView` is wired; W12's dashboard is a placeholder until `HomeView`
            // lands — swap `home:` for `{ HomeView(stores: stores) }` then.
            RootView(
                stores: stores,
                appState: appState,
                themeController: launch.themeController,
                toasts: launch.toasts,
                home: { PendingSurface.home() },
                session: { _ in ChatView(stores: stores) }
            )
            .environment(\.commandCenter, launch.commandCenter)
        }
    }
}
