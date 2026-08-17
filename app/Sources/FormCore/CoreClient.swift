import Foundation

/// Typed access to the core. One instance per app.
///
/// **Owner: W7.** The threading rule is the important part: events arrive on Rust's
/// dispatcher thread, the trampoline only yields into an `AsyncStream`, and the single hop
/// to `@MainActor` happens in the stores that consume `events` — not here.
public actor CoreClient {
    private let transport: CoreTransport
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()
    private var subscriptionToken: Int32?

    private let stream: AsyncStream<CoreEvent>
    private let continuation: AsyncStream<CoreEvent>.Continuation

    /// Events the app dropped because a consumer stalled. Surfaced as a diagnostic rather
    /// than silently swallowed.
    public private(set) var droppedEvents = 0

    public init(transport: CoreTransport) throws {
        self.transport = transport
        // Bounded: a stalled consumer must degrade visibly, not grow without limit.
        var continuation: AsyncStream<CoreEvent>.Continuation!
        self.stream = AsyncStream(bufferingPolicy: .bufferingNewest(4096)) { continuation = $0 }
        self.continuation = continuation
    }

    public init(config: CoreConfig) throws {
        try self.init(transport: FFITransport(config: config))
    }

    /// Must be called once before consuming `events`.
    public func start() throws {
        guard subscriptionToken == nil else { return }
        let continuation = self.continuation
        let decoder = JSONDecoder()
        subscriptionToken = try transport.subscribe { json in
            guard let data = json.data(using: .utf8) else { return }
            guard let event = try? decoder.decode(CoreEvent.self, from: data) else { return }
            continuation.yield(event)
        }
    }

    public nonisolated var events: AsyncStream<CoreEvent> { stream }

    public func query<Q: CoreQuery>(_ query: Q) throws -> Q.Response {
        let request = String(decoding: try encoder.encode(query), as: UTF8.self)
        let response = try transport.query(request)
        let envelope = try decoder.decode(
            Envelope<Q.Response>.self,
            from: Data(response.utf8)
        )
        return try envelope.value()
    }

    @discardableResult
    public func dispatch(_ command: CoreCommand) throws -> String {
        let request = String(decoding: try encoder.encode(command), as: UTF8.self)
        let response = try transport.dispatch(request)
        let envelope = try decoder.decode(Envelope<CommandAck>.self, from: Data(response.utf8))
        return try envelope.value().commandId
    }

    public func shutdown() {
        if let token = subscriptionToken {
            transport.unsubscribe(token)
            subscriptionToken = nil
        }
        continuation.finish()
        transport.shutdown()
    }
}
