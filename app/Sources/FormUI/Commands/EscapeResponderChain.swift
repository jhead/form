import Foundation
import Observation

/// One link in the `Esc` chain.
///
/// Spec 14 §2 asks for an ordered responder chain rather than nested `if`s in a view,
/// because the three things `Esc` might mean live in three different workstreams: the
/// overlays are W14's, streaming is the core's, and composer focus is W10's. A view that
/// branched on all three would have to know about all three.
@MainActor
public struct EscapeResponder: Identifiable {
    public let id: String
    /// Lower runs first. Use the `Order` constants rather than raw numbers.
    public let order: Int
    /// `true` if this responder consumed the key; the chain stops there.
    public let handle: @MainActor () async -> Bool

    public init(id: String, order: Int, handle: @escaping @MainActor () async -> Bool) {
        self.id = id
        self.order = order
        self.handle = handle
    }

    /// The three stages spec 14 §2 names, spaced so a workstream can slot in between.
    public enum Order {
        public static let overlay = 100
        public static let stopStreaming = 200
        public static let composerFocus = 300
    }
}

/// The ordered chain `Esc` walks. Registration is by id, so re-registering replaces rather
/// than duplicating — a view that re-appears does not stack up handlers.
@MainActor
@Observable
public final class EscapeResponderChain {
    private var responders: [EscapeResponder] = []

    public init() {}

    public func register(_ responder: EscapeResponder) {
        responders.removeAll { $0.id == responder.id }
        responders.append(responder)
        // Stable within an order so registration sequence breaks ties predictably.
        responders.sort { $0.order < $1.order }
    }

    public func register(
        id: String, order: Int, handle: @escaping @MainActor () async -> Bool
    ) {
        register(EscapeResponder(id: id, order: order, handle: handle))
    }

    public func unregister(id: String) {
        responders.removeAll { $0.id == id }
    }

    /// The order the chain will be walked in — what the test asserts against.
    public var responderIDs: [String] { responders.map(\.id) }

    /// Walks the chain and returns whether anything consumed the key. `false` means `Esc`
    /// should be passed through to whatever is below.
    @discardableResult
    public func handle() async -> Bool {
        for responder in responders {
            if await responder.handle() { return true }
        }
        return false
    }
}
