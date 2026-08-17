import Foundation

/// The app's store set, and the **one event pump**.
///
/// ## Why one pump and not one per store
///
/// `AsyncStream` has a single consumer: two `for await` loops over the same stream split the
/// events between them rather than each seeing all of them. A per-store pump would therefore
/// need a broadcast layer, and that layer would have to preserve the core's ordering
/// contract (spec 00 §5.1) across four independent tasks — four main-actor hops per event,
/// and four chances to interleave. So there is exactly one task: it consumes
/// `CoreClient.events`, hops to `@MainActor` once, and calls `apply` on each store in a
/// fixed order. Stores are plain `@MainActor @Observable` classes that never touch the
/// stream themselves.
@MainActor
@Observable
public final class CoreStores {
    public let client: CoreClient
    public let sessions: SessionStore
    public let chat: ChatStore
    public let settings: SettingsStore
    public let stats: StatsStore
    public let catalog: CatalogStore

    /// Non-fatal `error` events, newest last. The shell renders these as toasts.
    public private(set) var errors: [CoreErrorBody] = []
    /// Dropped-event and decode-failure counters, refreshed with each pumped event.
    public private(set) var diagnostics = CoreDiagnostics(
        droppedEvents: 0, decodeFailures: 0, eventsDelivered: 0)

    /// An extra event sink, called after the built-in stores have applied the event.
    ///
    /// The stream has exactly one consumer — this class's pump — so a module above
    /// `FormCore` that needs raw events (W13's `AttachmentIntake`, which has to learn the id
    /// the core minted for an attachment) hangs off this rather than opening a second
    /// `for await`, which would *split* the stream rather than duplicate it. One sink,
    /// deliberately: if a second consumer ever needs one, this becomes an array of
    /// observers with a cancellation token, not two mechanisms.
    @ObservationIgnored public var onEvent: (@MainActor (CoreEvent) -> Void)?

    @ObservationIgnored private var pump: Task<Void, Never>?
    /// Set only by `CoreStores.preview`, so a preview can drive its own event log.
    @ObservationIgnored var previewTransport: MockTransport?

    public init(client: CoreClient) {
        self.client = client
        sessions = SessionStore(client: client)
        chat = ChatStore(client: client)
        settings = SettingsStore(client: client)
        stats = StatsStore(client: client)
        catalog = CatalogStore(client: client)
    }

    public convenience init(config: CoreConfig) throws {
        self.init(client: try CoreClient(config: config))
    }

    /// Subscribes, loads the initial documents, and starts the pump. Idempotent.
    public func start() async throws {
        guard pump == nil else { return }
        try await client.start()

        pump = Task { [weak self] in
            guard let client = self?.client else { return }
            for await event in client.events {
                guard let self else { return }
                self.apply(event)
            }
        }

        await catalog.load()
        await settings.load()
        adoptSettings()
        await sessions.load()
        await stats.refresh()
    }

    /// Settings that a store needs to behave, rather than to render.
    private func adoptSettings() {
        chat.queueMode = settings.settings.defaults.queueMode
    }

    /// Select a session and load its transcript — the one call the sidebar, the palette and
    /// the `⌘1`–`⌘9` shortcuts all make.
    public func select(_ sessionId: String?) async {
        sessions.selectedSessionId = sessionId
        guard let sessionId else { return }
        await chat.load(sessionId: sessionId)
    }

    /// Creates a session and selects it once the core acknowledges it. The
    /// `session_created` event carries the same `commandId`, and `SessionStore` selects on
    /// that, so this only has to dispatch.
    @discardableResult
    public func newSession(groupId: String? = nil) async throws -> CommandID {
        try await sessions.createSession(
            groupId: groupId, modelRef: settings.settings.defaults.modelRef)
    }

    /// Fan-out, in a fixed order so a session's summary is up to date before the transcript
    /// that references it reacts.
    private func apply(_ event: CoreEvent) {
        sessions.apply(event)
        chat.apply(event)
        settings.apply(event)
        stats.apply(event)
        if case .settingsChanged = event.kind { adoptSettings() }

        if case let .error(code, message, detail) = event.kind {
            let body = CoreErrorBody(code: code, message: message, detail: detail)
            Log.events.error("core error \(code, privacy: .public): \(message, privacy: .public)")
            errors.append(body)
            if errors.count > 20 { errors.removeFirst(errors.count - 20) }
        }

        let latest = client.diagnostics
        if latest != diagnostics { diagnostics = latest }

        // Last, so a sink sees a fully settled world: stores applied, toasts queued.
        onEvent?(event)
    }

    public func dismissError(_ error: CoreErrorBody) {
        errors.removeAll { $0 == error }
    }

    public func shutdown() async {
        pump?.cancel()
        pump = nil
        await client.shutdown()
    }
}
