import FormCore
import FormDesign
import SwiftUI

/// The app shell. **Owner: W9** — see `docs/specs/09-app-shell-sidebar.md`.
///
/// Generic over the two content surfaces other workstreams own; see `ShellContent.swift` for
/// why, and `RootView(stores:appState:themeController:toasts:)` for the placeholder form the
/// app uses until they land.
public struct RootView<HomeContent: View, SessionContent: View>: View {
    @Environment(\.theme) private var theme

    private let stores: CoreStores
    private let appState: AppState
    private let themeController: ThemeController
    private let toasts: ToastCenter
    private let home: () -> HomeContent
    private let session: (String) -> SessionContent

    @State private var columnVisibility: NavigationSplitViewVisibility = .all
    @State private var ungroupedCollapsed = ShellRestoration.ungroupedCollapsed
    @State private var didRestore = false
    @State private var workspace: WorkspaceRootController

    public init(
        stores: CoreStores,
        appState: AppState,
        themeController: ThemeController,
        toasts: ToastCenter,
        @ViewBuilder home: @escaping () -> HomeContent,
        @ViewBuilder session: @escaping (String) -> SessionContent
    ) {
        self.stores = stores
        self.appState = appState
        self.themeController = themeController
        self.toasts = toasts
        self.home = home
        self.session = session
        _workspace = State(initialValue: WorkspaceRootController(stores: stores))
    }

    public var body: some View {
        NavigationSplitView(columnVisibility: $columnVisibility) {
            SidebarView(
                stores: stores,
                appState: appState,
                themeController: themeController,
                ungroupedCollapsed: $ungroupedCollapsed
            )
            .measuringSidebarWidth()
            .navigationSplitViewColumnWidth(
                min: theme.metrics.sidebarMinWidth,
                ideal: appState.sidebarWidth,
                max: theme.metrics.sidebarMaxWidth
            )
        } detail: {
            ContentShell(
                stores: stores, appState: appState, workspace: workspace,
                home: home, session: session)
        }
        .navigationSplitViewStyle(.balanced)
        // `maxHeight` is not decoration: `NavigationSplitView` sizes itself to the taller
        // column's ideal height, so with only a minimum the sidebar's full row list makes the
        // whole split view taller than the window and it overflows off both edges.
        .frame(
            minWidth: theme.metrics.windowMinWidth,
            maxWidth: .infinity,
            minHeight: theme.metrics.windowMinHeight,
            maxHeight: .infinity)
        .background(
            WindowConfigurator(
                autosaveName: "form.main",
                minSize: CGSize(
                    width: theme.metrics.windowMinWidth,
                    height: theme.metrics.windowMinHeight)
            )
        )
        .toastOverlay(toasts)
        // `⌘,` and the footer menu both raise this flag; W13 owns what it presents.
        .preferencesSheet(
            isPresented: Binding(
                get: { appState.preferencesPresented },
                set: { appState.preferencesPresented = $0 }),
            stores: stores,
            themeController: themeController
        )
        .environment(appState)
        .environment(stores)
        .onPreferenceChange(SidebarWidthKey.self) { width in
            // `onPreferenceChange` is `@Sendable`; hop before touching main-actor state.
            Task { @MainActor in
                guard width >= theme.metrics.sidebarMinWidth else { return }
                appState.sidebarWidth = width
            }
        }
        .task { restore() }
        .onChange(of: appState.route) { _, route in
            ShellRestoration.route = route
            Task { await stores.select(route.sessionId) }
        }
        .onChange(of: ungroupedCollapsed) { _, value in
            ShellRestoration.ungroupedCollapsed = value
        }
        .onChange(of: appState.sidebarCollapsed) { _, collapsed in
            let wanted: NavigationSplitViewVisibility = collapsed ? .detailOnly : .all
            if columnVisibility != wanted { columnVisibility = wanted }
            // Not before `restore()`: the split view reports a visibility of its own during
            // the first layout pass, and persisting that would write the restored value back
            // over itself with whatever the first frame happened to be.
            guard didRestore else { return }
            persist { $0.appearance.sidebarCollapsed = collapsed }
        }
        .onChange(of: columnVisibility) { _, visibility in
            guard didRestore else { return }
            let collapsed = visibility == .detailOnly
            if appState.sidebarCollapsed != collapsed { appState.sidebarCollapsed = collapsed }
        }
        // A session created by `⌘N` or by the palette is selected in the store by the
        // `session_created` event; the route follows it rather than the other way round.
        .onChange(of: stores.sessions.selectedSessionId) { _, id in
            guard let id, appState.route.sessionId != id else { return }
            appState.navigate(to: .session(id))
        }
        .onChange(of: stores.settings.settings.appearance) { _, appearance in
            themeController.setMode(FormDesign.ThemeMode(appearance.themeMode))
            themeController.setTextScale(appearance.textSizeMultiplier)
        }
        .onChange(of: stores.errors) { _, errors in
            for error in errors {
                toasts.post(
                    ToastMessage(
                        tone: .danger, title: error.message, message: error.code, duration: nil))
                stores.dismissError(error)
            }
        }
        // Persisting on every drag frame would flood the core; `task(id:)` cancels the
        // pending write each time the width moves and only lands once it settles.
        .task(id: appState.sidebarWidth) {
            let width = appState.sidebarWidth
            guard width >= theme.metrics.sidebarMinWidth else { return }
            try? await Task.sleep(for: .milliseconds(400))
            guard !Task.isCancelled,
                abs(stores.settings.settings.appearance.sidebarWidth - width) > 0.5
            else { return }
            persist { $0.appearance.sidebarWidth = width }
        }
    }

    // MARK: - Launch restoration (spec 09 §5, acceptance criterion 5)

    private func restore() {
        guard !didRestore else { return }
        didRestore = true

        let appearance = stores.settings.settings.appearance
        appState.sidebarWidth = max(
            theme.metrics.sidebarMinWidth,
            min(theme.metrics.sidebarMaxWidth, appearance.sidebarWidth))
        appState.sidebarCollapsed = appearance.sidebarCollapsed
        columnVisibility = appearance.sidebarCollapsed ? .detailOnly : .all
        themeController.setMode(FormDesign.ThemeMode(appearance.themeMode))
        themeController.setTextScale(appearance.textSizeMultiplier)

        appState.replaceRoute(with: restoredRoute())
        Task { await stores.select(appState.route.sessionId) }
    }

    /// The last route, unless it named a session that is gone. With nothing stored,
    /// `general.startupView` decides.
    private func restoredRoute() -> AppRoute {
        if let stored = ShellRestoration.route {
            switch stored {
            case .home, .noSession:
                return stored
            case let .session(id):
                if stores.sessions.session(id: id) != nil { return stored }
            }
        }
        guard stores.settings.settings.general.startupView != "home" else { return .home }
        return SidebarOrder.visibleSessions(in: stores.sessions).first
            .map { AppRoute.session($0.id) } ?? .home
    }

    private func persist(_ mutate: @escaping (inout FormCore.Settings) -> Void) {
        Task {
            do {
                try await stores.settings.update(mutate)
            } catch {
                Log.ui.error(
                    "updateSettings failed: \(String(describing: error), privacy: .public)")
            }
        }
    }
}

// MARK: - The form the app uses until W10 and W12 land

public extension RootView where HomeContent == PendingSurface, SessionContent == PendingSurface {
    init(
        stores: CoreStores,
        appState: AppState,
        themeController: ThemeController,
        toasts: ToastCenter
    ) {
        self.init(
            stores: stores,
            appState: appState,
            themeController: themeController,
            toasts: toasts,
            home: { PendingSurface.home() },
            session: { _ in PendingSurface.session() }
        )
    }
}

#Preview("Shell — populated") {
    ShellPreview(scenario: .populated)
}

#Preview("Shell — empty") {
    ShellPreview(scenario: .empty)
}

private struct ShellPreview: View {
    @State private var stores: CoreStores
    @State private var appState = AppState()
    @State private var controller: ThemeController
    @State private var toasts = ToastCenter()

    init(scenario: PreviewScenario, mode: FormDesign.ThemeMode = .light) {
        _stores = State(initialValue: CoreStores.preview(scenario))
        _controller = State(initialValue: ThemeController(mode: mode))
    }

    var body: some View {
        RootView(
            stores: stores, appState: appState, themeController: controller, toasts: toasts
        )
        .formTheme(controller)
        .frame(width: 1_280, height: 860)
    }
}
