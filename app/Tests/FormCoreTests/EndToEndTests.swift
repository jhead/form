import Foundation
import Testing

@testable import FormCore

/// The proof that the Swift ↔ Rust boundary works: a real core, a real run, real events.
/// Acceptance criterion 4 in the PRD depends on this path, so it is a test rather than
/// something verified by hand.
@Suite("Swift ↔ Rust boundary")
struct EndToEndTests {

    private func makeClient() throws -> (CoreClient, URL) {
        let dir = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("form-tests-\(UUID().uuidString)")
        let config = CoreConfig(
            dataDir: dir.path,
            seedMockData: false,
            // 40× so the test is fast without changing event ordering.
            harnessSpeed: 40,
            // The suite asserts the *protocol*, not a provider. The stub emits the same
            // events the real agent does, so this stays offline, deterministic, and free.
            harness: .stub
        )
        return (try CoreClient(config: config), dir)
    }

    private func makeSession(_ client: CoreClient) async throws -> SessionSummary {
        try await client.dispatch(.createSession())
        let sessions = try await client.query(ListSessions())
        return try #require(sessions.sessions.first)
    }

    @Test("a prompt streams a full, correctly ordered run")
    func streamsARun() async throws {
        let (client, dir) = try makeClient()
        defer { try? FileManager.default.removeItem(at: dir) }

        try await client.start()
        let session = try await makeSession(client)
        try await client.dispatch(.sendPrompt(sessionId: session.id, text: "Add a health check"))

        // The stub harness plans 1–3 turns and fails one run in nine, all seeded from the
        // session id (spec 02 §5) — so this asserts the *ordering contract*, which holds for
        // every plan, rather than one particular script.
        var order: [String] = []
        var deltas: [String: String] = [:]
        var terminal: [String: AssistantMessage] = [:]
        var openMessages = Set<String>()
        var updatesOutsideAMessage = 0
        var outcome: RunOutcome?

        loop: for await event in client.events {
            switch event.kind {
            case .runStart: order.append("run_start")
            case .turnStart: order.append("turn_start")
            case let .messageStart(_, entry):
                // The user's own message is logged before the run starts, so only the
                // assistant's messages are part of the run's ordering contract.
                guard entry.message?.asAssistant != nil else { break }
                order.append("message_start")
                openMessages.insert(entry.id)
            case let .messageUpdate(_, entryId, inner):
                if !openMessages.contains(entryId) { updatesOutsideAMessage += 1 }
                switch inner {
                case let .textDelta(_, delta, _): deltas[entryId, default: ""] += delta
                case let .done(_, message): terminal[entryId] = message
                case let .error(_, message): terminal[entryId] = message
                default: break
                }
            case let .messageEnd(_, entry):
                guard entry.message?.asAssistant != nil else { break }
                order.append("message_end")
                openMessages.remove(entry.id)
            case .turnEnd: order.append("turn_end")
            case let .runEnd(_, _, result, _, _):
                order.append("run_end")
                outcome = result
                break loop
            default: break
            }
        }

        #expect(order.first == "run_start")
        #expect(order.contains("turn_start"))
        #expect(order.contains("message_start"))
        #expect(order.last == "run_end")
        #expect(order.filter { $0 == "run_end" }.count == 1, "exactly one terminal run_end")
        #expect(updatesOutsideAMessage == 0, "a message_update outside its message brackets")
        #expect(
            [RunOutcome.completed, .aborted, .failed].contains(outcome ?? .failed),
            "run ended with \(String(describing: outcome))")
        #expect(!deltas.isEmpty, "the run should stream text deltas")

        // A failing turn is cut off mid-prose and never sends `text_end`, so the deltas are a
        // prefix of the terminal message rather than equal to it.
        for (entryId, text) in deltas {
            let final = try #require(terminal[entryId]?.text)
            #expect(final.hasPrefix(text) || final == text, "deltas diverged from the message")
        }

        await client.shutdown()
    }

    /// Spec 07 §6: the reconstructed transcript equals the terminal `partial`.
    @Test("the store's transcript equals the message the core finished with")
    func transcriptMatchesTerminalMessage() async throws {
        let (client, dir) = try makeClient()
        defer { try? FileManager.default.removeItem(at: dir) }

        let stores = await CoreStores(client: client)
        try await stores.start()
        let session = try await makeSession(client)
        await MainActor.run { stores.sessions.selectedSessionId = session.id }
        await stores.chat.load(sessionId: session.id)

        try await stores.chat.send("Add a health check endpoint")

        let deadline = ContinuousClock.now.advanced(by: .seconds(30))
        while await MainActor.run(body: { stores.chat.lastRun == nil }),
            ContinuousClock.now < deadline
        {
            try await Task.sleep(for: .milliseconds(20))
        }

        await MainActor.run {
            #expect(stores.chat.lastRun != nil, "the run never ended")
            #expect(stores.chat.isStreaming == false)
            #expect(
                stores.chat.reconciliationRepairs == 0,
                "the incremental transcript disagreed with the core's `partial`")

            // What the deltas built must be what the core said it produced.
            let assistant = stores.chat.messages.compactMap { $0.message.asAssistant }.last
            #expect(assistant?.text.isEmpty == false)
            #expect(stores.chat.messages.first?.message.asUser != nil, "the prompt is in the log")
            #expect(stores.chat.turns.isEmpty == false, "a turn footer was recorded")
            #expect(
                stores.chat.toolRuns.values.allSatisfy { !$0.isRunning },
                "every tool run closed")
            #expect(stores.diagnostics.isHealthy, "no events were dropped or undecodable")
        }

        await stores.shutdown()
    }

    @Test("context usage is computed from the real transcript")
    func reportsContextUsage() async throws {
        let (client, dir) = try makeClient()
        defer { try? FileManager.default.removeItem(at: dir) }

        try await client.start()
        let session = try await makeSession(client)

        let usage = try await client.query(GetContextUsage(sessionId: session.id))
        #expect(usage.total > 0, "the model's context window should be known")
        #expect(usage.segments.count == 5, "every segment kind should be reported")
        #expect(usage.fraction >= 0 && usage.fraction <= 1)
        #expect(usage.segments.map(\.kind).contains(.outputReserve))

        await client.shutdown()
    }

    @Test("every query the app makes at launch answers or says why not")
    func launchQueries() async throws {
        let (client, dir) = try makeClient()
        defer { try? FileManager.default.removeItem(at: dir) }
        try await client.start()

        let settings = try await client.query(GetSettings())
        #expect(settings.version >= 1)

        let catalog = try await client.query(GetCatalog())
        #expect(catalog.providers.isEmpty == false)
        #expect(catalog.model(settings.defaults.modelRef) != nil)

        let stats = try await client.query(GetStats(range: .d7, tz: "UTC"))
        #expect(stats.hourly.count == 24)

        let markdown = try await client.query(
            RenderMarkdown(text: "hello\n\n```rust\nfn main() {}\n```", complete: true))
        #expect(markdown.blocks.count == 2)

        await client.shutdown()
    }

    /// Spec 07 §6: free-while-streaming does not crash or hang — the Swift-side mirror of
    /// the Rust test.
    @Test("freeing the core mid-stream neither crashes nor hangs", .timeLimit(.minutes(1)))
    func freeWhileStreaming() async throws {
        for _ in 0..<5 {
            let (client, dir) = try makeClient()
            defer { try? FileManager.default.removeItem(at: dir) }

            try await client.start()
            let session = try await makeSession(client)
            try await client.dispatch(.sendPrompt(sessionId: session.id, text: "stream please"))

            // Let the run get properly underway, then pull the handle out from under it.
            var seen = 0
            for await _ in client.events {
                seen += 1
                if seen >= 3 { break }
            }
            #expect(seen >= 3)

            await client.shutdown()
            // A second shutdown must be a no-op, not a double free.
            await client.shutdown()
        }
    }

    @Test("an ABI mismatch is refused with a diagnosable error")
    func abiMismatch() {
        let stale = StaleTransport()
        #expect(throws: TransportError.self) {
            _ = try CoreClient(transport: stale)
        }
    }
}

/// A transport claiming an ABI this build does not speak.
private final class StaleTransport: CoreTransport, @unchecked Sendable {
    let abiVersion: UInt32 = formABIVersion + 99
    func query(_ json: String) throws -> String { "{}" }
    func dispatch(_ json: String) throws -> String { "{}" }
    func subscribe(_ handler: @escaping @Sendable (String) -> Void) throws -> Int32 { 1 }
    func unsubscribe(_ token: Int32) {}
    func shutdown() {}
}
