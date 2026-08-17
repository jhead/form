import Foundation
import Testing

@testable import FormCore
@testable import FormUI

/// W10's pure derivations: the strings the reference specifies, the tool-group phrasing
/// rules in F1.3, the transcript grouping in spec 10 §1, the markdown debounce's
/// block-boundary rule in spec 10 §2, and the 40 pt scroll pin in the same section.
///
/// The views themselves are covered by `#Preview`s and by the harness runs recorded in the
/// W10 report; what is testable without a render loop is here.
struct ChatFormatTests {
    @Test("token counts read the way the reference prints them")
    func compactTokens() {
        #expect(ChatFormat.compact(0) == "0")
        #expect(ChatFormat.compact(486) == "486")
        #expect(ChatFormat.compact(5_900) == "5.9k")
        #expect(ChatFormat.compact(1_686) == "1.7k")
        #expect(ChatFormat.compact(18_420) == "18k")
        #expect(ChatFormat.compact(1_240_000) == "1.2M")
    }

    @Test("durations read as elapsed wall time")
    func durations() {
        #expect(ChatFormat.duration(840) == "840ms")
        #expect(ChatFormat.duration(1_400) == "1.4s")
        #expect(ChatFormat.duration(2_000) == "2s")
        #expect(ChatFormat.duration(211_000) == "3m 31s")
        #expect(ChatFormat.duration(3_840_000) == "1h 04m")
        #expect(ChatFormat.duration(-5) == "0ms")
    }

    @Test("diff counts carry their sign")
    func diffs() {
        let text = ChatFormat.diff(added: 268, removed: 0)
        #expect(text.added == "+268")
        #expect(text.removed == "-0")
    }

    @Test("cost keeps sub-cent amounts honest")
    func costs() {
        #expect(ChatFormat.cost(0) == "$0.00")
        #expect(ChatFormat.cost(0.42) == "$0.42")
        #expect(ChatFormat.cost(0.0019) == "$0.0019")
    }

    @Test("relative timestamps")
    func relative() {
        let now = Date(timeIntervalSince1970: 1_755_000_000)
        func at(_ secondsAgo: Double) -> TimestampMs {
            Int64((now.timeIntervalSince1970 - secondsAgo) * 1_000)
        }
        #expect(ChatFormat.relative(at(5), now: now) == "just now")
        #expect(ChatFormat.relative(at(720), now: now) == "12m ago")
        #expect(ChatFormat.relative(at(10_800), now: now) == "3h ago")
        #expect(ChatFormat.relative(at(100_000), now: now) == "yesterday")
    }
}

struct ToolGroupSummaryTests {
    @Test("the reference's phrase falls out of names and counts")
    func referencePhrase() {
        // `Ran 5 commands, used a tool ›` — spec 10 §4.
        let phrase = ToolGroupSummary.phrase(
            for: ["bash", "bash", "bash", "bash", "bash", "web_fetch"], running: false)
        #expect(phrase == "Ran 5 commands, fetched 1 page")

        #expect(ToolGroupSummary.phrase(for: ["read", "read"], running: false) == "Read 2 files")
        #expect(ToolGroupSummary.phrase(for: ["write"], running: false) == "Created 1 file")
        #expect(ToolGroupSummary.phrase(for: ["grep"], running: true) == "Searching")
    }

    @Test("an unknown tool still reads as English")
    func unknownTools() {
        #expect(ToolGroupSummary.phrase(for: ["mcp__weird"], running: false) == "Used a tool")
        #expect(
            ToolGroupSummary.phrase(for: ["mcp__a", "mcp__b"], running: false) == "Used 2 tools")
    }

    @Test("past two categories the tail collapses into the generic clause")
    func manyCategories() {
        let phrase = ToolGroupSummary.phrase(
            for: ["read", "read", "read", "bash", "grep", "write"], running: false)
        #expect(phrase == "Read 3 files, used 3 tools")
    }

    @Test("a live group uses the present participle")
    func running() {
        #expect(ToolGroupSummary.phrase(for: ["bash"], running: true) == "Running 1 command")
        #expect(ToolGroupSummary.phrase(for: ["edit", "edit"], running: true) == "Editing 2 files")
    }

    @Test("diff counts sum across the group and come from linesAdded/linesRemoved")
    func diffCounts() {
        let calls = [
            display(name: "edit", added: 12, removed: 3),
            display(name: "write", added: 268, removed: 0),
        ]
        let summary = ToolGroupSummary(calls)
        #expect(summary.linesAdded == 280)
        #expect(summary.linesRemoved == 3)
        #expect(summary.hasDiff)
        #expect(!summary.isRunning)
    }

    private func display(name: String, added: Int64, removed: Int64) -> ToolCallDisplay {
        var run = ToolRun(
            id: "t_\(name)", name: name, args: .object([:]), startedAt: 0)
        run.result = .object(["linesAdded": .int(added), "linesRemoved": .int(removed)])
        run.endedAt = 1_000
        return ToolCallDisplay(call: ToolCall(id: run.id, name: name), run: run, result: nil)
    }
}

@MainActor
struct TranscriptBuilderTests {
    @Test("tool results fold into the group and never render as their own row")
    func grouping() {
        let items = TranscriptBuilder.items(
            entries: Fixtures.turn(),
            toolRuns: [:],
            turns: [],
            streamingEntryId: nil,
            queued: [],
            showsFooters: true)

        let kinds = items.map(\.id)
        #expect(kinds.contains { $0.hasPrefix("u:") })
        #expect(kinds.contains { $0.hasPrefix("a:") })
        #expect(kinds.contains { $0.hasPrefix("t:") })
        // The `toolResult` entry has no row of its own.
        #expect(items.count == 4)  // user, assistant, tools, footer

        guard case let .tools(_, calls) = items[2] else {
            Issue.record("expected a tool group")
            return
        }
        #expect(calls.count == 1)
        #expect(calls[0].linesAdded == 268)
        #expect(!calls[0].isRunning)
    }

    @Test("a footer falls back to the message's own usage when no turn record exists")
    func footerFallback() {
        let items = TranscriptBuilder.items(
            entries: Fixtures.turn(), toolRuns: [:], turns: [], streamingEntryId: nil,
            queued: [], showsFooters: true)
        guard case let .footer(_, model) = items[3] else {
            Issue.record("expected a footer")
            return
        }
        #expect(model.durationMs == nil)
        #expect(model.totalTokens == 1_686)
        #expect(model.note == nil)
    }

    @Test("a live turn record wins, and carries the duration")
    func footerFromRecord() {
        let record = TurnRecord(
            id: "run#0", runId: "run", usage: Usage(totalTokens: 5_900), endedAt: 0,
            durationMs: 211_000)
        let items = TranscriptBuilder.items(
            entries: Fixtures.turn(), toolRuns: [:], turns: [record], streamingEntryId: nil,
            queued: [], showsFooters: true)
        guard case let .footer(_, model) = items[3] else {
            Issue.record("expected a footer")
            return
        }
        #expect(model.durationMs == 211_000)
        #expect(model.totalTokens == 5_900)
    }

    @Test("a streaming message has no footer yet")
    func noFooterWhileStreaming() {
        let entries = Fixtures.turn()
        let items = TranscriptBuilder.items(
            entries: entries, toolRuns: [:], turns: [], streamingEntryId: entries[1].id,
            queued: [], showsFooters: true)
        #expect(!items.contains { $0.id.hasPrefix("f:") })
    }

    @Test("footers can be switched off from settings")
    func footersOff() {
        let items = TranscriptBuilder.items(
            entries: Fixtures.turn(), toolRuns: [:], turns: [], streamingEntryId: nil,
            queued: [], showsFooters: false)
        #expect(!items.contains { $0.id.hasPrefix("f:") })
    }

    @Test("an aborted turn says so; a failed one gets an inline error block")
    func outcomes() {
        var entries = Fixtures.turn()
        entries[1].kind = .message(
            message: .assistant(
                Fixtures.assistant(
                    stopReason: .error,
                    errorMessage: "overloaded_error: the model is temporarily overloaded")))
        let items = TranscriptBuilder.items(
            entries: entries, toolRuns: [:], turns: [], streamingEntryId: nil, queued: [],
            showsFooters: true)

        guard let error = items.first(where: { $0.id.hasPrefix("e:") }),
            case let .error(_, model, _) = error
        else {
            Issue.record("expected an inline error")
            return
        }
        #expect(model.code == "overloaded_error")
        #expect(model.message == "the model is temporarily overloaded")
    }

    @Test("an error message with no code prefix keeps the whole string")
    func uncodedError() {
        let model = InlineErrorModel(raw: "something went wrong: really")
        #expect(model.code == nil)
        #expect(model.message == "something went wrong: really")
    }

    @Test("a user message renders in both of its wire shapes")
    func userContentShapes() {
        let bare = UserMessageRow.parts(of: .text("read the router"))
        #expect(bare.notes.isEmpty)
        #expect(bare.prompt == "read the router")

        // With attachments the core sends blocks: images, then a line per file it could not
        // inline, then the prompt last (`Core::user_message`).
        let withAttachments = UserMessageRow.parts(
            of: .blocks([
                .image(ImageContent(data: "AAAA", mimeType: "image/png")),
                .text("[attached: notes.pdf (application/pdf, 284910 bytes)]"),
                .text("summarise these"),
            ]))
        #expect(withAttachments.notes == ["[attached: notes.pdf (application/pdf, 284910 bytes)]"])
        #expect(withAttachments.prompt == "summarise these")
    }

    @Test("queued prompts land after the transcript")
    func queued() {
        let items = TranscriptBuilder.items(
            entries: Fixtures.turn(), toolRuns: [:], turns: [], streamingEntryId: nil,
            queued: ["and add a test"], showsFooters: true)
        #expect(items.last?.id == "q:0")
    }

    enum Fixtures {
        static func assistant(
            stopReason: StopReason = .toolUse, errorMessage: String? = nil
        ) -> AssistantMessage {
            AssistantMessage(
                content: [
                    .thinking(ThinkingContent(thinking: "planning")),
                    .text(TextContent(text: "I'll read the router.")),
                    .toolCall(
                        ToolCall(
                            id: "toolu_1", name: "read",
                            arguments: ["path": .string("src/router.rs")])),
                ],
                api: "anthropic-messages", provider: "anthropic", model: "claude-opus-5",
                usage: Usage(totalTokens: 1_686), stopReason: stopReason,
                errorMessage: errorMessage, timestamp: 1_000)
        }

        static func turn() -> [Entry] {
            [
                Entry(
                    id: "e1", sessionId: "s", seq: 0, timestamp: 0,
                    kind: .message(message: .user(UserMessage(text: "read it", timestamp: 0)))),
                Entry(
                    id: "e2", sessionId: "s", seq: 1, timestamp: 1_000,
                    kind: .message(message: .assistant(assistant()))),
                Entry(
                    id: "e3", sessionId: "s", seq: 2, timestamp: 2_000,
                    kind: .message(
                        message: .toolResult(
                            ToolResultMessage(
                                toolCallId: "toolu_1", toolName: "read",
                                content: [.text("read 268 lines")],
                                details: .object([
                                    "linesAdded": .int(268), "linesRemoved": .int(0),
                                ]),
                                timestamp: 2_000)))),
            ]
        }
    }
}

@MainActor
struct MarkdownStreamTests {
    @Test("a blank line, a fence and a block marker all force an immediate reparse")
    func boundaries() {
        #expect(MarkdownStream.crossesBlockBoundary(from: "one", to: "one\n\n"))
        #expect(MarkdownStream.crossesBlockBoundary(from: "text\n", to: "text\n\n"))
        #expect(MarkdownStream.crossesBlockBoundary(from: "a\n", to: "a\n```rust"))
        #expect(MarkdownStream.crossesBlockBoundary(from: "a\n", to: "a\n# Heading"))
        #expect(MarkdownStream.crossesBlockBoundary(from: "a\n", to: "a\n- item"))
        #expect(MarkdownStream.crossesBlockBoundary(from: "a\n", to: "a\n1. item"))
        #expect(MarkdownStream.crossesBlockBoundary(from: "a\n", to: "a\n> quote"))
        #expect(MarkdownStream.crossesBlockBoundary(from: "a\n", to: "a\n| c |"))
    }

    @Test("ordinary word deltas do not, so the debounce actually debounces")
    func nonBoundaries() {
        #expect(!MarkdownStream.crossesBlockBoundary(from: "the quick", to: "the quick brown"))
        #expect(!MarkdownStream.crossesBlockBoundary(from: "line one\n", to: "line one\nmore"))
        #expect(!MarkdownStream.crossesBlockBoundary(from: "", to: "hello"))
    }

    @Test("a boundary split across two deltas is still caught")
    func splitBoundary() {
        // `"…one\n"` then `"\ntwo"` — the overlap window is what sees the pair.
        #expect(MarkdownStream.crossesBlockBoundary(from: "one\n", to: "one\n\ntwo"))
    }
}

@MainActor
struct TranscriptScrollTests {
    @Test("the pin threshold is 40 pt")
    func threshold() {
        var metrics = TranscriptMetrics(contentHeight: 2_000, offset: 1_260, viewportHeight: 700)
        #expect(metrics.distanceFromBottom == 40)
        #expect(metrics.isAtBottom)

        metrics.offset = 1_259
        #expect(!metrics.isAtBottom)
    }

    @Test("a few points of relayout clamp is not a scroll gesture")
    func clampIsNotIntent() {
        let state = TranscriptScrollState()
        state.route(to: "s")
        state.update(TranscriptMetrics(contentHeight: 2_000, offset: 0, viewportHeight: 700))
        state.update(TranscriptMetrics(contentHeight: 2_000, offset: 1_300, viewportHeight: 700))
        #expect(state.isPinned)

        // A long message re-lays out and the offset clamps back by a few points, twice.
        state.update(TranscriptMetrics(contentHeight: 2_100, offset: 1_294, viewportHeight: 700))
        state.update(TranscriptMetrics(contentHeight: 2_200, offset: 1_290, viewportHeight: 700))
        #expect(state.isPinned)
    }

    @Test("content growing under the viewport does not unpin the follow")
    func growthKeepsThePin() {
        let state = TranscriptScrollState()
        state.route(to: "s")
        state.update(TranscriptMetrics(contentHeight: 1_000, offset: 0, viewportHeight: 700))
        state.update(TranscriptMetrics(contentHeight: 1_000, offset: 300, viewportHeight: 700))
        #expect(state.isPinned)

        let before = state.scrollRequest
        // The tail grew; the offset has not moved yet.
        state.update(TranscriptMetrics(contentHeight: 1_400, offset: 300, viewportHeight: 700))
        #expect(state.isPinned)
        #expect(state.scrollRequest > before)
    }

    @Test("scrolling up unpins, and reaching the bottom re-arms")
    func userWins() {
        let state = TranscriptScrollState()
        state.route(to: "s")
        state.update(TranscriptMetrics(contentHeight: 2_000, offset: 0, viewportHeight: 700))
        state.update(TranscriptMetrics(contentHeight: 2_000, offset: 1_300, viewportHeight: 700))
        #expect(state.isPinned)

        state.update(TranscriptMetrics(contentHeight: 2_000, offset: 400, viewportHeight: 700))
        #expect(!state.isPinned)

        // A delta lands while they are reading history: no scroll.
        let before = state.scrollRequest
        state.update(TranscriptMetrics(contentHeight: 2_400, offset: 400, viewportHeight: 700))
        #expect(!state.isPinned)
        #expect(state.scrollRequest == before)

        state.jumpToLatest()
        #expect(state.isPinned)
        #expect(state.scrollRequest > before)
    }

    @Test("each session keeps its own pin state across a route change")
    func perSessionMemory() {
        let state = TranscriptScrollState()
        state.route(to: "a")
        state.update(TranscriptMetrics(contentHeight: 2_000, offset: 0, viewportHeight: 700))
        state.update(TranscriptMetrics(contentHeight: 2_000, offset: 1_300, viewportHeight: 700))
        state.update(TranscriptMetrics(contentHeight: 2_000, offset: 200, viewportHeight: 700))
        #expect(!state.isPinned)

        state.route(to: "b")
        #expect(state.isPinned)

        state.route(to: "a")
        #expect(!state.isPinned)
    }
}
