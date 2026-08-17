import Foundation
import Testing

@testable import FormCore

/// The reconciliation rule is the thing worth testing here (spec 07 §4): deltas build the
/// transcript, `partial` audits it, and a disagreement is repaired rather than rendered.
@MainActor
@Suite("ChatStore")
struct ChatStoreTests {

    private func store(sessionId: String = "ses_test") -> ChatStore {
        let store = ChatStore(client: CoreClient(mock: MockTransport(replaysRuns: false)))
        let summary = SessionSummary(
            id: sessionId, title: "test",
            modelRef: ModelRef(
                providerId: "anthropic", modelId: "claude-opus-5", thinkingLevel: .high))
        store.seed(Session(summary: summary, entries: []))
        return store
    }

    @Test("a recorded run reconstructs exactly the message the core finished with")
    func deltasReconstructTheTerminalMessage() throws {
        let chat = store()
        let model = ModelRef(
            providerId: "anthropic", modelId: "claude-opus-5", thinkingLevel: .high)
        let log = MockCorpus.recordedRun(
            sessionId: "ses_test", prompt: "Add a health check endpoint", model: model)

        for recorded in log { chat.apply(recorded.event) }

        // The terminal `done` carries the message the core believes it produced.
        let terminal = log.compactMap { recorded -> AssistantMessage? in
            guard case let .messageUpdate(_, _, event) = recorded.event.kind else { return nil }
            return event.terminalMessage
        }.last
        let expected = try #require(terminal)

        let assistant = try #require(
            chat.messages.compactMap { $0.message.asAssistant }.last)
        #expect(assistant.content == expected.content)
        #expect(assistant.text == expected.text)
        #expect(assistant.thinking == expected.thinking)
        #expect(assistant.toolCalls.map(\.id) == expected.toolCalls.map(\.id))
        #expect(chat.reconciliationRepairs == 0, "the deltas disagreed with `partial`")
    }

    @Test("a run's side effects land: user message, tool run, turn footer, run record")
    func runSideEffects() throws {
        let chat = store()
        let model = ModelRef(
            providerId: "anthropic", modelId: "claude-opus-5", thinkingLevel: .high)
        for recorded in MockCorpus.recordedRun(
            sessionId: "ses_test", prompt: "Add a health check", model: model)
        {
            chat.apply(recorded.event)
        }

        #expect(chat.messages.first?.message.asUser?.content.plainText == "Add a health check")
        #expect(chat.isStreaming == false)
        #expect(chat.lastRun?.outcome == .completed)
        #expect(chat.turns.count == 1)
        #expect(chat.turns.first?.usage.totalTokens == 1_686)

        let tool = try #require(chat.toolRuns.values.first)
        #expect(tool.name == "read")
        #expect(tool.isRunning == false)
        #expect(tool.linesAdded == 268)
        #expect((tool.durationMs ?? 0) > 0)
    }

    @Test("streaming state opens and closes with the run")
    func streamingState() {
        let chat = store()
        chat.apply(
            CoreEvent(kind: .runStart(sessionId: "ses_test", runId: "run_1")))
        #expect(chat.isStreaming)
        chat.apply(
            CoreEvent(
                kind: .runEnd(
                    sessionId: "ses_test", runId: "run_1", outcome: .aborted, usage: .zero,
                    durationMs: 12)))
        #expect(chat.isStreaming == false)
        #expect(chat.lastRun?.outcome == .aborted)
    }

    @Test("events for another session are ignored")
    func ignoresOtherSessions() {
        let chat = store()
        chat.apply(CoreEvent(kind: .runStart(sessionId: "ses_other", runId: "run_1")))
        #expect(chat.isStreaming == false)
    }

    @Test("a partial that disagrees with the deltas is repaired")
    func repairsDrift() throws {
        let previous = ChatStore.assertsOnReconciliationDrift
        ChatStore.assertsOnReconciliationDrift = false
        defer { ChatStore.assertsOnReconciliationDrift = previous }

        let chat = store()
        let base = AssistantMessage(
            api: "anthropic-messages", provider: "anthropic", model: "claude-opus-5",
            timestamp: 1)
        let entry = Entry(
            id: "ent_1", sessionId: "ses_test", seq: 0, timestamp: 1,
            kind: .message(message: .assistant(base)))
        chat.apply(CoreEvent(kind: .messageStart(sessionId: "ses_test", entry: entry)))

        var partial = base
        partial.content = [.text(TextContent(text: "hello"))]
        chat.apply(
            CoreEvent(
                kind: .messageUpdate(
                    sessionId: "ses_test", entryId: "ent_1",
                    event: .textStart(contentIndex: 0, partial: partial))))

        // The core says the block already holds "hello"; the deltas built "".
        #expect(chat.reconciliationRepairs == 1)
        #expect(chat.messages.last?.message.asAssistant?.text == "hello")
    }

    @Test("a prompt sent during a run is queued and released at the run boundary")
    func queuesWhileStreaming() async throws {
        let transport = MockTransport(replaysRuns: false)
        let chat = ChatStore(client: CoreClient(mock: transport))
        let summary = SessionSummary(
            id: "ses_test", title: "t",
            modelRef: ModelRef(providerId: "anthropic", modelId: "m", thinkingLevel: .off))
        chat.seed(Session(summary: summary, entries: []))

        chat.apply(CoreEvent(kind: .runStart(sessionId: "ses_test", runId: "run_1")))
        try await chat.send("second thought")
        #expect(chat.queued == ["second thought"])
        #expect(transport.commands.isEmpty, "a queued prompt must not reach the core yet")

        chat.apply(
            CoreEvent(
                kind: .runEnd(
                    sessionId: "ses_test", runId: "run_1", outcome: .completed, usage: .zero,
                    durationMs: 1)))
        #expect(chat.queued.isEmpty)

        // The release is a detached dispatch; give it a turn of the loop.
        try await Task.sleep(for: .milliseconds(120))
        #expect(transport.commands.contains(.sendPrompt(sessionId: "ses_test", text: "second thought")))
    }

    @Test("blocks arriving out of order do not crash the store")
    func toleratesSkippedIndices() {
        let chat = store()
        let base = AssistantMessage(api: "a", provider: "p", model: "m", timestamp: 1)
        let entry = Entry(
            id: "ent_1", sessionId: "ses_test", seq: 0, timestamp: 1,
            kind: .message(message: .assistant(base)))
        chat.apply(CoreEvent(kind: .messageStart(sessionId: "ses_test", entry: entry)))

        var partial = base
        partial.content = [
            .text(TextContent(text: "")), .text(TextContent(text: "")),
            .text(TextContent(text: "late")),
        ]
        ChatStore.assertsOnReconciliationDrift = false
        chat.apply(
            CoreEvent(
                kind: .messageUpdate(
                    sessionId: "ses_test", entryId: "ent_1",
                    event: .textDelta(contentIndex: 2, delta: "late", partial: partial))))
        ChatStore.assertsOnReconciliationDrift = true

        #expect(chat.messages.last?.message.asAssistant?.content.count == 3)
    }
}
