import Foundation

/// One tool invocation, from `tool_execution_start` to `tool_execution_end` (F1.3, F6.2).
public struct ToolRun: Sendable, Equatable, Identifiable {
    public var id: String
    public var name: String
    public var args: JSONValue
    public var partialResult: JSONValue?
    public var result: JSONValue?
    public var isError: Bool = false
    public var startedAt: TimestampMs
    public var endedAt: TimestampMs?

    public var isRunning: Bool { endedAt == nil }
    public var durationMs: Int64? { endedAt.map { $0 - startedAt } }

    /// `0…1` where the stub reports it, `nil` for an indeterminate tool.
    public var progress: Double? { partialResult?["progress"]?.doubleValue }

    /// Diff counts for a file-mutating call (F1.3).
    public var linesAdded: Int64? { result?["linesAdded"]?.intValue }
    public var linesRemoved: Int64? { result?["linesRemoved"]?.intValue }
}

/// A completed turn, for the transcript footer: `3m 31s · 5.9k tokens` (F1.4).
public struct TurnRecord: Sendable, Equatable, Identifiable {
    public var id: String
    public var runId: String
    public var usage: Usage
    public var endedAt: TimestampMs
    public var durationMs: Int64
}

public struct RunRecord: Sendable, Equatable {
    public var runId: String
    public var outcome: RunOutcome
    public var usage: Usage
    public var durationMs: Int64
}

/// The transcript for one session.
///
/// **Reconciliation rule (spec 07 §4).** Deltas are applied incrementally — a `text_delta`
/// appends to one string in one content block, and nothing else in the transcript is
/// touched. Every non-terminal event also carries `partial`, the message as the core
/// believes it stands; that is compared against what the deltas built, and any disagreement
/// is repaired by adopting `partial`. In debug the disagreement also trips an assertion,
/// because it means the delta logic here and the core have diverged and the app would go on
/// rendering a lie. The transcript is never rebuilt from scratch per event.
@MainActor
@Observable
public final class ChatStore {
    public private(set) var sessionId: String?
    public private(set) var entries: [Entry] = []
    public private(set) var toolRuns: [String: ToolRun] = [:]
    public private(set) var turns: [TurnRecord] = []
    public private(set) var lastRun: RunRecord?
    public private(set) var contextUsage: ContextUsage?
    public private(set) var isLoaded = false

    public private(set) var isStreaming = false
    public private(set) var streamingEntryId: String?
    /// Partial tool-call arguments, keyed by content index — the fragments a `toolcall_delta`
    /// carries before the call is complete, so the row can render growing JSON (spec 02).
    public private(set) var partialToolArguments: [Int: String] = [:]

    /// Prompts typed while a run was in flight; sent at the next turn boundary (F1.7).
    public private(set) var queued: [String] = []
    /// Mirrors `settings.defaults.queueMode`; `CoreStores` keeps it current so the composer
    /// and the preference never disagree.
    public var queueMode: QueueMode = .queue

    /// How many times `partial` disagreed with the locally-applied deltas. Zero is the
    /// expected value; anything else is drift worth reporting.
    public private(set) var reconciliationRepairs = 0

    /// Debug builds trip an assertion on drift. Tests that exercise the repair path turn it
    /// off; nothing else should.
    public static var assertsOnReconciliationDrift = true

    @ObservationIgnored private let client: CoreClient
    @ObservationIgnored private var streamingIndex: Int?
    @ObservationIgnored private var runStartedAt: TimestampMs?
    @ObservationIgnored private var turnStartedAt: TimestampMs?

    public init(client: CoreClient) {
        self.client = client
    }

    /// For previews: a transcript with no core behind it.
    public init(client: CoreClient, session: Session) {
        self.client = client
        sessionId = session.id
        entries = session.entries
        isLoaded = true
    }

    /// Preview seeding — synchronous, so a `#Preview` renders a full transcript on the
    /// first pass rather than after a query resolves.
    func seed(_ session: Session, streaming: Bool = false, usage: ContextUsage? = nil) {
        reset(to: session.id)
        entries = session.entries
        contextUsage = usage
        isLoaded = true
        if streaming, let last = entries.last, last.message?.asAssistant != nil {
            isStreaming = true
            streamingEntryId = last.id
            streamingIndex = entries.count - 1
        }
    }

    // MARK: - Loading

    public func load(sessionId: String) async {
        guard self.sessionId != sessionId else { return }
        reset(to: sessionId)
        do {
            let session = try await client.query(GetSession(sessionId: sessionId))
            // The user may have moved on, and a run may have streamed entries in while the
            // query was in flight — the fetched transcript is the base, anything streamed
            // since is fresher and wins.
            guard self.sessionId == sessionId else { return }
            let streamed = entries
            entries = session.entries
            for entry in streamed { upsert(entry) }
            isLoaded = true
        } catch {
            Log.stores.error(
                "getSession failed: \(String(describing: error), privacy: .public)")
        }
        await refreshContextUsage()
    }

    public func refreshContextUsage() async {
        guard let sessionId else { return }
        contextUsage = try? await client.query(GetContextUsage(sessionId: sessionId))
    }

    private func reset(to sessionId: String?) {
        self.sessionId = sessionId
        entries = []
        toolRuns = [:]
        turns = []
        lastRun = nil
        contextUsage = nil
        queued = []
        isStreaming = false
        streamingEntryId = nil
        streamingIndex = nil
        partialToolArguments = [:]
        isLoaded = false
    }

    // MARK: - Events

    public func apply(_ event: CoreEvent) {
        // Everything below is for the session on screen; other sessions' traffic belongs to
        // `SessionStore`, which keeps their summaries current.
        guard let sessionId, event.sessionId == sessionId else { return }

        switch event.kind {
        case let .runStart(_, runId):
            isStreaming = true
            runStartedAt = event.timestamp
            lastRun = nil
            Log.events.debug("run \(runId, privacy: .public) started")

        case .turnStart:
            turnStartedAt = event.timestamp

        case let .messageStart(_, entry):
            upsert(entry)
            if entry.message?.asAssistant != nil {
                streamingEntryId = entry.id
                streamingIndex = entries.firstIndex { $0.id == entry.id }
                partialToolArguments = [:]
            }

        case let .messageUpdate(_, entryId, inner):
            applyMessageEvent(inner, to: entryId)

        case let .messageEnd(_, entry):
            upsert(entry)
            if entry.id == streamingEntryId {
                streamingEntryId = nil
                streamingIndex = nil
                partialToolArguments = [:]
            }

        case let .toolExecutionStart(_, toolCallId, toolName, args):
            toolRuns[toolCallId] = ToolRun(
                id: toolCallId, name: toolName, args: args, startedAt: event.timestamp)

        case let .toolExecutionUpdate(_, toolCallId, partialResult):
            toolRuns[toolCallId]?.partialResult = partialResult

        case let .toolExecutionEnd(_, toolCallId, result, isError):
            toolRuns[toolCallId]?.result = result
            toolRuns[toolCallId]?.isError = isError
            toolRuns[toolCallId]?.endedAt = event.timestamp

        case let .turnEnd(_, runId, usage):
            turns.append(
                TurnRecord(
                    id: "\(runId)#\(turns.count)",
                    runId: runId,
                    usage: usage,
                    endedAt: event.timestamp,
                    durationMs: event.timestamp - (turnStartedAt ?? event.timestamp)
                ))
            turnStartedAt = nil

        case let .runEnd(_, runId, outcome, usage, durationMs):
            isStreaming = false
            streamingEntryId = nil
            streamingIndex = nil
            runStartedAt = nil
            lastRun = RunRecord(
                runId: runId, outcome: outcome, usage: usage, durationMs: durationMs)
            drainQueue()

        case let .contextUsageChanged(usage):
            contextUsage = usage

        default:
            break
        }
    }

    private func upsert(_ entry: Entry) {
        if let i = entries.firstIndex(where: { $0.id == entry.id }) {
            entries[i] = entry
        } else {
            entries.append(entry)
        }
        isLoaded = true
    }

    // MARK: - Incremental application

    private func applyMessageEvent(_ event: AssistantMessageEvent, to entryId: String) {
        guard let index = assistantIndex(of: entryId) else { return }

        if let terminal = event.terminalMessage {
            // The terminal message is authoritative — adopt it whole rather than diffing.
            setAssistant(terminal, at: index)
            partialToolArguments = [:]
            return
        }

        switch event {
        case let .textStart(i, _):
            ensureBlock(at: i, index: index) { .text(TextContent(text: "")) }
        case let .textDelta(i, delta, _):
            ensureBlock(at: i, index: index) { .text(TextContent(text: "")) }
            mutate(at: index) { message in
                if case let .text(t) = message.content[i] {
                    message.content[i] = .text(TextContent(text: t.text + delta,
                                                           textSignature: t.textSignature))
                }
            }
        case let .textEnd(i, content, _):
            ensureBlock(at: i, index: index) { .text(TextContent(text: "")) }
            mutate(at: index) { message in
                if case let .text(t) = message.content[i] {
                    message.content[i] = .text(
                        TextContent(text: content, textSignature: t.textSignature))
                }
            }
        case let .thinkingStart(i, _):
            ensureBlock(at: i, index: index) { .thinking(ThinkingContent(thinking: "")) }
        case let .thinkingDelta(i, delta, _):
            ensureBlock(at: i, index: index) { .thinking(ThinkingContent(thinking: "")) }
            mutate(at: index) { message in
                if case let .thinking(t) = message.content[i] {
                    message.content[i] = .thinking(
                        ThinkingContent(
                            thinking: t.thinking + delta,
                            thinkingSignature: t.thinkingSignature, redacted: t.redacted))
                }
            }
        case let .thinkingEnd(i, content, _):
            ensureBlock(at: i, index: index) { .thinking(ThinkingContent(thinking: "")) }
            mutate(at: index) { message in
                if case let .thinking(t) = message.content[i] {
                    message.content[i] = .thinking(
                        ThinkingContent(
                            thinking: content, thinkingSignature: t.thinkingSignature,
                            redacted: t.redacted))
                }
            }
        case let .toolCallStart(i, partial):
            // The call's identity is only known at `toolcall_end`; until then the block is
            // whatever the core says it is, and the arguments accumulate as text.
            ensureBlock(at: i, index: index) {
                partial.content.indices.contains(i)
                    ? partial.content[i] : .toolCall(ToolCall(id: "", name: ""))
            }
            partialToolArguments[i] = ""
        case let .toolCallDelta(i, delta, partial):
            partialToolArguments[i, default: ""] += delta
            // The core salvages the arguments as soon as the accumulated fragments happen to
            // parse, so the block itself changes mid-stream. Adopting that one block keeps
            // this incremental while staying identical to `partial`.
            if partial.content.indices.contains(i) {
                ensureBlock(at: i, index: index) { partial.content[i] }
                mutate(at: index) { message in message.content[i] = partial.content[i] }
            }
        case let .toolCallEnd(i, toolCall, _):
            ensureBlock(at: i, index: index) { .toolCall(toolCall) }
            mutate(at: index) { message in message.content[i] = .toolCall(toolCall) }
            partialToolArguments[i] = nil
        default:
            break
        }

        if let partial = event.partial {
            reconcile(at: index, against: partial)
        }
    }

    /// Content blocks arrive by index and a `*_start` may be the first thing we see for a
    /// given index; pad rather than crash if the core skips one.
    private func ensureBlock(
        at contentIndex: Int, index: Int, _ make: () -> AssistantContent
    ) {
        mutate(at: index) { message in
            while message.content.count <= contentIndex {
                message.content.append(
                    message.content.count == contentIndex ? make() : .text(TextContent(text: "")))
            }
        }
    }

    private func mutate(at index: Int, _ body: (inout AssistantMessage) -> Void) {
        guard case let .message(message) = entries[index].kind,
            var assistant = message.asAssistant
        else { return }
        body(&assistant)
        entries[index].kind = .message(message: .assistant(assistant))
    }

    private func setAssistant(_ message: AssistantMessage, at index: Int) {
        entries[index].kind = .message(message: .assistant(message))
    }

    /// Assert-and-repair. Debug compares the whole content tree; release compares block count
    /// and the length of each block's text, which is cheap and catches every drift a delta
    /// bug can produce.
    private func reconcile(at index: Int, against partial: AssistantMessage) {
        guard case let .message(message) = entries[index].kind,
            let local = message.asAssistant
        else { return }

        #if DEBUG
            let drifted = local.content != partial.content
        #else
            let drifted = Self.shapeDiffers(local.content, partial.content)
        #endif
        guard drifted else { return }

        reconciliationRepairs += 1
        Log.stores.error(
            """
            transcript drifted from the core's partial at entry \
            \(self.entries[index].id, privacy: .public); repairing
            """)
        #if DEBUG
            assert(
                !Self.assertsOnReconciliationDrift,
                "ChatStore delta application disagrees with the core's `partial`")
        #endif
        setAssistant(partial, at: index)
    }

    private static func shapeDiffers(_ a: [AssistantContent], _ b: [AssistantContent]) -> Bool {
        guard a.count == b.count else { return true }
        for (x, y) in zip(a, b) {
            switch (x, y) {
            case let (.text(p), .text(q)) where p.text.utf8.count == q.text.utf8.count: continue
            case let (.thinking(p), .thinking(q))
            where p.thinking.utf8.count == q.thinking.utf8.count:
                continue
            case let (.toolCall(p), .toolCall(q)) where p.id == q.id: continue
            default: return true
            }
        }
        return false
    }

    private func assistantIndex(of entryId: String) -> Int? {
        if let i = streamingIndex, entries.indices.contains(i), entries[i].id == entryId {
            return i
        }
        let i = entries.firstIndex { $0.id == entryId }
        streamingIndex = i
        return i
    }

    // MARK: - Derived

    /// Messages in transcript order, paired with the entry that carries them.
    public var messages: [(entry: Entry, message: Message)] {
        entries.compactMap { entry in entry.message.map { (entry, $0) } }
    }

    public var streamingMessage: AssistantMessage? {
        guard let id = streamingEntryId else { return nil }
        return entries.first { $0.id == id }?.message?.asAssistant
    }

    /// The last assistant message, for `⌘⇧C` (F12).
    public var lastAssistantText: String? {
        messages.reversed().compactMap { $0.message.asAssistant }.first?.text
    }

    public func toolRun(for id: String) -> ToolRun? { toolRuns[id] }

    // MARK: - Commands

    /// Sends, or queues if a run is in flight (F1.7).
    public func send(_ text: String, attachmentIds: [String] = []) async throws {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, let sessionId else { return }
        guard !isStreaming else {
            // Either way the prompt is queued — a run cannot be replaced mid-flight, only
            // stopped and followed. `interrupt` just stops the current one first, and the
            // queue drains on `run_end`.
            queued.append(trimmed)
            if queueMode == .interrupt {
                try await client.dispatch(.abortRun(sessionId: sessionId))
            }
            return
        }
        try await client.dispatch(
            .sendPrompt(sessionId: sessionId, text: trimmed, attachmentIds: attachmentIds))
    }

    private func drainQueue() {
        guard !queued.isEmpty, let sessionId else { return }
        let next = queued.removeFirst()
        Task { [client] in
            try? await client.dispatch(.sendPrompt(sessionId: sessionId, text: next))
        }
    }

    public func removeQueued(at index: Int) {
        guard queued.indices.contains(index) else { return }
        queued.remove(at: index)
    }

    /// `Esc` and the composer's stop button (F1.6).
    public func abort() async throws {
        guard let sessionId, isStreaming else { return }
        try await client.dispatch(.abortRun(sessionId: sessionId))
    }

    public func retry(entryId: String) async throws {
        guard let sessionId else { return }
        try await client.dispatch(.retryMessage(sessionId: sessionId, entryId: entryId))
    }

    public func branch(fromEntryId entryId: String) async throws {
        guard let sessionId else { return }
        try await client.dispatch(.branchFromMessage(sessionId: sessionId, entryId: entryId))
    }

    public func find(_ q: String) async -> [SearchHit] {
        guard let sessionId, !q.isEmpty else { return [] }
        return (try? await client.query(SearchInSession(sessionId: sessionId, q: q))) ?? []
    }
}
