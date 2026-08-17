import Foundation

extension MockCorpus {
    /// A recorded run, event for event, in the shape and cadence the stub harness produces
    /// (spec 02): thinking, streamed markdown, a tool call whose arguments arrive as
    /// fragments, tool execution with progress, then the turn and run footers.
    ///
    /// `partial` is accumulated exactly as the core accumulates it, so a `ChatStore` driven
    /// by this log reconciles clean — a preview must not trip the drift assertion.
    public static func recordedRun(
        sessionId: String,
        prompt: String,
        model: ModelRef,
        commandId: CommandID = "cmd_mock",
        startingSeq: Int64 = 0,
        at start: TimestampMs = 1_755_000_000_000
    ) -> [RecordedEvent] {
        var events: [RecordedEvent] = []
        var clock = start
        let runId = "run_mock"

        func push(_ delay: Int, _ kind: CoreEventKind, command: CommandID? = commandId) {
            clock += TimestampMs(delay)
            events.append(
                RecordedEvent(
                    delayMs: delay,
                    event: CoreEvent(timestamp: clock, commandId: command, kind: kind)))
        }

        // The user's message is in the transcript before the run starts, so the composer can
        // clear immediately.
        let userEntry = Entry(
            id: "ent_mock_user", sessionId: sessionId, seq: startingSeq, timestamp: clock,
            kind: .message(message: .user(UserMessage(text: prompt, timestamp: clock))))
        push(0, .messageStart(sessionId: sessionId, entry: userEntry))
        push(0, .messageEnd(sessionId: sessionId, entry: userEntry))

        push(0, .runStart(sessionId: sessionId, runId: runId))
        push(0, .turnStart(sessionId: sessionId, runId: runId))

        var partial = AssistantMessage(
            api: "anthropic-messages", provider: model.providerId, model: model.modelId,
            timestamp: clock)
        let assistantId = "ent_mock_assistant"
        func entry(_ message: AssistantMessage) -> Entry {
            Entry(
                id: assistantId, sessionId: sessionId, seq: startingSeq + 1,
                parentId: userEntry.id, timestamp: clock,
                kind: .message(message: .assistant(message)))
        }
        push(0, .messageStart(sessionId: sessionId, entry: entry(partial)))

        func update(_ delay: Int, _ event: AssistantMessageEvent) {
            push(
                delay,
                .messageUpdate(sessionId: sessionId, entryId: assistantId, event: event))
        }

        update(0, .start(partial: partial))

        // --- thinking ---
        partial.content.append(.thinking(ThinkingContent(thinking: "")))
        let thinkingIndex = partial.content.count - 1
        update(420, .thinkingStart(contentIndex: thinkingIndex, partial: partial))
        for chunk in [
            "Looking at the request", " and the workspace", " to plan an approach.",
        ] {
            if case let .thinking(t) = partial.content[thinkingIndex] {
                partial.content[thinkingIndex] = .thinking(
                    ThinkingContent(thinking: t.thinking + chunk))
            }
            update(
                90, .thinkingDelta(contentIndex: thinkingIndex, delta: chunk, partial: partial))
        }
        let thinkingText = partial.content[thinkingIndex].asThinking?.thinking ?? ""
        update(
            60,
            .thinkingEnd(contentIndex: thinkingIndex, content: thinkingText, partial: partial))

        // --- text ---
        partial.content.append(.text(TextContent(text: "")))
        let textIndex = partial.content.count - 1
        update(0, .textStart(contentIndex: textIndex, partial: partial))
        for chunk in chunked(replyMarkdown, wordsPerChunk: 6) {
            if case let .text(t) = partial.content[textIndex] {
                partial.content[textIndex] = .text(TextContent(text: t.text + chunk))
            }
            update(28, .textDelta(contentIndex: textIndex, delta: chunk, partial: partial))
        }
        update(
            0, .textEnd(contentIndex: textIndex, content: replyMarkdown, partial: partial))

        // --- one tool call, so the collapsed tool group renders (F1.3) ---
        let toolCall = ToolCall(
            id: "toolu_mock_read", name: "read",
            arguments: ["path": .string("src/router.rs")])
        partial.content.append(.toolCall(toolCall))
        let toolIndex = partial.content.count - 1
        update(120, .toolCallStart(contentIndex: toolIndex, partial: partial))
        for fragment in ["{\"path\":", "\"src/", "router.rs\"}"] {
            update(30, .toolCallDelta(contentIndex: toolIndex, delta: fragment, partial: partial))
        }
        update(
            30, .toolCallEnd(contentIndex: toolIndex, toolCall: toolCall, partial: partial))

        let usage = Usage(
            input: 1_200, output: 486, cacheRead: 900, cacheWrite: 120, totalTokens: 1_686,
            cost: Cost(
                input: 0.006, output: 0.012, cacheRead: 0.0005, cacheWrite: 0.0008,
                total: 0.0193))
        partial.usage = usage
        partial.stopReason = .toolUse
        update(0, .done(reason: .toolUse, message: partial))

        // --- tool execution ---
        push(
            0,
            .toolExecutionStart(
                sessionId: sessionId, toolCallId: toolCall.id, toolName: toolCall.name,
                args: .object(["path": .string("src/router.rs")])))
        for progress in [0.33, 0.66, 1.0] {
            push(
                160,
                .toolExecutionUpdate(
                    sessionId: sessionId, toolCallId: toolCall.id,
                    partialResult: .object(["progress": .double(progress)])))
        }
        push(
            60,
            .toolExecutionEnd(
                sessionId: sessionId, toolCallId: toolCall.id,
                result: .object([
                    "linesAdded": .int(268), "linesRemoved": .int(0),
                    "text": .string("read 268 lines"),
                ]),
                isError: false))

        push(0, .messageEnd(sessionId: sessionId, entry: entry(partial)))
        push(0, .turnEnd(sessionId: sessionId, runId: runId, usage: usage))
        push(
            0,
            .runEnd(
                sessionId: sessionId, runId: runId, outcome: .completed, usage: usage,
                durationMs: clock - start))
        push(0, .statsInvalidated, command: nil)

        return events
    }

    /// Splits on whitespace boundaries so deltas land like real token chunks.
    static func chunked(_ text: String, wordsPerChunk: Int) -> [String] {
        var out: [String] = []
        var current = ""
        var count = 0
        var word = ""
        for character in text {
            word.append(character)
            if character.isWhitespace {
                current += word
                word = ""
                count += 1
                if count >= wordsPerChunk {
                    out.append(current)
                    current = ""
                    count = 0
                }
            }
        }
        current += word
        if !current.isEmpty { out.append(current) }
        return out
    }
}
