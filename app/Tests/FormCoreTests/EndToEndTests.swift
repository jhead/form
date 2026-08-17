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
            harnessSpeed: 40
        )
        return (try CoreClient(config: config), dir)
    }

    @Test("a prompt streams a full, correctly ordered run")
    func streamsARun() async throws {
        let (client, dir) = try makeClient()
        defer { try? FileManager.default.removeItem(at: dir) }

        try await client.start()
        try await client.dispatch(.createSession())

        let sessions = try await client.query(ListSessions())
        let session = try #require(sessions.sessions.first)

        try await client.dispatch(.sendPrompt(sessionId: session.id, text: "Add a health check"))

        var order: [String] = []
        var text = ""
        var sawThinking = false
        var toolNames: [String] = []
        var outcome: String?

        loop: for await event in await client.events {
            switch event {
            case .runStart: order.append("run_start")
            case .turnStart: order.append("turn_start")
            case let .messageUpdate(_, _, inner):
                switch inner {
                case let .textDelta(_, delta): text += delta
                case .thinkingDelta: sawThinking = true
                case let .toolCallEnd(_, name): toolNames.append(name)
                default: break
                }
            case let .toolExecutionStart(_, _, name): order.append("tool:\(name)")
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
        #expect(order.last == "run_end")
        #expect(outcome == "completed")
        #expect(sawThinking, "the run should stream a thinking block")
        #expect(!text.isEmpty, "the run should stream text deltas")
        #expect(toolNames.contains("read"), "the run should emit a tool call")

        await client.shutdown()
    }

    @Test("context usage is computed from the real transcript")
    func reportsContextUsage() async throws {
        let (client, dir) = try makeClient()
        defer { try? FileManager.default.removeItem(at: dir) }

        try await client.start()
        try await client.dispatch(.createSession())
        let session = try #require(try await client.query(ListSessions()).sessions.first)

        let usage = try await client.query(GetContextUsage(sessionId: session.id))
        #expect(usage.total > 0, "the model's context window should be known")
        #expect(usage.segments.count == 5, "every segment kind should be reported")
        #expect(usage.fraction >= 0 && usage.fraction <= 1)

        await client.shutdown()
    }

    @Test("an unknown event type decodes instead of throwing")
    func toleratesNewerCore() throws {
        let json = #"{"type":"something_new_from_a_newer_core","timestamp":1}"#
        let event = try JSONDecoder().decode(CoreEvent.self, from: Data(json.utf8))
        guard case let .unknown(type) = event else {
            Issue.record("expected .unknown, got \(event)")
            return
        }
        #expect(type == "something_new_from_a_newer_core")
    }
}
