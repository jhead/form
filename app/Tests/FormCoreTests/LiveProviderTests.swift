import Foundation
import Testing

@testable import FormCore

/// The Swift stores against a **real** provider.
///
/// The rest of the suite runs on the stub, which is right: the protocol is what those tests
/// assert, and they should not need a network or a key. But the stub is a well-behaved
/// producer, and a live model is not — different cadence, tool calls the model decides to
/// make, and occasional empty streams from the free tier. This is the layer between "the Rust
/// core works" and "the window looks right", and it is the last thing that can be checked
/// without a display.
///
/// Opt in:
///
///     FORM_LIVE=1 swift test --package-path app \
///       -Xlinker -L$(pwd)/core/target/debug --filter LiveProvider
@Suite(
    "Live provider",
    .enabled(if: ProcessInfo.processInfo.environment["FORM_LIVE"] == "1")
)
struct LiveProviderTests {

    private func makeStores() async throws -> (CoreStores, URL) {
        let dir = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("form-live-\(UUID().uuidString)")
        let client = try CoreClient(
            config: CoreConfig(dataDir: dir.path, seedMockData: false, harness: .pi))
        let stores = await CoreStores(client: client)
        try await stores.start()
        return (stores, dir)
    }

    @Test("a live response reconstructs with no reconciliation drift", .timeLimit(.minutes(2)))
    func liveResponseReconciles() async throws {
        let (stores, dir) = try await makeStores()
        defer { try? FileManager.default.removeItem(at: dir) }

        try await stores.client.dispatch(.createSession())
        let session = try #require(try await stores.client.query(ListSessions()).sessions.first)
        await MainActor.run { stores.sessions.selectedSessionId = session.id }
        await stores.chat.load(sessionId: session.id)

        try await stores.chat.send("Reply with exactly: hello from form")

        let deadline = ContinuousClock.now.advanced(by: .seconds(90))
        while await MainActor.run(body: { stores.chat.lastRun == nil }),
            ContinuousClock.now < deadline
        {
            try await Task.sleep(for: .milliseconds(50))
        }

        await MainActor.run {
            let run = stores.chat.lastRun
            #expect(run != nil, "the run should have finished within the deadline")

            // A free-tier model that returns nothing is an upstream condition, not a defect
            // here — but it must surface as a failed run rather than as invented output.
            guard run?.outcome == .completed else {
                #expect(
                    run?.outcome == .failed,
                    "a run that did not complete must report failed, got \(String(describing: run?.outcome))"
                )
                return
            }

            let assistant = stores.chat.messages.compactMap { $0.message.asAssistant }.last
            #expect(assistant?.text.isEmpty == false, "a completed run must have text")
            #expect(
                stores.chat.reconciliationRepairs == 0,
                "the incremental transcript disagreed with the provider's own partial")
            #expect(stores.diagnostics.isHealthy, "events were dropped or failed to decode")
            #expect(run?.usage.totalTokens ?? 0 > 0, "a completed run must report usage")
        }

        await stores.shutdown()
    }

    @Test("the catalog the picker renders comes from the live registry")
    func catalogIsLive() async throws {
        let (stores, dir) = try await makeStores()
        defer { try? FileManager.default.removeItem(at: dir) }

        let catalog = try await stores.client.query(GetCatalog())
        let openrouter = catalog.providers.first { $0.id == "openrouter" }
        let models = openrouter?.models ?? []

        // The bundled snapshot carries a few hundred; the live registry carries far more.
        #expect(models.count > 300, "expected the live OpenRouter list, got \(models.count)")
        #expect(
            models.contains { $0.id == "z-ai/glm-5.2:free" },
            "a model the bundled snapshot lacks should be present after the live refresh")
        #expect(
            models.allSatisfy { $0.contextWindow > 0 },
            "every model needs a real context window or the ring cannot be drawn")

        await stores.shutdown()
    }
}
