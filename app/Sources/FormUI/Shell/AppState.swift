import FormDesign
import Observation
import SwiftUI

/// Shell-wide navigation and presentation state (spec 09 §5).
///
/// Created once at the root and injected via `.environment`. W14 mutates `searchPresented`
/// and `findPresented`; W13 presents on `preferencesPresented`. Everything else is W9's.
///
/// `route` is `private(set)` on purpose: every change has to go through the history stack, so
/// there is no way to move the content pane without `⌘[` / `⌘]` knowing about it.
@MainActor
@Observable
public final class AppState {
    public private(set) var route: AppRoute
    public var sidebarCollapsed: Bool
    public var sidebarWidth: CGFloat

    /// `⌘K`, owned by W14.
    public var searchPresented = false
    /// `⌘F`, owned by W14.
    public var findPresented = false
    /// `⌘,`, owned by W13. The shell's footer menu is one of the things that opens it.
    public var preferencesPresented = false

    /// The session the `Code` segment returns to. Distinct from `route` so switching to
    /// `Home` and back does not lose the user's place.
    public private(set) var lastSessionId: String?

    private var history: RouteHistory

    public init(
        route: AppRoute = .home,
        sidebarCollapsed: Bool = false,
        sidebarWidth: CGFloat = MetricTokens.standard.sidebarWidth
    ) {
        self.route = route
        self.sidebarCollapsed = sidebarCollapsed
        self.sidebarWidth = sidebarWidth
        history = RouteHistory(root: route)
        lastSessionId = route.sessionId
    }

    // MARK: - Navigation

    public func navigate(to route: AppRoute) {
        guard route != self.route else { return }
        history.push(route)
        adopt(route)
    }

    /// Corrects the current route in place — restoration landing on a session that has since
    /// been deleted, for instance. Does not add a history entry.
    public func replaceRoute(with route: AppRoute) {
        guard route != self.route else { return }
        history.replaceCurrent(route)
        adopt(route)
    }

    public var canGoBack: Bool { history.canGoBack }
    public var canGoForward: Bool { history.canGoForward }

    /// `⌘[`.
    public func goBack() {
        guard let route = history.back() else { return }
        adopt(route)
    }

    /// `⌘]`.
    public func goForward() {
        guard let route = history.forward() else { return }
        adopt(route)
    }

    /// Called when a session disappears, so history cannot navigate into a dead route.
    public func forget(sessionId: String) {
        if lastSessionId == sessionId { lastSessionId = nil }
        history.remove(.session(sessionId))
        if route == .session(sessionId) { route = history.current }
    }

    private func adopt(_ route: AppRoute) {
        self.route = route
        if let id = route.sessionId { lastSessionId = id }
    }

    /// The `Home` / `Code` segmented control (spec 09 §2). Selecting `Code` restores the last
    /// session; with none, the content pane shows its empty state.
    public var segment: SidebarSegment {
        get { route.isHome ? .home : .code }
        set {
            switch newValue {
            case .home: navigate(to: .home)
            case .code: navigate(to: lastSessionId.map(AppRoute.session) ?? .noSession)
            }
        }
    }

    public func toggleSidebar() { sidebarCollapsed.toggle() }
}

/// The narrow face W14's command table depends on (spec 14 §1). `AppState` is the app's
/// implementation; `PreviewAppState` is the one W14 tests against.
extension AppState: CommandAppState {
    public var currentSessionId: String? { route.sessionId }
    public var isShowingHome: Bool { route.isHome }
    public func showHome() { navigate(to: .home) }
    public func showSession(_ sessionId: String) { navigate(to: .session(sessionId)) }
}

/// The two segments of the sidebar's top control.
public enum SidebarSegment: String, Hashable, Sendable, CaseIterable {
    case home, code

    public var title: String {
        switch self {
        case .home: "Home"
        case .code: "Code"
        }
    }

    public var systemImage: String {
        switch self {
        case .home: "house"
        case .code: "chevron.left.forwardslash.chevron.right"
        }
    }
}
