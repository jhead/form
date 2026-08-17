import Foundation

/// What a `#Preview` in W8–W14 reaches for.
///
/// Everything here is synchronous and Rust-free: `CoreStores.preview()` hands back a fully
/// populated store set on the first pass, so a preview renders content immediately instead
/// of flashing an empty state while a query resolves. Call `startStreaming()` on it when the
/// preview wants motion.
public enum PreviewScenario: Sendable {
    /// Grouped sessions, a finished transcript, a populated dashboard.
    case populated
    /// First launch: no sessions, no history.
    case empty
    /// A session caught mid-run, with a partial assistant message.
    case streaming
}

extension CoreStores {
    @MainActor
    public static func preview(
        _ scenario: PreviewScenario = .populated,
        corpus: MockCorpus = .demo,
        speed: Double = 1
    ) -> CoreStores {
        let transport = MockTransport(corpus: corpus, speed: speed)
        let stores = CoreStores(client: CoreClient(mock: transport))
        stores.previewTransport = transport

        switch scenario {
        case .empty:
            stores.settings.seed(corpus.settings)
            stores.catalog.seed(corpus.catalog)
            stores.stats.seed([.d7: UsageStats(range: .d7)])
        case .populated, .streaming:
            stores.sessions.seed(corpus)
            stores.settings.seed(corpus.settings)
            stores.catalog.seed(corpus.catalog)
            stores.stats.seed(corpus.stats)

            let id = scenario == .streaming ? "ses_streaming" : corpus.primarySessionId
            if let session = corpus.session(id) {
                stores.sessions.selectedSessionId = id
                stores.chat.seed(
                    session, streaming: scenario == .streaming,
                    usage: corpus.contextUsage[id])
            }
        }
        return stores
    }

    /// Subscribes the preview to its mock transport and replays a run, so indicators and
    /// streaming text actually move in the canvas.
    @MainActor
    public func startPreviewStreaming(prompt: String = "Add a health check endpoint") {
        guard let transport = previewTransport else { return }
        Task {
            try? await self.start()
            let id = self.sessions.selectedSessionId ?? MockCorpus.demo.primarySessionId
            let model = self.sessions.selected?.modelRef
                ?? ModelRef(
                    providerId: "anthropic", modelId: "claude-opus-5", thinkingLevel: .high)
            transport.replay(
                MockCorpus.recordedRun(sessionId: id, prompt: prompt, model: model))
        }
    }
}

extension SessionStore {
    @MainActor
    public static func preview(corpus: MockCorpus = .demo) -> SessionStore {
        let store = SessionStore(client: CoreClient(mock: MockTransport(corpus: corpus)))
        store.seed(corpus)
        return store
    }
}

extension ChatStore {
    @MainActor
    public static func preview(
        sessionId: String = MockCorpus.demo.primarySessionId,
        streaming: Bool = false,
        corpus: MockCorpus = .demo
    ) -> ChatStore {
        let store = ChatStore(client: CoreClient(mock: MockTransport(corpus: corpus)))
        if let session = corpus.session(sessionId) {
            store.seed(session, streaming: streaming, usage: corpus.contextUsage[sessionId])
        }
        return store
    }
}

extension SettingsStore {
    @MainActor
    public static func preview(corpus: MockCorpus = .demo) -> SettingsStore {
        let store = SettingsStore(client: CoreClient(mock: MockTransport(corpus: corpus)))
        store.seed(corpus.settings)
        return store
    }
}

extension StatsStore {
    @MainActor
    public static func preview(corpus: MockCorpus = .demo, range: StatsRange = .d7) -> StatsStore {
        let store = StatsStore(client: CoreClient(mock: MockTransport(corpus: corpus)))
        store.range = range
        store.seed(corpus.stats)
        return store
    }

    /// The sparse/first-launch dashboard (F11.12).
    @MainActor
    public static func previewEmpty(range: StatsRange = .d7) -> StatsStore {
        let store = StatsStore(client: CoreClient(mock: MockTransport()))
        store.range = range
        store.seed([range: UsageStats(range: range)])
        return store
    }
}

extension CatalogStore {
    @MainActor
    public static func preview(corpus: MockCorpus = .demo) -> CatalogStore {
        let store = CatalogStore(client: CoreClient(mock: MockTransport(corpus: corpus)))
        store.seed(corpus.catalog)
        return store
    }
}
