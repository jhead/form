import Foundation

/// What the content pane is showing (spec 09 §1).
public enum AppRoute: Hashable, Sendable {
    case home
    case session(String)
    /// `Code` selected with nothing to return to. Carries no content, so it is not a
    /// `.session` — the content pane renders its empty state (spec 09 §4).
    case noSession

    public var sessionId: String? {
        if case let .session(id) = self { return id }
        return nil
    }

    public var isHome: Bool { self == .home }

    /// A flat string form, so launch restoration can round-trip a route without inventing a
    /// key in the core's settings document (spec 09 §5).
    var persisted: String {
        switch self {
        case .home: "home"
        case .noSession: "noSession"
        case let .session(id): "session:\(id)"
        }
    }

    init?(persisted: String) {
        switch persisted {
        case "home":
            self = .home
        case "noSession":
            self = .noSession
        default:
            guard persisted.hasPrefix("session:") else { return nil }
            let id = String(persisted.dropFirst("session:".count))
            guard !id.isEmpty else { return nil }
            self = .session(id)
        }
    }
}

/// Browser-style history behind `⌘[` / `⌘]` (F12).
///
/// Bounded: a long working session would otherwise accumulate an entry per selection for the
/// life of the process. The oldest entries fall off the front, which is invisible in practice
/// and keeps the stack a fixed cost.
struct RouteHistory: Equatable {
    static let capacity = 50

    private(set) var entries: [AppRoute]
    private(set) var index: Int

    init(root: AppRoute = .home) {
        entries = [root]
        index = 0
    }

    var current: AppRoute { entries[index] }
    var canGoBack: Bool { index > 0 }
    var canGoForward: Bool { index < entries.count - 1 }

    mutating func push(_ route: AppRoute) {
        guard route != current else { return }
        // A new destination truncates the forward branch, exactly as a browser does.
        if canGoForward { entries.removeSubrange((index + 1)...) }
        entries.append(route)
        if entries.count > Self.capacity {
            entries.removeFirst(entries.count - Self.capacity)
        }
        index = entries.count - 1
    }

    /// Swaps the current entry without adding one — used when the shell corrects a route it
    /// just restored (a session that no longer exists, say).
    mutating func replaceCurrent(_ route: AppRoute) {
        entries[index] = route
    }

    @discardableResult
    mutating func back() -> AppRoute? {
        guard canGoBack else { return nil }
        index -= 1
        return current
    }

    @discardableResult
    mutating func forward() -> AppRoute? {
        guard canGoForward else { return nil }
        index += 1
        return current
    }

    /// Drops every occurrence of a route that no longer exists — a deleted session must not
    /// leave a hole `⌘[` can navigate into. Runs of the same route left behind by the removal
    /// are collapsed so back/forward never appear to stall.
    mutating func remove(_ route: AppRoute) {
        guard entries.contains(route) else { return }

        var kept: [AppRoute] = []
        var newIndex = 0
        for (position, entry) in entries.enumerated() {
            guard entry != route else {
                if position <= index { newIndex = max(0, kept.count - 1) }
                continue
            }
            if kept.last != entry { kept.append(entry) }
            if position <= index { newIndex = kept.count - 1 }
        }

        entries = kept.isEmpty ? [.home] : kept
        index = min(max(0, newIndex), entries.count - 1)
    }
}
