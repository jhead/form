import Foundation
import Observation

/// What the shortcut table needs from the app's routing state.
///
/// ## Why a protocol and not `AppState` itself
///
/// Spec 14 §1 writes `isEnabled` as `(AppState) -> Bool`, and `AppState` is W9's type
/// (spec 09 §5) in W9's directory. Two workstreams cannot both declare it — one copy of the
/// name in one module is all Swift allows — so this is the shape W14 depends on and W9's
/// `AppState` conforms to. `route`, `sidebarWidth`, group collapse and everything else on
/// `AppState` stay invisible here, which is the point: the command table cannot reach into
/// shell state it does not own.
///
/// `searchPresented` and `findPresented` are the two flags spec 09 marks "owned by W14".
/// They are mirrored rather than owned outright so W9's sidebar magnifier button can raise
/// the palette by setting the flag; `CommandCenter` observes both directions.
@MainActor
public protocol CommandAppState: AnyObject {
    var sidebarCollapsed: Bool { get set }
    /// `⌘K`.
    var searchPresented: Bool { get set }
    /// `⌘F`.
    var findPresented: Bool { get set }

    /// `nil` when the Home dashboard is on screen.
    var currentSessionId: String? { get }
    var isShowingHome: Bool { get }

    func showHome()
    func showSession(_ sessionId: String)

    // The bounded route stack W9 maintains — `⌘[` / `⌘]` traverse *this*, not the sidebar
    // order, so back after a palette jump returns where you came from (spec 14 §2).
    var canGoBack: Bool { get }
    var canGoForward: Bool { get }
    func goBack()
    func goForward()
}

/// A working `CommandAppState` for previews, tests, and as a reference implementation of the
/// route stack `⌘[` / `⌘]` expect. Not the app's state — W9's `AppState` is.
@MainActor
@Observable
public final class PreviewAppState: CommandAppState {
    public enum Route: Equatable, Sendable {
        case home
        case session(String)
    }

    public private(set) var route: Route
    public var sidebarCollapsed = false
    public var searchPresented = false
    public var findPresented = false

    /// Bounded so a long session of jumping around cannot grow without limit (spec 09 §5).
    public static let historyLimit = 50

    private var back: [Route] = []
    private var forward: [Route] = []

    public init(route: Route = .home) {
        self.route = route
    }

    public var currentSessionId: String? {
        if case let .session(id) = route { return id }
        return nil
    }

    public var isShowingHome: Bool { route == .home }

    public func showHome() { navigate(to: .home) }
    public func showSession(_ sessionId: String) { navigate(to: .session(sessionId)) }

    private func navigate(to next: Route) {
        guard next != route else { return }
        back.append(route)
        if back.count > Self.historyLimit { back.removeFirst(back.count - Self.historyLimit) }
        forward.removeAll()
        route = next
    }

    public var canGoBack: Bool { !back.isEmpty }
    public var canGoForward: Bool { !forward.isEmpty }

    public func goBack() {
        guard let previous = back.popLast() else { return }
        forward.append(route)
        route = previous
    }

    public func goForward() {
        guard let next = forward.popLast() else { return }
        back.append(route)
        route = next
    }
}
