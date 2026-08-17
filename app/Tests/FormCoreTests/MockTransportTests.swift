import Foundation
import Testing

@testable import FormCore

/// `MockTransport` is what makes every preview in W8–W14 work with no Rust build
/// (spec 07 §6), so it gets the same scrutiny as the real transport.
@Suite("MockTransport")
struct MockTransportTests {

    @Test("answers every query the app makes at launch")
    func answersQueries() async throws {
        let client = CoreClient(mock: MockTransport())

        let sessions = try await client.query(ListSessions())
        #expect(sessions.groups.count == 3)
        #expect(sessions.sessions.contains { $0.id == MockCorpus.demo.primarySessionId })
        #expect(
            sessions.sessions.allSatisfy { !$0.archived },
            "archived sessions are excluded unless asked for")

        let withArchived = try await client.query(ListSessions(includeArchived: true))
        #expect(withArchived.sessions.count > sessions.sessions.count)

        let session = try await client.query(
            GetSession(sessionId: MockCorpus.demo.primarySessionId))
        #expect(session.entries.count == 4)
        #expect(session.entries.first?.message?.asUser != nil)

        let settings = try await client.query(GetSettings())
        #expect(settings.providers["anthropic"]?.hasKey == true)

        let catalog = try await client.query(GetCatalog())
        #expect(catalog.providers.count == 2)
        #expect(catalog.model(settings.defaults.modelRef)?.name == "Opus 5")

        let stats = try await client.query(GetStats(range: .d30, tz: "UTC"))
        #expect(stats.daily.count == 30)
        #expect(stats.hourly.count == 24)
        #expect(stats.weekdayHour.count == 7)
        #expect(stats.models.count == 3)
        #expect(stats.headline.totalTokens > 0)

        let usage = try await client.query(
            GetContextUsage(sessionId: MockCorpus.demo.primarySessionId))
        #expect(usage.segments.count == 5)
        #expect(usage.fraction > 0 && usage.fraction < 1)

        let roots = try await client.query(ListRecentRoots())
        #expect(roots.count == 2)
    }

    @Test("an unknown session comes back as an error envelope, not a crash")
    func errorEnvelope() async {
        let client = CoreClient(mock: MockTransport())
        await #expect(throws: CoreErrorBody.self) {
            _ = try await client.query(GetSession(sessionId: "nope"))
        }
    }

    @Test("commands are recorded and echoed as events")
    func commandsEcho() async throws {
        let transport = MockTransport(replaysRuns: false)
        let client = CoreClient(mock: transport)
        try await client.start()

        var seen: [CoreEvent] = []
        let collector = Task {
            for await event in client.events {
                seen.append(event)
                if seen.count == 2 { break }
            }
            return seen
        }

        try await client.dispatch(.createSession(title: "Fresh"))
        try await client.dispatch(.createGroup(name: "Bucket"))

        let events = await collector.value
        #expect(transport.commands.count == 2)
        #expect(events.contains { if case .sessionCreated = $0.kind { true } else { false } })
        #expect(events.contains { if case .groupsChanged = $0.kind { true } else { false } })

        await client.shutdown()
    }

    @Test("a replayed run drives the stores exactly as the real core does")
    func replayDrivesStores() async throws {
        let stores = await CoreStores.preview(.populated, speed: 0)
        try await stores.start()

        let sessionId = await MainActor.run { stores.sessions.selectedSessionId ?? "" }
        try await stores.chat.send("Add a health check endpoint")

        // Wait for the run to finish, with a ceiling so a hang fails rather than blocks.
        let deadline = ContinuousClock.now.advanced(by: .seconds(10))
        while await MainActor.run(body: { stores.chat.lastRun == nil }),
            ContinuousClock.now < deadline
        {
            try await Task.sleep(for: .milliseconds(20))
        }

        await MainActor.run {
            #expect(stores.chat.lastRun?.outcome == .completed)
            #expect(stores.chat.isStreaming == false)
            #expect(stores.chat.reconciliationRepairs == 0)
            #expect(stores.chat.turns.count == 1)
            #expect(stores.chat.toolRuns.count == 1)
            #expect(stores.chat.messages.contains { $0.entry.sessionId == sessionId })
            #expect(stores.diagnostics.isHealthy)
        }

        await stores.shutdown()
    }

    @Test("preview scenarios are populated without touching Rust")
    func previewScenarios() async {
        await MainActor.run {
            let populated = CoreStores.preview(.populated)
            #expect(populated.sessions.sessions.isEmpty == false)
            #expect(populated.chat.entries.isEmpty == false)
            #expect(populated.stats.stats?.headline.totalTokens ?? 0 > 0)
            #expect(populated.catalog.providers.isEmpty == false)
            #expect(populated.settings.isLoaded)

            let empty = CoreStores.preview(.empty)
            #expect(empty.sessions.sessions.isEmpty)
            #expect(empty.stats.stats?.isEmpty == true)

            let streaming = CoreStores.preview(.streaming)
            #expect(streaming.chat.isStreaming)
            #expect(streaming.chat.streamingMessage != nil)

            #expect(ChatStore.preview().messages.isEmpty == false)
            #expect(SessionStore.preview().ordered.isEmpty == false)
            #expect(StatsStore.previewEmpty().stats?.isEmpty == true)
            #expect(CatalogStore.preview().search("opus").first?.model.id == "claude-opus-5")
        }
    }
}
