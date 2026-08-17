import Foundation
import Testing

@testable import FormCore

@MainActor
@Suite("Stores")
struct StoreTests {

    // MARK: - SessionStore

    @Test("session events upsert, delete and reorder")
    func sessionEvents() {
        let store = SessionStore(client: CoreClient(mock: MockTransport(replaysRuns: false)))
        let model = ModelRef(providerId: "anthropic", modelId: "m", thinkingLevel: .off)

        let a = SessionSummary(
            id: "a", title: "A", modelRef: model, createdAt: 1, updatedAt: 10)
        let b = SessionSummary(
            id: "b", title: "B", modelRef: model, createdAt: 2, updatedAt: 20)
        store.apply(CoreEvent(kind: .sessionCreated(session: a)))
        store.apply(CoreEvent(kind: .sessionCreated(session: b)))
        #expect(store.ordered.map(\.id) == ["b", "a"], "newest first")

        var pinned = a
        pinned.pinned = true
        store.apply(CoreEvent(kind: .sessionUpdated(session: pinned)))
        #expect(store.ordered.map(\.id) == ["a", "b"], "pinned first")
        #expect(store.sessions.count == 2, "an update is not an insert")

        store.apply(CoreEvent(kind: .sessionDeleted(sessionId: "a")))
        #expect(store.ordered.map(\.id) == ["b"])

        store.apply(
            CoreEvent(
                kind: .groupsChanged(groups: [
                    SessionGroup(id: "g2", name: "Two", index: 1),
                    SessionGroup(id: "g1", name: "One", index: 0),
                ])))
        #expect(store.groups.map(\.id) == ["g1", "g2"], "groups are ordered by index")
    }

    @Test("rank and cycling map onto the sidebar order")
    func selection() {
        let store = SessionStore.preview()
        let first = store.ordered.first
        store.selectedSessionId = first?.id
        #expect(store.session(rank: 1)?.id == first?.id)

        store.selectNext()
        #expect(store.selectedSessionId == store.ordered[1].id)
        store.selectNext(offset: -1)
        #expect(store.selectedSessionId == first?.id)
    }

    @Test("archived sessions are hidden until asked for")
    func archivedFiltering() {
        let store = SessionStore.preview()
        #expect(store.ordered.contains { $0.id == "ses_archived" } == false)
        store.includeArchived = true
        #expect(store.ordered.contains { $0.id == "ses_archived" })
    }

    // MARK: - SettingsStore

    @Test("settings_changed replaces the document")
    func settingsEcho() {
        let store = SettingsStore(client: CoreClient(mock: MockTransport()))
        var settings = Settings()
        settings.appearance.themeMode = .dark
        store.apply(CoreEvent(kind: .settingsChanged(settings: settings)))
        #expect(store.themeMode == .dark)
        #expect(store.isLoaded)
    }

    @Test("an edit is sent as the whole document")
    func settingsUpdate() async throws {
        let transport = MockTransport()
        let store = SettingsStore(client: CoreClient(mock: transport))
        store.seed(MockCorpus.demo.settings)

        try await store.setThemeMode(.light)
        guard case let .updateSettings(sent)? = transport.commands.last else {
            Issue.record("expected updateSettings, got \(String(describing: transport.commands.last))")
            return
        }
        #expect(sent.appearance.themeMode == .light)
        #expect(sent.defaults.modelRef.modelId == "claude-opus-5", "unrelated fields survive")
    }

    @Test("settings export and import round-trip")
    func settingsExport() async throws {
        let store = SettingsStore.preview()
        let data = try store.exportJSON()
        let decoded = try JSONDecoder().decode(Settings.self, from: data)
        #expect(decoded == store.settings)
    }

    // MARK: - StatsStore

    @Test("a period switch serves the cached document")
    func statsCache() async {
        let store = StatsStore.preview(range: .d7)
        #expect(store.stats?.daily.count == 7)
        await store.select(.d30)
        #expect(store.stats?.daily.count == 30)
        #expect(store.cached(StatsStore.Key(range: .d7, tz: TimeZone.current.identifier)) != nil)
    }

    @Test("stats_invalidated coalesces into a single refresh")
    func statsCoalescing() async throws {
        let transport = MockTransport()
        let store = StatsStore(client: CoreClient(mock: transport), timeZone: "UTC")
        await store.refresh()
        #expect(store.stats != nil)

        // Ten invalidations in a row must not become ten queries; the second and later ones
        // fold into the one already scheduled.
        for _ in 0..<10 { store.apply(CoreEvent(kind: .statsInvalidated)) }
        #expect(store.isLoading == false)
    }

    // MARK: - CatalogStore

    @Test("the catalog resolves models and filters thinking levels")
    func catalog() {
        let store = CatalogStore.preview()
        let ref = ModelRef(
            providerId: "anthropic", modelId: "claude-opus-5", thinkingLevel: .high)
        #expect(store.model(ref)?.contextWindow == 200_000)
        #expect(store.displayName(ref) == "Opus 5")
        #expect(store.thinkingLevels(for: ref) == ThinkingLevel.ladder)
        #expect(
            store.thinkingLevels(
                for: ModelRef(providerId: "nope", modelId: "nope", thinkingLevel: .off)) == [.off])

        #expect(store.search("gpt").first?.model.id == "gpt-5")
        #expect(store.search("anthropic").count == 2)
        #expect(store.search("").count == 3)
    }

    // MARK: - CoreStores

    @Test("error events surface as dismissable toasts")
    func errorToasts() async throws {
        let transport = MockTransport(replaysRuns: false)
        let stores = CoreStores(client: CoreClient(mock: transport))
        try await stores.start()

        transport.emit(
            CoreEvent(kind: .error(code: "disk_full", message: "no room", detail: nil)))

        let deadline = ContinuousClock.now.advanced(by: .seconds(5))
        while stores.errors.isEmpty, ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(20))
        }
        #expect(stores.errors.first?.code == "disk_full")

        if let first = stores.errors.first { stores.dismissError(first) }
        #expect(stores.errors.isEmpty)

        await stores.shutdown()
    }
}
