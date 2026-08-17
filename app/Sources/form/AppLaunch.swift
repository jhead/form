import FormCore
import FormDesign
import FormUI
import Observation
import SwiftUI

/// Brings the Rust core up and holds everything the shell needs.
///
/// **This is the only place a `CoreClient` is constructed.** Until it existed the executable
/// referenced nothing in `FormFFI`, so the linker dropped `libform_ffi.a` and the app was
/// hollow — `make verify-xcode` exists to catch exactly that.
///
/// A core that will not start is a visible error state, not a crash: the store lives in the
/// user's Application Support directory and can be missing, unreadable or written by a build
/// with a different ABI, and none of those should take the window down.
@MainActor
@Observable
final class AppLaunch {
    enum Phase {
        case starting
        case ready(CoreStores, AppState)
        case failed(String)
    }

    private(set) var phase: Phase = .starting

    /// Built here because W14 asks the root to own it (spec 14); the menu bar and the global
    /// key handler both read it, and both outlive any single view.
    private(set) var commandCenter: CommandCenter?

    /// Created before the core is, so the splash and the failure screen are themed too.
    let themeController = ThemeController()
    let toasts = ToastCenter()

    func start() async {
        if case .ready = phase { return }
        phase = .starting

        do {
            let config = CoreConfig(
                dataDir: CoreConfig.defaultDataDir(), seedMockData: true)
            let stores = try CoreStores(config: config)
            try await stores.start()

            let appearance = stores.settings.settings.appearance
            themeController.setMode(FormDesign.ThemeMode(appearance.themeMode))
            themeController.setTextScale(appearance.textSizeMultiplier)

            let appState = AppState(sidebarCollapsed: appearance.sidebarCollapsed)
            let center = CommandCenter(
                stores: stores, theme: themeController, state: appState)
            center.hooks.openPreferences = { _ in appState.preferencesPresented = true }
            commandCenter = center

            phase = .ready(stores, appState)
            Log.core.info("core started at \(config.dataDir, privacy: .public)")
        } catch {
            let message = String(describing: error)
            Log.core.error("core failed to start: \(message, privacy: .public)")
            phase = .failed(message)
        }
    }

    func retry() {
        phase = .starting
        Task { await start() }
    }

    func quit() {
        NSApplication.shared.terminate(nil)
    }
}
