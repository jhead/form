import Foundation
import os

/// Typed access to the core. One instance per app.
///
/// The threading rule is the important part (spec 00 §7): events arrive on Rust's single
/// dispatcher thread, the C trampoline does nothing but hand the bytes to an
/// `AsyncStream.Continuation`, a Swift task does the decoding, and the single hop to
/// `@MainActor` happens in the stores that consume `events` — never here.
public actor CoreClient {
    private let transport: CoreTransport
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()
    private var subscriptionToken: Int32?
    private var decodeTask: Task<Void, Never>?
    private var isShutDown = false

    /// Raw JSON as it leaves the callback. Bounded, because a stalled consumer must degrade
    /// visibly rather than growing without limit.
    private let rawStream: AsyncStream<String>
    private let rawContinuation: AsyncStream<String>.Continuation

    private let eventStream: AsyncStream<CoreEvent>
    private let eventContinuation: AsyncStream<CoreEvent>.Continuation

    private let counters = Counters()

    /// Spec 07 §3: `.bufferingNewest(4096)`, with what falls off the end counted.
    static let bufferCapacity = 4096

    /// Asserts the ABI (spec 07 §3): a mismatch is diagnosable and fatal to startup, not
    /// something to discover halfway through a decode.
    public init(transport: CoreTransport) throws {
        guard transport.abiVersion == formABIVersion else {
            throw TransportError.abiMismatch(
                expected: formABIVersion, actual: transport.abiVersion)
        }
        self.init(unchecked: transport)
    }

    public init(config: CoreConfig) throws {
        try self.init(transport: FFITransport(config: config))
    }

    /// Previews and tests. Cannot fail — `MockTransport` reports this build's ABI by
    /// construction, so no call site needs a `try!`.
    public init(mock: MockTransport) {
        self.init(unchecked: mock)
    }

    private init(unchecked transport: CoreTransport) {
        self.transport = transport

        var rawContinuation: AsyncStream<String>.Continuation!
        rawStream = AsyncStream(bufferingPolicy: .bufferingNewest(Self.bufferCapacity)) {
            rawContinuation = $0
        }
        self.rawContinuation = rawContinuation

        var eventContinuation: AsyncStream<CoreEvent>.Continuation!
        eventStream = AsyncStream(bufferingPolicy: .bufferingNewest(Self.bufferCapacity)) {
            eventContinuation = $0
        }
        self.eventContinuation = eventContinuation
    }

    /// Subscribes and starts the decode pump. Must be called once before consuming `events`;
    /// calling it twice is a no-op.
    public func start() throws {
        guard subscriptionToken == nil, !isShutDown else { return }

        let raw = rawContinuation
        let counters = self.counters
        // The whole callback body. `String(cString:)` is the one allocation, and the yield is
        // lock-free — no decoding, no re-entry into the core, no actor hop.
        subscriptionToken = try transport.subscribe { json in
            if case .dropped = raw.yield(json) { counters.recordDrop() }
        }

        let events = eventContinuation
        decodeTask = Task.detached(priority: .userInitiated) { [rawStream] in
            let decoder = JSONDecoder()
            for await json in rawStream {
                let event: CoreEvent
                do {
                    event = try decoder.decode(CoreEvent.self, from: Data(json.utf8))
                } catch {
                    // Unknown *types* decode to `.unknown`; reaching here means malformed
                    // JSON or a changed field shape, which is a drift bug worth seeing.
                    counters.recordDecodeFailure()
                    Log.events.error(
                        "undecodable event: \(String(describing: error), privacy: .public)")
                    continue
                }
                counters.recordDelivered()
                if case .dropped = events.yield(event) { counters.recordDrop() }
            }
            events.finish()
        }
    }

    /// The event stream. Single-consumer by construction — `CoreStores` owns the one pump
    /// and fans out from there (see `CoreStores`).
    public nonisolated var events: AsyncStream<CoreEvent> { eventStream }

    public nonisolated var abiVersion: UInt32 { formABIVersion }

    /// Counters a stalled or drifting core shows up in. Surfaced in the UI's diagnostics
    /// rather than swallowed (spec 07 §3).
    public nonisolated var diagnostics: CoreDiagnostics { counters.snapshot() }

    public func query<Q: CoreQuery>(_ query: Q) throws -> Q.Response {
        let request = String(decoding: try encoder.encode(query), as: UTF8.self)
        let response = try transport.query(request)
        let envelope = try decoder.decode(
            Envelope<Q.Response>.self, from: Data(response.utf8))
        return try envelope.value()
    }

    /// Every outcome arrives as an event carrying this id — nothing but the ack comes back
    /// here, including failures (spec 00 §4).
    @discardableResult
    public func dispatch(_ command: CoreCommand) throws -> CommandID {
        let request = String(decoding: try encoder.encode(command), as: UTF8.self)
        let response = try transport.dispatch(request)
        let envelope = try decoder.decode(Envelope<CommandAck>.self, from: Data(response.utf8))
        return try envelope.value().commandId
    }

    /// Unsubscribes, drains, and frees. Safe to call while a run is streaming, and safe to
    /// call twice.
    public func shutdown() {
        guard !isShutDown else { return }
        isShutDown = true
        if let token = subscriptionToken {
            transport.unsubscribe(token)
            subscriptionToken = nil
        }
        rawContinuation.finish()
        decodeTask?.cancel()
        decodeTask = nil
        eventContinuation.finish()
        transport.shutdown()
    }
}

public struct CoreDiagnostics: Sendable, Equatable {
    /// Events discarded because a consumer could not keep up.
    public var droppedEvents: Int
    /// Payloads that did not decode at all — always a bug, never expected traffic.
    public var decodeFailures: Int
    public var eventsDelivered: Int

    public var isHealthy: Bool { droppedEvents == 0 && decodeFailures == 0 }
}

/// Touched from the core's dispatcher thread, so it is a lock rather than actor state — the
/// callback must never wait on an actor.
private final class Counters: Sendable {
    private let state = OSAllocatedUnfairLock(
        initialState: CoreDiagnostics(droppedEvents: 0, decodeFailures: 0, eventsDelivered: 0))

    func recordDrop() { state.withLock { $0.droppedEvents += 1 } }
    func recordDecodeFailure() { state.withLock { $0.decodeFailures += 1 } }
    func recordDelivered() { state.withLock { $0.eventsDelivered += 1 } }
    func snapshot() -> CoreDiagnostics { state.withLock { $0 } }
}
