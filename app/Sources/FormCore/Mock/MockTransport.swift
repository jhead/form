import Foundation

/// An in-memory core, for SwiftUI previews and tests (spec 07 §1, §6).
///
/// It answers every query from `MockCorpus` and replays a recorded event log for
/// `sendPrompt`, on one serial queue, in order — the same delivery contract the real core
/// gives (spec 00 §7). **No Rust build is involved**, which is what lets every view in
/// W8–W14 have a working `#Preview`.
public final class MockTransport: CoreTransport, @unchecked Sendable {
    public let abiVersion = formABIVersion

    /// Multiplier on the recorded gaps. `0` replays as fast as the queue allows.
    public let speed: Double
    /// Replay `sendPrompt` as a live run. Off for a still preview, on for a moving one.
    public let replaysRuns: Bool

    private let queue = DispatchQueue(label: "dev.jhead.form.mock-events")
    private let lock = NSLock()

    private var corpus: MockCorpus
    private var listeners: [Int32: @Sendable (String) -> Void] = [:]
    private var nextToken: Int32 = 1
    private var isShutDown = false
    private var recorded: [CoreCommand] = []
    private var sequence = 0

    private let encoder: JSONEncoder = {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return encoder
    }()

    public init(corpus: MockCorpus = .demo, speed: Double = 1, replaysRuns: Bool = true) {
        self.corpus = corpus
        self.speed = speed
        self.replaysRuns = replaysRuns
    }

    /// Commands the app sent, in order — what a test asserts against.
    public var commands: [CoreCommand] { lock.withLock { recorded } }

    // MARK: - Queries

    public func query(_ json: String) throws -> String {
        let request = try JSONValue(jsonString: json)
        let type = request["type"]?.stringValue ?? ""
        let corpus = lock.withLock { self.corpus }

        switch type {
        case ListSessions.queryType:
            let includeArchived = request["includeArchived"]?.boolValue ?? false
            return try ok(corpus.list(includeArchived: includeArchived))
        case GetSession.queryType:
            let id = request["sessionId"]?.stringValue ?? ""
            guard let session = corpus.session(id) else {
                return error(code: "session_not_found", message: "no session \(id)")
            }
            return try ok(session)
        case GetSettings.queryType:
            return try ok(corpus.settings)
        case GetCatalog.queryType:
            return try ok(corpus.catalog)
        case GetStats.queryType:
            let range =
                request["range"]?.stringValue.flatMap(StatsRange.init(rawValue:)) ?? .d7
            return try ok(corpus.stats[range] ?? UsageStats(range: range))
        case GetContextUsage.queryType:
            let id = request["sessionId"]?.stringValue ?? ""
            return try ok(
                corpus.contextUsage[id]
                    ?? ContextUsage(sessionId: id, used: 0, total: 200_000))
        case ListRecentRoots.queryType:
            return try ok(corpus.workspaces)
        case GetAttachment.queryType:
            let id = request["attachmentId"]?.stringValue ?? ""
            guard let attachment = corpus.attachments[id] else {
                return error(code: "attachment_not_found", message: "no attachment \(id)")
            }
            return try ok(attachment)
        case SearchSessions.queryType, SearchInSession.queryType:
            let q = request["q"]?.stringValue ?? ""
            return try ok(MockTransport.hits(for: q, in: corpus))
        case ResolvePath.queryType:
            let path = request["path"]?.stringValue ?? ""
            return try ok(ResolvedPath(resolved: path, insideRoot: !path.hasPrefix("..")))
        case RenderMarkdown.queryType:
            // The real parser lives in Rust (W5); previews only need a block per paragraph.
            let text = request["text"]?.stringValue ?? ""
            return try ok(MockTransport.markdown(text))
        default:
            return error(code: "not_implemented", message: "mock has no answer for \(type)")
        }
    }

    // MARK: - Commands

    public func dispatch(_ json: String) throws -> String {
        let command = try JSONDecoder().decode(CoreCommand.self, from: Data(json.utf8))
        let sequence = lock.withLock { () -> Int in
            self.sequence += 1
            recorded.append(command)
            return self.sequence
        }
        let commandId = "cmd_mock_\(sequence)"

        switch command {
        case let .createSession(groupId, title, workspaceRoot, modelRef):
            let summary = SessionSummary(
                id: "ses_mock_\(sequence)", title: title ?? "New chat",
                titleIsCustom: title != nil, groupId: groupId, workspaceRoot: workspaceRoot,
                modelRef: modelRef
                    ?? ModelRef(
                        providerId: "anthropic", modelId: "claude-opus-5", thinkingLevel: .high))
            lock.withLock { corpus.sessions.insert(summary, at: 0) }
            emit(CoreEvent(commandId: commandId, kind: .sessionCreated(session: summary)))

        case let .sendPrompt(sessionId, text, _):
            guard replaysRuns else { break }
            let model =
                lock.withLock { corpus.sessions.first { $0.id == sessionId }?.modelRef }
                ?? ModelRef(
                    providerId: "anthropic", modelId: "claude-opus-5", thinkingLevel: .high)
            replay(
                MockCorpus.recordedRun(
                    sessionId: sessionId, prompt: text, model: model, commandId: commandId))

        case let .renameSession(sessionId, title):
            mutateSession(sessionId, commandId: commandId) {
                $0.title = title
                $0.titleIsCustom = true
            }

        case let .archiveSession(sessionId, archived):
            mutateSession(sessionId, commandId: commandId) { $0.archived = archived }

        case let .pinSession(sessionId, pinned):
            mutateSession(sessionId, commandId: commandId) { $0.pinned = pinned }

        case let .moveSession(sessionId, groupId, index):
            mutateSession(sessionId, commandId: commandId) {
                $0.groupId = groupId
                $0.index = index
            }

        case let .setSessionModel(sessionId, modelRef):
            mutateSession(sessionId, commandId: commandId) { $0.modelRef = modelRef }

        case let .setWorkspaceRoot(sessionId, path):
            mutateSession(sessionId, commandId: commandId) { $0.workspaceRoot = path }

        case let .deleteSession(sessionId):
            lock.withLock { corpus.sessions.removeAll { $0.id == sessionId } }
            emit(CoreEvent(commandId: commandId, kind: .sessionDeleted(sessionId: sessionId)))

        case let .createGroup(name):
            let group = SessionGroup(
                id: "grp_mock_\(sequence)", name: name,
                index: lock.withLock { corpus.groups.count })
            let groups: [SessionGroup] = lock.withLock {
                corpus.groups.append(group)
                return corpus.groups
            }
            emit(CoreEvent(commandId: commandId, kind: .groupsChanged(groups: groups)))

        case let .renameGroup(groupId, name):
            mutateGroups(commandId: commandId) { groups in
                groups.firstIndex { $0.id == groupId }.map { groups[$0].name = name }
            }

        case let .deleteGroup(groupId):
            mutateGroups(commandId: commandId) { $0.removeAll { $0.id == groupId } }

        case let .setGroupCollapsed(groupId, collapsed):
            mutateGroups(commandId: commandId) { groups in
                groups.firstIndex { $0.id == groupId }.map { groups[$0].collapsed = collapsed }
            }

        case let .reorderGroup(groupId, index):
            mutateGroups(commandId: commandId) { groups in
                groups.firstIndex { $0.id == groupId }.map { groups[$0].index = index }
            }

        case let .updateSettings(settings):
            lock.withLock { corpus.settings = settings }
            emit(CoreEvent(commandId: commandId, kind: .settingsChanged(settings: settings)))

        default:
            break
        }

        return try okString(CommandAck(commandId: commandId))
    }

    private func mutateSession(
        _ sessionId: String, commandId: String, _ body: (inout SessionSummary) -> Void
    ) {
        let updated: SessionSummary? = lock.withLock {
            guard let i = corpus.sessions.firstIndex(where: { $0.id == sessionId }) else {
                return nil
            }
            body(&corpus.sessions[i])
            corpus.sessions[i].updatedAt = Date.nowMs
            return corpus.sessions[i]
        }
        guard let updated else { return }
        emit(CoreEvent(commandId: commandId, kind: .sessionUpdated(session: updated)))
    }

    private func mutateGroups(commandId: String, _ body: (inout [SessionGroup]) -> Void) {
        let groups: [SessionGroup] = lock.withLock {
            body(&corpus.groups)
            return corpus.groups
        }
        emit(CoreEvent(commandId: commandId, kind: .groupsChanged(groups: groups)))
    }

    // MARK: - Events

    public func subscribe(_ handler: @escaping @Sendable (String) -> Void) throws -> Int32 {
        lock.withLock {
            guard !isShutDown else { return -1 }
            let token = nextToken
            nextToken += 1
            listeners[token] = handler
            return token
        }
    }

    public func unsubscribe(_ token: Int32) {
        lock.withLock { _ = listeners.removeValue(forKey: token) }
    }

    public func shutdown() {
        lock.withLock {
            isShutDown = true
            listeners.removeAll()
        }
    }

    /// Push an event by hand — how a test drives a store without a run.
    public func emit(_ event: CoreEvent) {
        guard let json = try? encoder.encode(event) else { return }
        let text = String(decoding: json, as: UTF8.self)
        queue.async { [weak self] in
            guard let self else { return }
            let handlers = self.lock.withLock { Array(self.listeners.values) }
            for handler in handlers { handler(text) }
        }
    }

    /// Replay a recorded log with its original cadence, on the event queue.
    public func replay(_ log: [RecordedEvent]) {
        queue.async { [weak self] in
            guard let self else { return }
            for recorded in log {
                if self.lock.withLock({ self.isShutDown }) { return }
                let delay = self.speed <= 0 ? 0 : Double(recorded.delayMs) / self.speed
                if delay >= 1 { Thread.sleep(forTimeInterval: delay / 1000) }
                guard let json = try? self.encoder.encode(recorded.event) else { continue }
                let text = String(decoding: json, as: UTF8.self)
                let handlers = self.lock.withLock { Array(self.listeners.values) }
                for handler in handlers { handler(text) }
            }
        }
    }

    // MARK: - Envelopes

    private struct Ok<T: Encodable>: Encodable {
        let ok = true
        let data: T
    }

    private func ok<T: Encodable>(_ value: T) throws -> String {
        try okString(value)
    }

    private func okString<T: Encodable>(_ value: T) throws -> String {
        String(decoding: try encoder.encode(Ok(data: value)), as: UTF8.self)
    }

    private func error(code: String, message: String) -> String {
        let escaped = message.replacingOccurrences(of: "\"", with: "'")
        return #"{"ok":false,"error":{"code":"\#(code)","message":"\#(escaped)"}}"#
    }

    // MARK: - Canned answers

    private static func hits(for query: String, in corpus: MockCorpus) -> [SearchHit] {
        let q = query.lowercased()
        guard !q.isEmpty else { return [] }
        return corpus.sessions
            .filter { $0.title.lowercased().contains(q) }
            .enumerated()
            .map { index, session in
                let snippet = "…\(session.title)…"
                let start = snippet.lowercased().range(of: q).map {
                    snippet.utf16.distance(from: snippet.startIndex, to: $0.lowerBound)
                }
                return SearchHit(
                    sessionId: session.id, title: session.title, snippet: snippet,
                    highlights: start.map { [HighlightRange(start: $0, len: q.utf16.count)] } ?? [],
                    score: 1.0 - Double(index) * 0.1, timestamp: session.updatedAt)
            }
    }

    private static func markdown(_ text: String) -> MarkdownDoc {
        let paragraphs = text.components(separatedBy: "\n\n").filter { !$0.isEmpty }
        return MarkdownDoc(
            blocks: paragraphs.enumerated().map { index, body in
                MarkdownBlock(
                    id: "b\(index)", kind: .paragraph(spans: [.text(text: body)]))
            })
    }
}
